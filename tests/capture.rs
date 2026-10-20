//! Daemonless capture of plain-directory Layers: stat cache, racy entries, stable
//! reads, idempotent generated operations and the cache's crash behavior.
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use javelin::{
    Capture, FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, Manifest, RepoPath,
    Repository, VersionNumber,
    capture::{StatCache, scan},
};
use tempfile::TempDir;

fn path(s: &str) -> RepoPath {
    RepoPath::parse(s).unwrap()
}

/// A repository whose World 1 holds `files`, a Layer over it, and a materialized
/// Layer directory outside the store.
fn setup(files: &[(&str, &[u8])]) -> (TempDir, Repository, LayerId, PathBuf) {
    let dir = TempDir::new().unwrap();
    let mut repo = Repository::init(dir.path()).unwrap();
    let entries = files
        .iter()
        .map(|(p, bytes)| {
            let entry = FileEntry::Regular {
                content: repo.put_blob(bytes).unwrap(),
                executable: false,
            };
            (path(p), entry)
        })
        .collect::<Vec<_>>();
    let manifest = repo.put_manifest(&Manifest::new(entries).unwrap()).unwrap();
    repo.compare_and_advance(
        &javelin::OperationId::parse("world-1").unwrap(),
        VersionNumber::new(0).unwrap(),
        manifest,
    )
    .unwrap();
    let layer = LayerId::parse("work").unwrap();
    let base = repo.layer_base(VersionNumber::new(1).unwrap()).unwrap();
    repo.create_layer(&layer, &base).unwrap();
    let root = dir.path().join("layer");
    for (p, bytes) in files {
        write(&root, p, bytes);
    }
    fs::create_dir_all(&root).unwrap();
    (dir, repo, layer, root)
}

fn write(root: &Path, p: &str, bytes: &[u8]) {
    let full = root.join(p);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, bytes).unwrap();
}

fn applied(capture: &Capture) -> LayerHead {
    match &capture.update {
        Some(LayerUpdate::Applied {
            state: LayerState::Active { head },
        }) => head.clone(),
        other => panic!("expected an applied capture, got {other:?}"),
    }
}

/// Current Layer content by path, read back from the latest checkpoint.
fn content(repo: &Repository, layer: &LayerId) -> Vec<(String, Vec<u8>)> {
    let LayerState::Active { head } = repo.layer_state(layer).unwrap() else {
        panic!("inactive")
    };
    repo.checkpoint_manifest(head.checkpoint())
        .unwrap()
        .entries()
        .iter()
        .map(|(p, e)| (p.as_str().to_owned(), repo.read_blob(e.content()).unwrap()))
        .collect()
}

