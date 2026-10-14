use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use macos_sparse_backend::{CRASH_AFTER_STAGE_ENV, Overlay, clone_upper};
use nfsserve::nfs::{fileid3, nfsstat3, sattr3};
use nfsserve::vfs::NFSFileSystem;

static NEXT: AtomicU32 = AtomicU32::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "javelin-macos-overlay-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&root);
        let base = root.join("base");
        fs::create_dir_all(base.join("dir")).unwrap();
        fs::write(base.join("README.txt"), "base text\n").unwrap();
        fs::write(base.join("keep.txt"), "unchanged\n").unwrap();
        fs::write(base.join("dir/nested.txt"), "nested\n").unwrap();
        symlink("README.txt", base.join("inside-link")).unwrap();
        Fixture { root }
    }
    fn base(&self) -> PathBuf {
        self.root.join("base")
    }
    fn upper(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
    fn open(&self, name: &str) -> Overlay {
        Overlay::open(&self.base(), &self.upper(name)).unwrap()
    }
    fn base_listing(&self) -> Vec<(PathBuf, Vec<u8>)> {
        listing(&self.base())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn listing(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let m = fs::symlink_metadata(&p).unwrap();
            let rel = p.strip_prefix(root).unwrap().to_path_buf();
            if m.file_type().is_symlink() {
                out.push((
                    rel,
                    fs::read_link(&p)
                        .unwrap()
                        .into_os_string()
                        .into_encoded_bytes(),
                ));
            } else if m.is_dir() {
                out.push((rel, b"<dir>".to_vec()));
                walk(root, &p, out);
            } else {
                out.push((rel, fs::read(&p).unwrap()));
            }
        }
    }
    walk(root, root, &mut out);
    out.sort();
    out
}

async fn id(fs: &Overlay, path: &str) -> Result<fileid3, nfsstat3> {
    fs.path_to_id(path.as_bytes()).await
}

async fn read(fs: &Overlay, path: &str) -> Vec<u8> {
    let id = id(fs, path).await.unwrap();
    fs.read(id, 0, 1 << 20).await.unwrap().0
}

async fn names(fs: &Overlay, path: &str) -> Vec<String> {
    let dir = id(fs, path).await.unwrap();
    let r = fs.readdir(dir, 0, 100).await.unwrap();
    assert!(r.end);
    r.entries
        .iter()
        .map(|e| String::from_utf8(e.name.0.clone()).unwrap())
        .collect()
}

async fn write_new(fs: &Overlay, dir: &str, name: &str, body: &[u8]) {
    let dir = id(fs, dir).await.unwrap();
    let (fid, _) = fs
        .create(dir, &name.as_bytes().into(), sattr3::default())
        .await
        .unwrap();
    fs.write(fid, 0, body).await.unwrap();
}

#[tokio::test]
async fn write_copies_up_only_the_changed_file() {
    let fx = Fixture::new();
    let before = fx.base_listing();
    let a = fx.open("a");
    assert_eq!(read(&a, "README.txt").await, b"base text\n");
    let fid = id(&a, "README.txt").await.unwrap();
    a.write(fid, 0, b"BASE").await.unwrap();
    assert_eq!(read(&a, "README.txt").await, b"BASE text\n");
    assert_eq!(fx.base_listing(), before, "base must stay immutable");
    let private = listing(&fx.upper("a").join("tree"));
    assert_eq!(
        private,
        vec![(PathBuf::from("README.txt"), b"BASE text\n".to_vec())]
    );
    assert_eq!(read(&a, "keep.txt").await, b"unchanged\n");
}

