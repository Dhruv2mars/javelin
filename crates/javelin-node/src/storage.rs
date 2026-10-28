//! What the node core needs from the provider it runs on (`docs/direction.md`, "What a node
//! needs"): durable storage for the repository and the node's records, and one ordering point
//! per repository. The core reaches both only through these traits, so the same code serves a
//! self-hosted node from a data directory (`Directory`, with the `native` feature) and a
//! Cloudflare Durable Object from its SQLite storage (`crates/javelin-node-cloudflare`).

use std::{any::Any, io};

use javelin::Repository;
use rusqlite::Connection;

/// The node's documents in storage: the applied permissions and the keys it trusts.
pub const PERMISSIONS: &str = "permissions.json";
pub const KEYS: &str = "keys.json";

/// Durable storage for one repository's node. An acknowledged write survives a restart.
pub trait Storage {
    /// The repository store: World log, objects, Layers and review state. Creates World 0 on
    /// first use and validates an existing store.
    fn repository(&self) -> javelin::Result<Repository>;
    /// Whether the repository store exists, without creating it.
    fn provisioned(&self) -> io::Result<bool>;
    /// A connection to the node's own records (`node.db`): decided operations, uploads,
    /// review, checks and traces. The core opens it once per node.
    fn records(&self) -> rusqlite::Result<Connection>;
    /// A named document, or `None` when it was never written.
    fn read(&self, name: &str) -> io::Result<Option<Vec<u8>>>;
    /// Replaces a named document; a crash leaves the old or the new content.
    fn write(&self, name: &str, bytes: &[u8]) -> io::Result<()>;
}

/// The one ordering point per repository: every World advance goes through it, one at a
/// time. A node holds the guard for its whole life, and a second node on the same storage is
/// refused with `Busy`.
pub trait Coordination {
    fn acquire(&self) -> javelin::Result<Box<dyn Any + Send>>;
}

#[cfg(feature = "native")]
pub use native::Directory;

#[cfg(feature = "native")]
mod native {
    use std::{
        fs::{self, OpenOptions, TryLockError},
        io::{self, ErrorKind, Write},
        path::{Path, PathBuf},
    };

    use javelin::{Error, Repository};
    use rusqlite::Connection;

    use super::{Coordination, Storage};

    /// Held for the node's lifetime. A second node on the same directory is refused.
    const LOCK: &str = "node.lock";
    /// The node's records, beside the repository's `.javelin`.
    const RECORDS: &str = "node.db";

    /// A self-hosted node's data directory: the repository in `.javelin`, the records in
    /// `node.db`, documents as files, and an exclusive process lock on `node.lock`. Local
    /// fsync, no replica.
    pub struct Directory {
        dir: PathBuf,
    }

    impl Directory {
        pub fn new(dir: impl AsRef<Path>) -> Self {
            Self {
                dir: dir.as_ref().to_path_buf(),
            }
        }
    }

    impl Storage for Directory {
        /// Creates the directory on first use. The lock and records sit beside `.javelin`, so
        /// the repository's directory stays as it is.
        fn repository(&self) -> javelin::Result<Repository> {
            fs::create_dir_all(&self.dir)?;
            Repository::init(&self.dir)
        }

        fn provisioned(&self) -> io::Result<bool> {
            Ok(Repository::open(&self.dir).is_ok())
        }

        fn records(&self) -> rusqlite::Result<Connection> {
            Connection::open(self.dir.join(RECORDS))
        }

        fn read(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
            match fs::read(self.dir.join(name)) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            }
        }

        /// Writes through a temporary file, so a crash leaves the old or the new content. The
        /// file is private to its owner on Unix.
        fn write(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
            let path = self.dir.join(name);
            let tmp = path.with_extension("tmp");
            let _ = fs::remove_file(&tmp);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(tmp, path)
        }
    }

    impl Coordination for Directory {
        fn acquire(&self) -> javelin::Result<Box<dyn std::any::Any + Send>> {
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(self.dir.join(LOCK))?;
            match lock.try_lock() {
                Ok(()) => Ok(Box::new(lock)),
                Err(TryLockError::WouldBlock) => Err(Error::Busy(format!(
                    "node data directory {} is held by another node",
                    self.dir.display()
                ))),
                Err(TryLockError::Error(e)) => Err(e.into()),
            }
        }
    }
}