fn cache_file(dir: &TempDir) -> PathBuf {
    let caches: Vec<_> = fs::read_dir(dir.path().join(".javelin/capture"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_none())
        .collect();
    assert_eq!(caches.len(), 1, "{caches:?}");
    caches[0].clone()
}

/// Waits until entries written so far are older than the racy window.
fn settle() {
    std::thread::sleep(Duration::from_nanos(
        javelin::capture::RACY_MARGIN_NS as u64 + 200_000_000,
    ));
}

#[test]
fn capture_records_edits_adds_and_deletes_against_the_base() {
    let (_dir, mut repo, layer, root) = setup(&[
        ("keep.txt", b"keep"),
        ("edit.txt", b"old"),
        ("gone.txt", b"x"),
    ]);
    let first = repo.capture_directory(&layer, &root).unwrap();
    assert!(first.update.is_none(), "a clean Layer is unchanged");
    assert!(first.rebuilt);
    write(&root, "edit.txt", b"new");
    write(&root, "dir/added.txt", b"added");
    fs::remove_file(root.join("gone.txt")).unwrap();
    fs::create_dir_all(root.join("empty/nested")).unwrap();
    let capture = repo.capture_directory(&layer, &root).unwrap();
    let head = applied(&capture);
    assert_eq!(head.revision(), 1);
    let changed: Vec<_> = capture
        .changes
        .entries()
        .iter()
        .map(|(p, e)| (p.as_str(), e.is_some()))
        .collect();
    assert_eq!(
        changed,
        [
            ("dir/added.txt", true),
            ("edit.txt", true),
            ("gone.txt", false)
        ]
    );
    assert_eq!(
        content(&repo, &layer),
        [
            ("dir/added.txt".into(), b"added".to_vec()),
            ("edit.txt".into(), b"new".to_vec()),
            ("keep.txt".into(), b"keep".to_vec()),
        ]
    );
    // Removing a Layer-only file drops it from the delta instead of a tombstone.
    fs::remove_file(root.join("dir/added.txt")).unwrap();
    let capture = repo.capture_directory(&layer, &root).unwrap();
    applied(&capture);
    assert!(
        !capture
            .changes
            .entries()
            .contains_key(&path("dir/added.txt"))
    );
}

#[test]
fn repeated_capture_is_idempotent_and_retries_replay_the_generated_operation() {
    let (dir, mut repo, layer, root) = setup(&[("a.txt", b"a")]);
    let LayerState::Active { head: start } = repo.layer_state(&layer).unwrap() else {
        panic!()
    };
    write(&root, "a.txt", b"edited");
    let capture = repo.capture_directory(&layer, &root).unwrap();
    let head = applied(&capture);
    let operation = capture.operation.clone().unwrap();
    assert!(operation.as_str().starts_with("capture-"));

    // Same directory again: nothing new is recorded.
    let again = repo.capture_directory(&layer, &root).unwrap();
    assert!(again.update.is_none());
    assert_eq!(again.head, head);

    // A retry of the same operation (lost reply) replays the original outcome.
    let replay = repo
        .capture_layer(&operation, &start, &capture.changes)
        .unwrap();
    assert_eq!(Some(replay), capture.update);

    // A crash after commit but before the cache save: no cache, same result.
    fs::remove_file(cache_file(&dir)).unwrap();
    let rebuilt = repo.capture_directory(&layer, &root).unwrap();
    assert!(rebuilt.rebuilt && rebuilt.update.is_none());
    assert_eq!(repo.layer_history(&layer).unwrap().len(), 2);
}

#[test]
fn concurrent_identical_captures_agree_on_one_revision() {
    let (dir, mut repo, layer, root) = setup(&[("a.txt", b"a")]);
    write(&root, "a.txt", b"edited");
    write(&root, "b.txt", b"added");
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let (project, layer, root) = (dir.path().to_owned(), layer.clone(), root.clone());
            std::thread::spawn(move || {
                let mut repo = Repository::open(project).unwrap();
                loop {
                    match repo.capture_directory(&layer, &root) {
                        Err(javelin::Error::Storage(_)) => continue,
                        other => return other.unwrap(),
                    }
                }
            })
        })
        .collect();
    for thread in threads {
        let capture = thread.join().unwrap();
        if capture.update.is_some() {
            assert_eq!(applied(&capture).revision(), 1);
        }
    }
    assert_eq!(repo.layer_history(&layer).unwrap().len(), 2);
    assert!(
        repo.capture_directory(&layer, &root)
            .unwrap()
            .update
            .is_none()
    );
}

#[test]
fn racy_same_size_edit_with_restored_mtime_is_caught() {
    let (_dir, mut repo, layer, root) = setup(&[]);
    write(&root, "f.txt", b"aaaa");
    repo.capture_directory(&layer, &root).unwrap();
    let mtime = fs::metadata(root.join("f.txt"))
        .unwrap()
        .modified()
        .unwrap();
    // Same size, same mtime: within one timestamp tick the fingerprint can match.
    fs::write(root.join("f.txt"), b"bbbb").unwrap();
    fs::File::options()
        .write(true)
        .open(root.join("f.txt"))
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let capture = repo.capture_directory(&layer, &root).unwrap();
    applied(&capture);
    assert_eq!(content(&repo, &layer), [("f.txt".into(), b"bbbb".to_vec())]);
}