#[tokio::test]
async fn layers_over_one_base_are_isolated() {
    let fx = Fixture::new();
    let a = fx.open("a");
    let b = fx.open("b");
    write_new(&a, "", "only-a.txt", b"A").await;
    let fid = id(&a, "keep.txt").await.unwrap();
    a.write(fid, 0, b"CHANGED").await.unwrap();
    assert_eq!(
        id(&b, "only-a.txt").await.unwrap_err() as u32,
        nfsstat3::NFS3ERR_NOENT as u32
    );
    assert_eq!(read(&b, "keep.txt").await, b"unchanged\n");
    assert_eq!(
        names(&b, "").await,
        ["README.txt", "dir", "inside-link", "keep.txt"]
    );
}

#[tokio::test]
async fn atomic_save_rename_and_delete_persist_across_reopen() {
    let fx = Fixture::new();
    let before = fx.base_listing();
    {
        let a = fx.open("a");
        write_new(&a, "", ".README.saving", b"saved\n").await;
        let root = a.root_dir();
        a.rename(
            root,
            &b".README.saving".as_slice().into(),
            root,
            &b"README.txt".as_slice().into(),
        )
        .await
        .unwrap();
        a.remove(root, &b"keep.txt".as_slice().into())
            .await
            .unwrap();
        assert_eq!(names(&a, "").await, ["README.txt", "dir", "inside-link"]);
    }
    let a = fx.open("a");
    assert_eq!(read(&a, "README.txt").await, b"saved\n");
    assert!(id(&a, "keep.txt").await.is_err());
    assert_eq!(names(&a, "").await, ["README.txt", "dir", "inside-link"]);
    assert_eq!(fx.base_listing(), before);
}

#[tokio::test]
async fn directory_rename_moves_base_children_and_hides_old_name() {
    let fx = Fixture::new();
    let before = fx.base_listing();
    let a = fx.open("a");
    let root = a.root_dir();
    a.rename(
        root,
        &b"dir".as_slice().into(),
        root,
        &b"renamed".as_slice().into(),
    )
    .await
    .unwrap();
    assert_eq!(read(&a, "renamed/nested.txt").await, b"nested\n");
    assert!(id(&a, "dir").await.is_err());
    a.rename(
        root,
        &b"renamed".as_slice().into(),
        root,
        &b"dir".as_slice().into(),
    )
    .await
    .unwrap();
    assert_eq!(read(&a, "dir/nested.txt").await, b"nested\n");
    assert!(id(&a, "renamed").await.is_err());
    assert_eq!(fx.base_listing(), before);
}

#[tokio::test]
async fn recreated_directory_does_not_resurrect_deleted_base_children() {
    let fx = Fixture::new();
    let a = fx.open("a");
    let root = a.root_dir();
    let dir = id(&a, "dir").await.unwrap();
    assert_eq!(
        a.remove(root, &b"dir".as_slice().into()).await.unwrap_err() as u32,
        nfsstat3::NFS3ERR_NOTEMPTY as u32
    );
    a.remove(dir, &b"nested.txt".as_slice().into())
        .await
        .unwrap();
    a.remove(root, &b"dir".as_slice().into()).await.unwrap();
    a.mkdir(root, &b"dir".as_slice().into()).await.unwrap();
    assert!(names(&a, "dir").await.is_empty());
}

#[tokio::test]
async fn symlinks_resolve_from_base_and_private_links_stay_private() {
    let fx = Fixture::new();
    let a = fx.open("a");
    let link = id(&a, "inside-link").await.unwrap();
    assert_eq!(a.readlink(link).await.unwrap().0, b"README.txt");
    let root = a.root_dir();
    a.symlink(
        root,
        &b"new-link".as_slice().into(),
        &b"keep.txt".as_slice().into(),
        &sattr3::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read_link(fx.upper("a").join("tree/new-link")).unwrap(),
        PathBuf::from("keep.txt")
    );
    assert!(id(&fx.open("b"), "new-link").await.is_err());
}

