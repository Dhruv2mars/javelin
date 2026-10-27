//! The node's SQLite databases on a provider's `Pages`, through a SQLite VFS named
//! `javelin`. `PageStorage` is the node core's `Storage` and `Coordination` for one
//! repository: the repository and the node's records are two databases in its pages, and its
//! documents are two plain files there.
//!
//! The databases use `PAGE`-sized pages, an in-memory rollback journal and exclusive locking.
//! A journal on disk is not needed: the Durable Object commits each request's page writes
//! together or not at all (`transactionSync` in the Worker), and SQLite rolls back a
//! refused transaction from its in-memory journal. A request that fails after a commit is
//! rolled back by the object, so the Worker drops the node and opens it again, empty of
//! cached pages. Exclusive locking keeps the page cache across transactions: one connection
//! per database is the only writer, which `xOpen` enforces.

use std::{
    any::Any,
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    ffi::{CStr, c_char, c_int},
    io,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use javelin::Repository;
use javelin_node::storage::{Coordination, Storage};
use rsqlite_vfs::{
    SQLiteIoMethods, SQLiteVfs, SQLiteVfsFile, VfsError, VfsFile, VfsResult, VfsStore, ffi,
    register_vfs, registered_vfs,
};
use rusqlite::{Connection, OpenFlags};

use crate::pages::{self, PAGE, Pages};

const VFS: &str = "javelin";
/// Page cache per database. The repository's is larger: compose and Stack read its trees.
const REPOSITORY_CACHE_KIB: i64 = 8 << 10;
const RECORDS_CACHE_KIB: i64 = 2 << 10;

/// One repository's storage on `pages`. Its files are named `<id>/<name>` in the VFS, so
/// several nodes in one isolate never share a file.
pub struct PageStorage {
    id: u64,
    pages: Rc<dyn Pages>,
}

impl PageStorage {
    pub fn new(pages: Rc<dyn Pages>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        STORES.with(|stores| stores.borrow_mut().insert(id, pages.clone()));
        Self { id, pages }
    }

    fn connect(&self, name: &str, cache_kib: i64) -> rusqlite::Result<Connection> {
        register();
        let conn = Connection::open_with_flags_and_vfs(
            format!("{}/{name}", self.id),
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            VFS,
        )?;
        conn.execute_batch(&format!(
            "PRAGMA page_size={PAGE}; PRAGMA journal_mode=MEMORY; PRAGMA locking_mode=EXCLUSIVE;
             PRAGMA synchronous=OFF; PRAGMA temp_store=MEMORY; PRAGMA cache_size=-{cache_kib};"
        ))?;
        Ok(conn)
    }

    fn document(name: &str) -> String {
        format!("doc/{name}")
    }
}

impl Drop for PageStorage {
    fn drop(&mut self) {
        STORES.with(|stores| stores.borrow_mut().remove(&self.id));
    }
}

impl Storage for PageStorage {
    fn repository(&self) -> javelin::Result<Repository> {
        Repository::init_on(self.connect("repository", REPOSITORY_CACHE_KIB)?)
    }

    /// A repository exists once its database holds a page. A first initialization that
    /// failed rolled back to an empty file.
    fn provisioned(&self) -> io::Result<bool> {
        let size = self.pages.size("repository").map_err(io::Error::other)?;
        Ok(size.is_some_and(|size| size > 0))
    }

    fn records(&self) -> rusqlite::Result<Connection> {
        self.connect("node.db", RECORDS_CACHE_KIB)
    }

    fn read(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        pages::read_all(&*self.pages, &Self::document(name)).map_err(io::Error::other)
    }

    fn write(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        pages::write_all(&*self.pages, &Self::document(name), bytes).map_err(io::Error::other)
    }
}

/// A Durable Object is the ordering point: the runtime runs one instance per repository and
/// hands it one request at a time, since the node never awaits inside a request.
impl Coordination for PageStorage {
    fn acquire(&self) -> javelin::Result<Box<dyn Any + Send>> {
        Ok(Box::new(()))
    }
}

thread_local! {
    /// Each registered repository's pages, by id.
    static STORES: RefCell<BTreeMap<u64, Rc<dyn Pages>>> = const { RefCell::new(BTreeMap::new()) };
    /// Every file SQLite holds open, with its size.
    static OPEN: RefCell<BTreeMap<String, PageFile>> = const { RefCell::new(BTreeMap::new()) };
}

fn register() {
    if matches!(registered_vfs(VFS), Ok(Some(_))) {
        return;
    }
    register_vfs::<PageIo, PageVfs>(VFS, (), false).expect("register the javelin VFS");
}

/// `<id>/<name>` as the repository's pages and the file's name in them.
fn locate(path: &str) -> VfsResult<(Rc<dyn Pages>, String)> {
    let unknown = || VfsError::new(ffi::SQLITE_CANTOPEN, format!("no storage for {path}"));
    let (id, name) = path.split_once('/').ok_or_else(unknown)?;
    let id: u64 = id.parse().map_err(|_| unknown())?;
    let pages = STORES
        .with(|stores| stores.borrow().get(&id).cloned())
        .ok_or_else(unknown)?;
    Ok((pages, name.to_string()))
}

fn io_error(detail: String) -> VfsError {
    VfsError::new(ffi::SQLITE_IOERR, detail)
}

/// One open file: its pages, its name there and its size, which only this connection changes.
pub struct PageFile {
    pages: Rc<dyn Pages>,
    name: String,
    size: u64,
}

impl VfsFile for PageFile {
    fn read(&self, buf: &mut [u8], offset: usize) -> VfsResult<bool> {
        pages::read(&*self.pages, &self.name, self.size, buf, offset as u64).map_err(io_error)
    }

    fn write(&mut self, buf: &[u8], offset: usize) -> VfsResult<()> {
        self.size = pages::write(&*self.pages, &self.name, self.size, buf, offset as u64)
            .map_err(io_error)?;
        Ok(())
    }

    fn truncate(&mut self, size: usize) -> VfsResult<()> {
        pages::truncate(&*self.pages, &self.name, size as u64).map_err(io_error)?;
        self.size = size as u64;
        Ok(())
    }

    fn flush(&mut self) -> VfsResult<()> {
        Ok(())
    }

    fn size(&self) -> VfsResult<usize> {
        Ok(self.size as usize)
    }
}

struct PageStore;

impl VfsStore<PageFile, ()> for PageStore {
    fn add_file(_: *mut ffi::sqlite3_vfs, file: &str, _: i32) -> VfsResult<()> {
        let (pages, name) = locate(file)?;
        pages.set_size(&name, 0).map_err(io_error)
    }

    fn contains_file(_: *mut ffi::sqlite3_vfs, file: &str) -> VfsResult<bool> {
        let (pages, name) = locate(file)?;
        Ok(pages.size(&name).map_err(io_error)?.is_some())
    }

    fn delete_file(_: *mut ffi::sqlite3_vfs, file: &str) -> VfsResult<()> {
        let (pages, name) = locate(file)?;
        pages.remove(&name).map_err(io_error)
    }

    fn with_file<F: Fn(&PageFile) -> VfsResult<i32>>(
        vfs_file: &SQLiteVfsFile,
        f: F,
    ) -> VfsResult<i32> {
        let path = unsafe { vfs_file.name() };
        OPEN.with(|open| match open.borrow().get(&*path) {
            Some(file) => f(file),
            None => Err(io_error(format!("{path} is not open"))),
        })
    }

    fn with_file_mut<F: Fn(&mut PageFile) -> VfsResult<i32>>(
        vfs_file: &SQLiteVfsFile,
        f: F,
    ) -> VfsResult<i32> {
        let path = unsafe { vfs_file.name() };
        OPEN.with(|open| match open.borrow_mut().get_mut(&*path) {
            Some(file) => f(file),
            None => Err(io_error(format!("{path} is not open"))),
        })
    }
}

struct PageIo;

impl SQLiteIoMethods for PageIo {
    type File = PageFile;
    type AppData = ();
    type Store = PageStore;

    const VERSION: c_int = 1;

    unsafe extern "C" fn xClose(file: *mut ffi::sqlite3_file) -> c_int {
        let path = unsafe { SQLiteVfsFile::from_file(file).name() }.to_string();
        OPEN.with(|open| open.borrow_mut().remove(&path));
        unsafe { Self::xCloseImpl(file) }
    }
}

struct PageVfs;

impl SQLiteVfs<PageIo> for PageVfs {
    const VERSION: c_int = 1;

    /// Opens a file once: a second connection to an open database would read pages the
    /// first one has cached and not yet written, so it is refused as busy.
    unsafe extern "C" fn xOpen(
        vfs: *mut ffi::sqlite3_vfs,
        name: ffi::sqlite3_filename,
        file: *mut ffi::sqlite3_file,
        flags: c_int,
        out_flags: *mut c_int,
    ) -> c_int {
        if name.is_null() {
            return ffi::SQLITE_CANTOPEN;
        }
        let Ok(path) = unsafe { CStr::from_ptr(name as *const c_char) }.to_str() else {
            return ffi::SQLITE_CANTOPEN;
        };
        let path = path.to_string();
        if OPEN.with(|open| open.borrow().contains_key(&path)) {
            return ffi::SQLITE_BUSY;
        }
        let code = unsafe { Self::xOpenImpl(vfs, name, file, flags, out_flags) };
        if code != ffi::SQLITE_OK {
            return code;
        }
        let Ok((pages, inner)) = locate(&path) else {
            return ffi::SQLITE_CANTOPEN;
        };
        let size = match pages.size(&inner) {
            Ok(size) => size.unwrap_or(0),
            Err(_) => return ffi::SQLITE_IOERR,
        };
        OPEN.with(|open| {
            open.borrow_mut().insert(
                path,
                PageFile {
                    pages,
                    name: inner,
                    size,
                },
            )
        });
        ffi::SQLITE_OK
    }

    fn sleep(_: Duration) {}

    fn random(buf: &mut [u8]) {
        getrandom::fill(buf).expect("OS randomness");
    }

    fn epoch_timestamp_in_ms() -> i64 {
        #[cfg(target_arch = "wasm32")]
        {
            js_sys::Date::now() as i64
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_millis() as i64)
        }
    }
}

/// The names of the files SQLite holds open, for tests.
pub fn open_files() -> BTreeSet<String> {
    OPEN.with(|open| open.borrow().keys().cloned().collect())
}