/// Unlike the racy case above, the entry here is settled and trusted before the rewrite, so
/// only the change time can reveal it. Windows once keyed on creation time, which never moves.
#[test]
fn settled_same_size_rewrite_with_restored_mtime_is_caught() {
    let (_dir, mut repo, layer, root) = setup(&[]);
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_934_245);
    let set_mtime = |path: &Path| {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(old)
            .unwrap()
    };
    write(&root, "f.txt", b"aaaa");
    set_mtime(&root.join("f.txt"));
    settle();
    repo.capture_directory(&layer, &root).unwrap();
    let settled = repo.capture_directory(&layer, &root).unwrap();
    assert_eq!(settled.hashed, 0, "a settled entry is trusted");
    fs::write(root.join("f.txt"), b"bbbb").unwrap();
    set_mtime(&root.join("f.txt"));
    let capture = repo.capture_directory(&layer, &root).unwrap();
    assert_eq!(capture.hashed, 1);
    applied(&capture);
    assert_eq!(content(&repo, &layer), [("f.txt".into(), b"bbbb".to_vec())]);
}

#[test]
fn settled_entries_are_reused_and_edits_after_settling_are_seen() {
    let (_dir, mut repo, layer, root) = setup(&[]);
    for i in 0..20 {
        write(
            &root,
            &format!("d/f{i}.txt"),
            format!("file {i}").as_bytes(),
        );
    }
    let first = repo.capture_directory(&layer, &root).unwrap();
    assert_eq!(first.hashed, 20);
    // Every entry is racy, so the next scan re-hashes them all.
    assert_eq!(repo.capture_directory(&layer, &root).unwrap().hashed, 20);
    settle();
    // Whether the second scan was still racy depends on wall-clock time: a slow runner can
    // pass the racy window first, so this scan may trust them. Either way, the scan after
    // this one must find them old and reuse every entry.
    repo.capture_directory(&layer, &root).unwrap();
    // Now they are trusted: nothing is hashed.
    let idle = repo.capture_directory(&layer, &root).unwrap();
    assert_eq!((idle.hashed, idle.update.is_none()), (0, true));
    // Same-size rewrite, replace-by-rename and delete after settling.
    write(&root, "d/f0.txt", b"FILE 0");
    write(&root, "tmp", b"renamed in");
    fs::rename(root.join("tmp"), root.join("d/f1.txt")).unwrap();
    fs::remove_file(root.join("d/f2.txt")).unwrap();
    let capture = repo.capture_directory(&layer, &root).unwrap();
    applied(&capture);
    assert_eq!(capture.hashed, 2);
    let files = content(&repo, &layer);
    assert_eq!(files.len(), 19);
    assert!(files.contains(&("d/f0.txt".into(), b"FILE 0".to_vec())));
    assert!(files.contains(&("d/f1.txt".into(), b"renamed in".to_vec())));
}

#[test]
fn corrupt_or_torn_cache_is_rebuilt_by_hashing_everything() {
    let (dir, mut repo, layer, root) = setup(&[("a.txt", b"a"), ("b.txt", b"b")]);
    repo.capture_directory(&layer, &root).unwrap();
    let cache = cache_file(&dir);
    let bytes = fs::read(&cache).unwrap();
    // A torn write (truncated), a flipped bit and garbage all rebuild.
    for damaged in [
        bytes[..bytes.len() / 2].to_vec(),
        {
            let mut b = bytes.clone();
            b[20] ^= 1;
            b
        },
        b"garbage".to_vec(),
    ] {
        fs::write(&cache, damaged).unwrap();
        write(&root, "a.txt", b"A");
        let capture = repo.capture_directory(&layer, &root).unwrap();
        assert!(capture.rebuilt);
        assert_eq!(capture.hashed, 2);
        assert_eq!(content(&repo, &layer)[0], ("a.txt".into(), b"A".to_vec()));
        write(&root, "a.txt", b"a");
        repo.capture_directory(&layer, &root).unwrap();
    }
    // A leftover temporary from an interrupted save is ignored.
    fs::write(cache.with_extension("999.tmp"), b"partial").unwrap();
    assert!(!repo.capture_directory(&layer, &root).unwrap().rebuilt);
}