#[tokio::test]
async fn quiescent_clone_copies_only_private_state_and_diverges() {
    let fx = Fixture::new();
    {
        let a = fx.open("a");
        write_new(&a, "", "private.txt", b"A1").await;
        a.remove(a.root_dir(), &b"keep.txt".as_slice().into())
            .await
            .unwrap();
    }
    clone_upper(&fx.upper("a"), &fx.upper("c")).unwrap();
    let a = fx.open("a");
    let c = fx.open("c");
    assert_eq!(names(&c, "").await, names(&a, "").await);
    assert_eq!(
        listing(&fx.upper("c").join("tree")),
        vec![(PathBuf::from("private.txt"), b"A1".to_vec())]
    );
    let fid = id(&c, "private.txt").await.unwrap();
    c.write(fid, 0, b"C2").await.unwrap();
    assert_eq!(read(&a, "private.txt").await, b"A1");
    assert_eq!(read(&c, "private.txt").await, b"C2");
}

#[tokio::test]
async fn readdir_pages_resume_after_cookie() {
    let fx = Fixture::new();
    let a = fx.open("a");
    let root = a.root_dir();
    let first = a.readdir(root, 0, 2).await.unwrap();
    assert!(!first.end);
    let rest = a.readdir(root, first.entries[1].fileid, 10).await.unwrap();
    let all: Vec<_> = first
        .entries
        .iter()
        .chain(&rest.entries)
        .map(|e| e.name.0.clone())
        .collect();
    assert_eq!(
        all,
        [&b"README.txt"[..], b"dir", b"inside-link", b"keep.txt"]
    );
    assert!(rest.end);
}

/// Child entry point: aborts inside copy-up after staging, before publishing.
#[tokio::test]
#[ignore]
async fn child_crash_during_copy_up() {
    let base = PathBuf::from(std::env::var("JAVELIN_TEST_BASE").unwrap());
    let upper = PathBuf::from(std::env::var("JAVELIN_TEST_UPPER").unwrap());
    let a = Overlay::open(&base, &upper).unwrap();
    let fid = id(&a, "README.txt").await.unwrap();
    let _ = a.write(fid, 0, b"LOST").await;
    unreachable!("copy-up should have aborted");
}

#[tokio::test]
async fn interrupted_copy_up_leaves_base_bytes_and_is_cleaned_on_reopen() {
    let fx = Fixture::new();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "child_crash_during_copy_up",
            "--nocapture",
        ])
        .env(CRASH_AFTER_STAGE_ENV, "1")
        .env("JAVELIN_TEST_BASE", fx.base())
        .env("JAVELIN_TEST_UPPER", fx.upper("a"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success(), "child must be killed by abort");
    let staged = fs::read_dir(fx.upper("a").join("tmp")).unwrap().count();
    assert_eq!(staged, 1, "abort must happen with one staged copy");
    assert!(!fx.upper("a").join("tree/README.txt").exists());
    let a = fx.open("a");
    assert_eq!(read(&a, "README.txt").await, b"base text\n");
    assert_eq!(fs::read_dir(fx.upper("a").join("tmp")).unwrap().count(), 0);
}

#[test]
fn clone_refuses_a_layer_that_is_being_served() {
    let fx = Fixture::new();
    let bin = env!("CARGO_BIN_EXE_macos-sparse-backend");
    let mut server = Command::new(bin)
        .args(["serve", "--base"])
        .arg(fx.base())
        .arg("--upper")
        .arg(fx.upper("a"))
        .args(["--port", "0"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(server.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.starts_with("listening 127.0.0.1:"), "{line}");
    let out = Command::new(bin)
        .args(["clone", "--from"])
        .arg(fx.upper("a"))
        .arg("--to")
        .arg(fx.upper("c"))
        .output()
        .unwrap();
    server.kill().unwrap();
    server.wait().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("clone refused"));
    assert!(!fx.upper("c").exists());
    let out = Command::new(bin)
        .args(["clone", "--from"])
        .arg(fx.upper("a"))
        .arg("--to")
        .arg(fx.upper("c"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
