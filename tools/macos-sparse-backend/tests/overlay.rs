use std::fs::{self, FileTimes};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

use macos_sparse_backend::{CRASH_AT_ENV, Overlay, clone_upper};
use nfsserve::nfs::{
    fileid3, nfsstat3, nfstime3, sattr3, set_atime, set_mode3, set_mtime, set_size3,
};
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
async fn writable_base_files_copy_up_beneath_readonly_directories() {
    let fx = Fixture::new();
    let dir = fx.base().join("dir");
    fs::create_dir(dir.join("nested")).unwrap();
    fs::write(dir.join("nested/editable"), b"base bytes").unwrap();
    fs::set_permissions(
        dir.join("nested/editable"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    fs::write(dir.join("nested/blocked"), b"unreadable").unwrap();
    fs::set_permissions(
        dir.join("nested/blocked"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    for path in [&dir, &dir.join("nested")] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o555)).unwrap();
    }
    let a = fx.open("a");
    let fid = id(&a, "dir/nested/editable").await.unwrap();
    let written = a.write(fid, 0, b"EDIT").await;
    let blocked = a
        .write(id(&a, "dir/nested/blocked").await.unwrap(), 0, b"X")
        .await;
    // Restore base access before assertions so fixture cleanup works on failure.
    for path in [&dir, &dir.join("nested")] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::set_permissions(
        dir.join("nested/blocked"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    written.unwrap();
    assert_eq!(a.getattr(fid).await.unwrap().mode, 0o644);
    assert_eq!(blocked.unwrap_err() as u32, nfsstat3::NFS3ERR_ACCES as u32);
    assert_eq!(read(&a, "dir/nested/editable").await, b"EDIT bytes");
    assert_eq!(
        fs::read(dir.join("nested/editable")).unwrap(),
        b"base bytes"
    );
    let nested = id(&a, "dir/nested").await.unwrap();
    assert_eq!(
        a.create(nested, &b"new-file".as_slice().into(), sattr3::default())
            .await
            .unwrap_err() as u32,
        nfsstat3::NFS3ERR_ACCES as u32,
        "copy-up access must not permit creating new entries in a read-only directory"
    );
    for path in ["dir", "dir/nested"] {
        let attr = a.getattr(id(&a, path).await.unwrap()).await.unwrap();
        assert_eq!(attr.mode, 0o555, "reported mode: {path}");
        assert_eq!(
            fs::metadata(fx.upper("a").join("tree").join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o555
        );
    }
    let reopened = fx.open("a");
    assert_eq!(
        reopened
            .getattr(id(&reopened, "dir/nested").await.unwrap())
            .await
            .unwrap()
            .mode,
        0o555
    );
    for path in ["dir", "dir/nested"] {
        fs::set_permissions(
            fx.upper("a").join("tree").join(path),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
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
async fn renamed_file_handle_and_name_stay_valid_without_reopen() {
    let fx = Fixture::new();
    let a = fx.open("a");
    let root = a.root_dir();
    write_new(&a, "", ".README.saving", b"saved\n").await;
    let moved = id(&a, ".README.saving").await.unwrap();
    let replaced = id(&a, "README.txt").await.unwrap();
    a.rename(
        root,
        &b".README.saving".as_slice().into(),
        root,
        &b"README.txt".as_slice().into(),
    )
    .await
    .unwrap();
    assert_eq!(
        a.getattr(moved).await.unwrap().size,
        6,
        "moved handle must follow the file"
    );
    assert_eq!(a.read(moved, 0, 100).await.unwrap().0, b"saved\n");
    assert_eq!(id(&a, "README.txt").await.unwrap(), moved);
    assert_eq!(read(&a, "README.txt").await, b"saved\n");
    assert_eq!(
        a.getattr(replaced).await.unwrap_err() as u32,
        nfsstat3::NFS3ERR_STALE as u32
    );
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
        .env(CRASH_AT_ENV, "stage")
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

/// Child entry point: aborts after a rename moved private bytes, before whiteouts persist.
#[tokio::test]
#[ignore]
async fn child_crash_during_rename() {
    let base = PathBuf::from(std::env::var("JAVELIN_TEST_BASE").unwrap());
    let upper = PathBuf::from(std::env::var("JAVELIN_TEST_UPPER").unwrap());
    let a = Overlay::open(&base, &upper).unwrap();
    write_new(&a, "", ".README.saving", b"saved\n").await;
    let root = a.root_dir();
    let _ = a
        .rename(
            root,
            &b".README.saving".as_slice().into(),
            root,
            &b"README.txt".as_slice().into(),
        )
        .await;
    unreachable!("rename should have aborted");
}

#[tokio::test]
async fn interrupted_rename_never_loses_the_destination() {
    let fx = Fixture::new();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "child_crash_during_rename",
            "--nocapture",
        ])
        .env(CRASH_AT_ENV, "rename")
        .env("JAVELIN_TEST_BASE", fx.base())
        .env("JAVELIN_TEST_UPPER", fx.upper("a"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success(), "child must be killed by abort");
    let a = fx.open("a");
    assert_eq!(read(&a, "README.txt").await, b"saved\n");
    assert!(id(&a, ".README.saving").await.is_err());
}

#[tokio::test]
async fn unchecked_create_of_existing_file_keeps_bytes() {
    let fx = Fixture::new();
    let a = fx.open("a");
    let root = a.root_dir();
    a.create(root, &b"README.txt".as_slice().into(), sattr3::default())
        .await
        .unwrap();
    assert_eq!(read(&a, "README.txt").await, b"base text\n");
    let truncate = sattr3 {
        size: nfsserve::nfs::set_size3::size(0),
        ..Default::default()
    };
    a.create(root, &b"keep.txt".as_slice().into(), truncate)
        .await
        .unwrap();
    assert_eq!(read(&a, "keep.txt").await, b"");
}

#[tokio::test]
async fn data_operations_refuse_symlink_handles_and_base_stays_unchanged() {
    let fx = Fixture::new();
    let before = fx.base_listing();
    let a = fx.open("a");
    let target = fx.base().join("keep.txt");
    let (link, _) = a
        .symlink(
            a.root_dir(),
            &b"escape".as_slice().into(),
            &target.as_os_str().as_encoded_bytes().into(),
            &sattr3::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        a.write(link, 0, b"X").await.unwrap_err() as u32,
        nfsstat3::NFS3ERR_INVAL as u32
    );
    assert_eq!(
        a.read(link, 0, 10).await.unwrap_err() as u32,
        nfsstat3::NFS3ERR_INVAL as u32
    );
    let inside = id(&a, "inside-link").await.unwrap();
    assert!(a.write(inside, 0, b"X").await.is_err());
    assert_eq!(fx.base_listing(), before);
}

#[tokio::test]
async fn failed_whiteout_persistence_keeps_base_entry_visible() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let a = fx.open("a");
    let tmp = fx.upper("a").join("tmp");
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o555)).unwrap();
    let result = a.remove(a.root_dir(), &b"keep.txt".as_slice().into()).await;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        result.is_err(),
        "remove must report the persistence failure"
    );
    assert_eq!(read(&a, "keep.txt").await, b"unchanged\n");
    a.remove(a.root_dir(), &b"keep.txt".as_slice().into())
        .await
        .unwrap();
    assert!(
        id(&fx.open("a"), "keep.txt").await.is_err(),
        "retry must persist the whiteout"
    );
}

#[tokio::test]
async fn getattr_reports_actual_mode_bits() {
    let fx = Fixture::new();
    let a = fx.open("a");
    let fid = id(&a, "README.txt").await.unwrap();
    let mode = sattr3 {
        mode: nfsserve::nfs::set_mode3::mode(0o444),
        ..Default::default()
    };
    assert_eq!(a.setattr(fid, mode).await.unwrap().mode, 0o444);
    assert_eq!(a.getattr(fid).await.unwrap().mode, 0o444);
}

#[tokio::test]
async fn readonly_create_applies_size_and_times_before_restricting_mode() {
    let fx = Fixture::new();
    let a = fx.open("a");
    let time = nfstime3 {
        seconds: 123456789,
        nseconds: 123456789,
    };
    let (_, attr) = a
        .create(
            a.root_dir(),
            &b"readonly".as_slice().into(),
            sattr3 {
                mode: set_mode3::mode(0o444),
                size: set_size3::size(0),
                mtime: set_mtime::SET_TO_CLIENT_TIME(time),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(attr.mode, 0o444);
    assert_eq!(attr.size, 0);
    assert_eq!(
        (attr.mtime.seconds, attr.mtime.nseconds),
        (time.seconds, time.nseconds)
    );
}

#[tokio::test]
async fn owner_can_change_times_on_mode_zero_file() {
    let fx = Fixture::new();
    let a = fx.open("a");
    let (fid, _) = a
        .create(
            a.root_dir(),
            &b"no-access".as_slice().into(),
            sattr3 {
                mode: set_mode3::mode(0),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let atime = nfstime3 {
        seconds: 123456789,
        nseconds: 123456789,
    };
    let mtime = nfstime3 {
        seconds: 234567890,
        nseconds: 234567890,
    };
    let result = a
        .setattr(
            fid,
            sattr3 {
                atime: set_atime::SET_TO_CLIENT_TIME(atime),
                mtime: set_mtime::SET_TO_CLIENT_TIME(mtime),
                ..Default::default()
            },
        )
        .await;
    fs::set_permissions(
        fx.upper("a").join("tree/no-access"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let attr = result.unwrap();
    assert_eq!(attr.mode, 0);
    assert_eq!(
        (attr.atime.seconds, attr.atime.nseconds),
        (atime.seconds, atime.nseconds)
    );
    assert_eq!(
        (attr.mtime.seconds, attr.mtime.nseconds),
        (mtime.seconds, mtime.nseconds)
    );
}

#[tokio::test]
async fn mode_only_copy_up_preserves_mtime() {
    let fx = Fixture::new();
    let path = fx.base().join("README.txt");
    fs::File::open(&path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::new(123456789, 123456789)))
        .unwrap();
    let before = fs::metadata(&path).unwrap().modified().unwrap();
    let a = fx.open("a");
    let fid = id(&a, "README.txt").await.unwrap();
    a.setattr(
        fid,
        sattr3 {
            mode: set_mode3::mode(0o444),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        fs::metadata(fx.upper("a").join("tree/README.txt"))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
    assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), before);
}

#[tokio::test]
async fn clone_preserves_private_entry_times() {
    let fx = Fixture::new();
    let a = fx.open("a");
    a.mkdir(a.root_dir(), &b"private".as_slice().into())
        .await
        .unwrap();
    write_new(&a, "private", "file", b"private bytes").await;
    drop(a);
    let tree = fx.upper("a").join("tree");
    symlink("file", tree.join("private/link")).unwrap();
    let paths = ["private/file", "private/link", "private", ""];
    let time = UNIX_EPOCH + Duration::new(123456789, 123456789);
    for p in paths {
        filetime::set_symlink_file_times(tree.join(p), time.into(), time.into()).unwrap();
    }
    clone_upper(&fx.upper("a"), &fx.upper("c")).unwrap();
    for p in paths {
        let meta = fs::symlink_metadata(fx.upper("c").join("tree").join(p)).unwrap();
        assert_eq!(meta.modified().unwrap(), time, "mtime: {p}");
        assert_eq!(meta.accessed().unwrap(), time, "atime: {p}");
    }
}

#[tokio::test]
async fn directory_and_symlink_copy_up_preserve_source_times() {
    let fx = Fixture::new();
    let paths = ["dir/nested.txt", "dir", "inside-link"];
    let time = UNIX_EPOCH + Duration::new(123456789, 123456789);
    for p in paths {
        filetime::set_symlink_file_times(fx.base().join(p), time.into(), time.into()).unwrap();
    }
    let target_time = fs::metadata(fx.base().join("README.txt"))
        .unwrap()
        .modified()
        .unwrap();
    let a = fx.open("a");
    let root = a.root_dir();
    for (from, to) in [("dir", "moved"), ("inside-link", "moved-link")] {
        a.rename(root, &from.as_bytes().into(), root, &to.as_bytes().into())
            .await
            .unwrap();
    }
    for p in ["moved/nested.txt", "moved", "moved-link"] {
        let meta = fs::symlink_metadata(fx.upper("a").join("tree").join(p)).unwrap();
        assert_eq!(meta.modified().unwrap(), time, "mtime: {p}");
        assert_eq!(meta.accessed().unwrap(), time, "atime: {p}");
    }
    assert_eq!(
        fs::metadata(fx.base().join("README.txt"))
            .unwrap()
            .modified()
            .unwrap(),
        target_time
    );
}

#[test]
fn incomplete_clone_stays_unpublished_and_rollback_keeps_competing_layer() {
    let fx = Fixture::new();
    drop(fx.open("a"));
    let fifo = fx.upper("a").join("tree/blocked");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let bin = env!("CARGO_BIN_EXE_macos-sparse-backend");
    let mut clone = Command::new(bin)
        .args(["clone", "--from"])
        .arg(fx.upper("a"))
        .arg("--to")
        .arg(fx.upper("c"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut copying = false;
    while Instant::now() < deadline && clone.try_wait().unwrap().is_none() {
        copying = fs::read_dir(&fx.root).unwrap().any(|e| {
            let p = e.unwrap().path();
            p != fx.base() && p != fx.upper("a") && p.join("tree").is_dir()
        });
        if copying {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let exposed = fx.upper("c").exists();
    let mut server = Command::new(bin)
        .args(["serve", "--base"])
        .arg(fx.base())
        .arg("--upper")
        .arg(fx.upper("c"))
        .args(["--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(server.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let ready = line.starts_with("listening 127.0.0.1:");
    let sentinel = fx.upper("c").join("tree/competitor");
    if ready {
        fs::write(&sentinel, b"must survive clone rollback").unwrap();
    }
    // Opening the FIFO writer releases the clone's blocked reader. fs::copy then
    // rejects the non-regular source, so this also exercises failed-clone cleanup.
    let release = Command::new("python3")
        .args([
            "-I",
            "-c",
            r#"import os, sys, time
for _ in range(200):
    try:
        fd = os.open(sys.argv[1], os.O_WRONLY | os.O_NONBLOCK)
        os.close(fd)
        break
    except OSError:
        time.sleep(.01)
else:
    sys.exit(1)
"#,
        ])
        .arg(&fifo)
        .status()
        .unwrap();
    if !release.success() {
        let _ = clone.kill();
    }
    let status = clone.wait().unwrap();
    let _ = server.kill();
    server.wait().unwrap();
    assert!(copying, "clone must reach the copy phase");
    assert!(
        !exposed,
        "incomplete clone was published before copying finished"
    );
    assert!(
        ready,
        "a competing new Layer should start while the clone is staged"
    );
    assert!(
        release.success(),
        "clone must be blocked on its source FIFO"
    );
    assert!(!status.success());
    assert_eq!(fs::read(sentinel).unwrap(), b"must survive clone rollback");
}

#[tokio::test]
async fn directory_rename_replaces_an_empty_directory() {
    let fx = Fixture::new();
    let a = fx.open("a");
    let root = a.root_dir();
    a.mkdir(root, &b"empty".as_slice().into()).await.unwrap();
    a.rename(
        root,
        &b"dir".as_slice().into(),
        root,
        &b"empty".as_slice().into(),
    )
    .await
    .unwrap();
    assert_eq!(read(&a, "empty/nested.txt").await, b"nested\n");
    assert!(id(&a, "dir").await.is_err());
}

#[test]
fn clone_rejects_destination_nested_in_source() {
    let fx = Fixture::new();
    drop(fx.open("a"));
    let nested = fx.upper("a").join("tree").join("new");
    let err = clone_upper(&fx.upper("a"), &nested).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
    assert!(!nested.exists());
}

#[tokio::test]
async fn rename_handles_follow_bytes_when_whiteout_persistence_fails() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let a = fx.open("a");
    let root = a.root_dir();
    write_new(&a, "", ".README.saving", b"saved\n").await;
    let moved = id(&a, ".README.saving").await.unwrap();
    let replaced = id(&a, "README.txt").await.unwrap();
    let tmp = fx.upper("a").join("tmp");
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o555)).unwrap();
    let result = a
        .rename(
            root,
            &b".README.saving".as_slice().into(),
            root,
            &b"README.txt".as_slice().into(),
        )
        .await;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        result.is_err(),
        "whiteout persistence failure must be reported"
    );
    assert_eq!(a.read(moved, 0, 100).await.unwrap().0, b"saved\n");
    assert_eq!(
        a.getattr(replaced).await.unwrap_err() as u32,
        nfsstat3::NFS3ERR_STALE as u32
    );
    assert_eq!(read(&a, "README.txt").await, b"saved\n");
}

#[test]
fn clone_accepts_bare_relative_destination() {
    let fx = Fixture::new();
    drop(fx.open("a"));
    let out = Command::new(env!("CARGO_BIN_EXE_macos-sparse-backend"))
        .current_dir(&fx.root)
        .args(["clone", "--from", "a", "--to", "c"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(fx.upper("c").join("tree").is_dir());
}

#[test]
fn failed_clone_removes_partial_output_with_read_only_directories() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    drop(fx.open("a"));
    let ro = fx.upper("a").join("tree/ro");
    fs::create_dir(&ro).unwrap();
    fs::write(ro.join("child"), "x").unwrap();
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    let whiteouts = fx.upper("a").join("whiteouts");
    fs::write(&whiteouts, "keep.txt\n").unwrap();
    fs::set_permissions(&whiteouts, fs::Permissions::from_mode(0o000)).unwrap();
    let result = clone_upper(&fx.upper("a"), &fx.upper("c"));
    fs::set_permissions(&whiteouts, fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
    let err = result.unwrap_err();
    assert!(!err.to_string().contains("not removed"), "{err}");
    assert!(!fx.upper("c").exists(), "partial clone must be removed");
}

#[tokio::test]
async fn rename_handles_follow_bytes_when_directory_sync_fails() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let a = fx.open("a");
    let root = a.root_dir();
    a.mkdir(root, &b"sub".as_slice().into()).await.unwrap();
    write_new(&a, "sub", "x", b"moved\n").await;
    write_new(&a, "sub", "y", b"old\n").await;
    let sub = id(&a, "sub").await.unwrap();
    let moved = id(&a, "sub/x").await.unwrap();
    let replaced = id(&a, "sub/y").await.unwrap();
    let dir = fx.upper("a").join("tree/sub");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o300)).unwrap();
    let result = a
        .rename(sub, &b"x".as_slice().into(), sub, &b"y".as_slice().into())
        .await;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        result.is_err(),
        "unreadable directory must make the sync fail"
    );
    assert_eq!(a.read(moved, 0, 100).await.unwrap().0, b"moved\n");
    assert_eq!(
        a.getattr(replaced).await.unwrap_err() as u32,
        nfsstat3::NFS3ERR_STALE as u32
    );
}

#[tokio::test]
async fn removed_handle_goes_stale_when_directory_sync_fails() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let a = fx.open("a");
    a.mkdir(a.root_dir(), &b"sub".as_slice().into())
        .await
        .unwrap();
    write_new(&a, "sub", "x", b"first\n").await;
    let sub = id(&a, "sub").await.unwrap();
    let old = id(&a, "sub/x").await.unwrap();
    let dir = fx.upper("a").join("tree/sub");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o300)).unwrap();
    let result = a.remove(sub, &b"x".as_slice().into()).await;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        result.is_err(),
        "unreadable directory must make the sync fail"
    );
    write_new(&a, "sub", "x", b"second\n").await;
    assert_ne!(
        id(&a, "sub/x").await.unwrap(),
        old,
        "recreated file must get a new handle"
    );
    assert_eq!(
        a.getattr(old).await.unwrap_err() as u32,
        nfsstat3::NFS3ERR_STALE as u32
    );
}

#[tokio::test]
async fn whiteout_memory_matches_manifest_when_upper_sync_fails() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let a = fx.open("a");
    let upper = fx.upper("a");
    fs::set_permissions(&upper, fs::Permissions::from_mode(0o300)).unwrap();
    let result = a.remove(a.root_dir(), &b"keep.txt".as_slice().into()).await;
    fs::set_permissions(&upper, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(result.is_err(), "unreadable upper must make the sync fail");
    assert!(
        id(&a, "keep.txt").await.is_err(),
        "live view must match the replaced manifest"
    );
    a.remove(a.root_dir(), &b"README.txt".as_slice().into())
        .await
        .unwrap();
    let reopened = fx.open("a");
    assert!(
        id(&reopened, "keep.txt").await.is_err(),
        "later update must keep the earlier whiteout"
    );
    assert!(id(&reopened, "README.txt").await.is_err());
}

#[tokio::test]
async fn removed_base_handle_goes_stale_when_whiteout_sync_fails() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let a = fx.open("a");
    let root = a.root_dir();
    let old = id(&a, "keep.txt").await.unwrap();
    let upper = fx.upper("a");
    fs::set_permissions(&upper, fs::Permissions::from_mode(0o300)).unwrap();
    let result = a.remove(root, &b"keep.txt".as_slice().into()).await;
    fs::set_permissions(&upper, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(result.is_err(), "unreadable upper must make the sync fail");
    write_new(&a, "", "keep.txt", b"replacement\n").await;
    assert_ne!(
        id(&a, "keep.txt").await.unwrap(),
        old,
        "recreated file must get a new handle"
    );
    assert_eq!(
        a.getattr(old).await.unwrap_err() as u32,
        nfsstat3::NFS3ERR_STALE as u32
    );
}