#[test]
fn case_only_rename_is_a_delete_plus_add() {
    let (_dir, mut repo, layer, root) = setup(&[("Readme.md", b"r")]);
    fs::rename(root.join("Readme.md"), root.join("README.md")).unwrap();
    let capture = repo.capture_directory(&layer, &root).unwrap();
    applied(&capture);
    let changed: Vec<_> = capture
        .changes
        .entries()
        .iter()
        .map(|(p, e)| (p.as_str(), e.is_some()))
        .collect();
    assert_eq!(changed, [("README.md", true), ("Readme.md", false)]);
}

#[test]
fn the_store_directory_is_never_captured() {
    let (dir, mut repo, layer, _root) = setup(&[]);
    fs::remove_dir_all(dir.path().join("layer")).unwrap();
    write(dir.path(), "src/main.rs", b"fn main() {}");
    let capture = repo.capture_directory(&layer, dir.path()).unwrap();
    applied(&capture);
    assert_eq!(
        content(&repo, &layer),
        [("src/main.rs".into(), b"fn main() {}".to_vec())]
    );
}

#[test]
fn ignore_rules_hide_only_untracked_outputs() {
    let (_dir, mut repo, layer, root) =
        setup(&[("build/keep.txt", b"tracked"), ("notes.txt", b"n")]);
    write(&root, ".javelinignore", b"build/\nnotes.txt\nlogs\n");
    write(&root, "build/output.o", b"private");
    write(&root, "build/keep.txt", b"tracked edit");
    write(&root, "logs/today", b"private");
    write(&root, "notes.txt", b"tracked edit");
    write(&root, "src/lib.rs", b"visible");
    applied(&repo.capture_directory(&layer, &root).unwrap());
    let paths: Vec<_> = content(&repo, &layer).into_iter().map(|(p, _)| p).collect();
    assert_eq!(
        paths,
        [
            ".javelinignore",
            "build/keep.txt",
            "notes.txt",
            "src/lib.rs"
        ]
    );

    for bad in [&b"*.o\n"[..], &[0xff][..]] {
        write(&root, ".javelinignore", bad);
        assert!(matches!(
            repo.capture_directory(&layer, &root),
            Err(javelin::Error::InvalidInput(_))
        ));
    }
}

#[test]
fn concurrent_rewrites_never_capture_a_mixed_version() {
    let (_dir, mut repo, layer, root) = setup(&[]);
    let (a, b) = (vec![b'a'; 64 * 1024], vec![b'b'; 64 * 1024]);
    write(&root, "atomic.bin", &a);
    write(&root, "inplace.bin", &a);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer = {
        let (root, stop, a, b) = (root.clone(), stop.clone(), a.clone(), b.clone());
        std::thread::spawn(move || {
            let mut i = 0usize;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let bytes = if i.is_multiple_of(2) { &b } else { &a };
                let _ = fs::write(root.join("next.tmp"), bytes);
                // Windows refuses to replace a file another handle has open.
                let _ = fs::rename(root.join("next.tmp"), root.join("atomic.bin"));
                let _ = fs::write(root.join("inplace.bin"), bytes);
                i += 1;
            }
        })
    };
    for _ in 0..20 {
        let capture = loop {
            match repo.capture_directory(&layer, &root) {
                Err(javelin::Error::Io(e)) if e.kind() == std::io::ErrorKind::PermissionDenied => {}
                other => break other.unwrap(),
            }
        };
        if capture.update.is_some() {
            for (p, bytes) in content(&repo, &layer) {
                // A replaced inode is always read whole.
                if p == "atomic.bin" {
                    assert!(bytes == a || bytes == b, "mixed atomic.bin");
                }
            }
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    writer.join().unwrap();
    let _ = fs::remove_file(root.join("next.tmp"));
    // Once writes stop, capture converges on the final bytes of every file.
    let (atomic, inplace) = (
        fs::read(root.join("atomic.bin")).unwrap(),
        fs::read(root.join("inplace.bin")).unwrap(),
    );
    let capture = repo.capture_directory(&layer, &root).unwrap();
    assert!(capture.uncertain.is_empty());
    let files = content(&repo, &layer);
    assert_eq!(
        files,
        [
            ("atomic.bin".into(), atomic),
            ("inplace.bin".into(), inplace)
        ]
    );
}

#[test]
fn scan_works_on_any_directory_without_a_repository() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "x/y.txt", b"y");
    let mut cache = StatCache::load(&dir.path().join("missing-cache"));
    assert!(cache.rebuilt() && cache.is_empty());
    let mut stored = Vec::new();
    let scan = scan(
        dir.path(),
        &mut cache,
        &Manifest::new([]).unwrap(),
        &mut |bytes| {
            stored.push(bytes.to_vec());
            Ok(javelin::ObjectId::for_blob(bytes))
        },
    )
    .unwrap();
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(stored, [b"y".to_vec()]);
    assert_eq!(cache.len(), 1);
    cache.save(&dir.path().join("c/cache")).unwrap();
    let loaded = StatCache::load(&dir.path().join("c/cache"));
    assert!(!loaded.rebuilt());
    assert_eq!(loaded.len(), 1);
}

#[cfg(unix)]
#[test]
fn symlinks_are_read_not_followed_and_the_executable_bit_is_kept() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (dir, mut repo, layer, root) = setup(&[("tool.sh", b"#!/bin/sh\n")]);
    fs::set_permissions(root.join("tool.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    let outside = dir.path().join("outside");
    write(&outside, "secret", b"never captured");
    symlink(&outside, root.join("link-dir")).unwrap();
    symlink("tool.sh", root.join("link")).unwrap();
    let capture = repo.capture_directory(&layer, &root).unwrap();
    applied(&capture);
    let entries = capture.changes.entries();
    assert_eq!(entries.len(), 3);
    assert!(matches!(
        entries[&path("tool.sh")],
        Some(FileEntry::Regular {
            executable: true,
            ..
        })
    ));
    let Some(FileEntry::Symlink { target }) = &entries[&path("link")] else {
        panic!("symlink kind lost")
    };
    assert_eq!(repo.read_blob(*target).unwrap(), b"tool.sh");
    assert!(matches!(
        entries[&path("link-dir")],
        Some(FileEntry::Symlink { .. })
    ));
    // Retargeting a symlink is a change; clearing the bit is a change.
    fs::remove_file(root.join("link")).unwrap();
    symlink("elsewhere", root.join("link")).unwrap();
    fs::set_permissions(root.join("tool.sh"), fs::Permissions::from_mode(0o644)).unwrap();
    let capture = repo.capture_directory(&layer, &root).unwrap();
    applied(&capture);
    assert!(!capture.changes.entries().contains_key(&path("tool.sh")));
    let Some(FileEntry::Symlink { target }) = &capture.changes.entries()[&path("link")] else {
        panic!()
    };
    assert_eq!(repo.read_blob(*target).unwrap(), b"elsewhere");
}

#[cfg(unix)]
#[test]
fn special_files_refuse_capture() {
    let (_dir, mut repo, layer, root) = setup(&[]);
    let fifo = std::ffi::CString::new(root.join("pipe").to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
    let error = repo.capture_directory(&layer, &root).unwrap_err();
    assert_eq!(error.code(), "unsupported");
}
