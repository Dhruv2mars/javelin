//! Refreshing a clone-backed directory after Stack moved its head.
//!
//! Stack is simulated with `commit_stack`, which moves the head exactly as the Stack
//! adapter does: the full change set against the Layer's base, with the source as an
//! ancestor. Each Layer starts at its base, so that change set is the stacked work alone.
use std::{collections::BTreeMap, fs, path::Path};

use javelin::{
    AdvanceOutcome, Error, FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, Manifest,
    OperationId, PublishAttribution, RepoPath, Repository, SparseChanges, VersionNumber,
};
use tempfile::TempDir;

fn scratch() -> TempDir {
    match std::env::var_os("JAVELIN_WORKSPACE_TEST_DIR") {
        Some(dir) => {
            fs::create_dir_all(&dir).unwrap();
            TempDir::new_in(dir).unwrap()
        }
        None => TempDir::new().unwrap(),
    }
}

fn id(s: &str) -> LayerId {
    LayerId::parse(s).unwrap()
}

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}

/// Publishes the next World with exactly these files.
fn world(repo: &mut Repository, files: &[(&str, &[u8])]) -> VersionNumber {
    let entries: Vec<_> = files
        .iter()
        .map(|(p, bytes)| {
            let entry = FileEntry::Regular {
                content: repo.put_blob(bytes).unwrap(),
                executable: false,
            };
            (RepoPath::parse(p).unwrap(), entry)
        })
        .collect();
    let manifest = repo.put_manifest(&Manifest::new(entries).unwrap()).unwrap();
    let head = repo.history().unwrap().last().unwrap().number;
    let operation = op(&format!("world-{}", head.get() + 1));
    match repo
        .compare_and_advance(&operation, head, manifest)
        .unwrap()
    {
        AdvanceOutcome::Accepted { version } => version.number,
        other => panic!("{other:?}"),
    }
}

/// Creates the Layer at World `version` and opens its directory.
fn open(repo: &mut Repository, layer: &str, version: VersionNumber) -> std::path::PathBuf {
    let base = repo.layer_base(version).unwrap();
    repo.create_layer(&id(layer), &base).unwrap();
    repo.open_workspace(&id(layer)).unwrap().root
}

fn head(repo: &Repository, layer: &str) -> LayerHead {
    match repo.layer_state(&id(layer)).unwrap() {
        LayerState::Active { head } => head,
        other => panic!("{other:?}"),
    }
}

fn applied(update: LayerUpdate) -> LayerHead {
    match update {
        LayerUpdate::Applied {
            state: LayerState::Active { head },
        } => head,
        other => panic!("{other:?}"),
    }
}

/// Regular files under `root` as path to bytes. Symlinks are not used in these tests.
fn contents(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let name = path.strip_prefix(root).unwrap().to_str().unwrap();
                out.insert(name.replace('\\', "/"), fs::read(&path).unwrap());
            }
        }
    }
    out
}

fn expected(files: &[(&str, &[u8])]) -> BTreeMap<String, Vec<u8>> {
    files
        .iter()
        .map(|(p, bytes)| ((*p).to_owned(), bytes.to_vec()))
        .collect()
}

/// Stack `files` onto Layer `target` through a source Layer. `files` is the whole change
/// set against the target's base World, so the target must still be at its base.
fn stack(
    repo: &mut Repository,
    target: &str,
    version: VersionNumber,
    files: &[(&str, &[u8])],
    label: &str,
) -> LayerHead {
    let changes = SparseChanges::new(files.iter().map(|(p, bytes)| {
        let entry = FileEntry::Regular {
            content: repo.put_blob(bytes).unwrap(),
            executable: false,
        };
        (RepoPath::parse(p).unwrap(), Some(entry))
    }))
    .unwrap();
    stack_changes(repo, target, version, changes, label)
}

/// Stack `changes` onto Layer `target`, which must still be at `version`. A `None` entry
/// deletes its path.
fn stack_changes(
    repo: &mut Repository,
    target: &str,
    version: VersionNumber,
    changes: SparseChanges,
    label: &str,
) -> LayerHead {
    let base = repo.layer_base(version).unwrap();
    let source = repo
        .create_layer(&id(&format!("source-{label}")), &base)
        .unwrap();
    let source = applied(
        repo.capture_layer(&op(&format!("{label}-source")), &source, &changes)
            .unwrap(),
    );
    let target_head = head(repo, target);
    applied(
        repo.commit_stack(
            &op(&format!("{label}-stack")),
            &target_head,
            source.checkpoint(),
            &changes,
        )
        .unwrap(),
    )
}

#[test]
fn clean_refresh_brings_the_directory_to_the_stacked_head_and_captures_cleanly() {
    let root = scratch();
    let mut repo = Repository::init(root.path()).unwrap();
    let files: [(&str, &[u8]); 2] = [("keep", b"keep"), ("shared", b"base")];
    let version = world(&mut repo, &files);
    let dir = open(&mut repo, "work", version);
    let stacked: [(&str, &[u8]); 2] = [("shared", b"stacked"), ("stacked.txt", b"new")];
    stack(&mut repo, "work", version, &stacked, "clean");

    let before = repo.capture_workspace(&id("work")).unwrap().unwrap();
    assert!(
        before.behind,
        "a stale directory must not be captured over Stack's work"
    );
    assert_eq!(contents(&dir), expected(&files), "capture did not run");

    let refresh = repo.refresh_workspace(&id("work")).unwrap();
    assert!(refresh.refreshed);
    assert_eq!((refresh.applied, refresh.kept), (2, 0));
    assert_eq!(
        contents(&dir),
        expected(&[
            ("keep", b"keep"),
            ("shared", b"stacked"),
            ("stacked.txt", b"new")
        ])
    );

    let after = repo.capture_workspace(&id("work")).unwrap().unwrap();
    assert!(!after.behind);
    assert!(after.update.is_none(), "{after:?}");
    assert!(!repo.refresh_workspace(&id("work")).unwrap().refreshed);
}

#[test]
fn refresh_keeps_unrelated_local_edits_and_the_next_capture_includes_them() {
    let root = scratch();
    let mut repo = Repository::init(root.path()).unwrap();
    let version = world(&mut repo, &[("keep", b"keep"), ("shared", b"base")]);
    let dir = open(&mut repo, "work", version);
    fs::write(dir.join("keep"), b"edited").unwrap();
    fs::write(dir.join("local.txt"), b"mine").unwrap();
    stack(
        &mut repo,
        "work",
        version,
        &[("stacked.txt", b"new")],
        "unrelated",
    );

    let refresh = repo.refresh_workspace(&id("work")).unwrap();
    assert_eq!((refresh.applied, refresh.kept), (1, 2));
    assert_eq!(
        contents(&dir),
        expected(&[
            ("keep", b"edited"),
            ("shared", b"base"),
            ("local.txt", b"mine"),
            ("stacked.txt", b"new"),
        ])
    );

    let capture = repo.capture_workspace(&id("work")).unwrap().unwrap();
    assert!(!capture.behind);
    assert!(matches!(capture.update, Some(LayerUpdate::Applied { .. })));
    let captured: Vec<&str> = capture
        .changes
        .entries()
        .keys()
        .map(RepoPath::as_str)
        .collect();
    assert_eq!(captured, ["keep", "local.txt", "stacked.txt"]);
}

#[test]
fn refresh_blocked_by_a_conflicting_edit_changes_nothing() {
    let root = scratch();
    let mut repo = Repository::init(root.path()).unwrap();
    let version = world(&mut repo, &[("shared", b"base")]);
    let dir = open(&mut repo, "work", version);
    fs::write(dir.join("shared"), b"mine").unwrap();
    fs::write(dir.join("x"), b"local file").unwrap();
    stack(
        &mut repo,
        "work",
        version,
        &[("shared", b"theirs"), ("x/y", b"stacked under x")],
        "conflict",
    );

    let error = repo.refresh_workspace(&id("work")).unwrap_err();
    assert_eq!(error.code(), "refresh_conflict", "{error}");
    match error {
        Error::RefreshConflict(paths) => assert_eq!(paths, ["shared", "x"]),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        contents(&dir),
        expected(&[("shared", b"mine"), ("x", b"local file")]),
        "a refused refresh must not write"
    );

    // Still behind and still refused: the sync record did not move.
    let capture = repo.capture_workspace(&id("work")).unwrap().unwrap();
    assert!(capture.behind);
    assert!(repo.refresh_workspace(&id("work")).is_err());
}

#[test]
fn publish_after_refresh_publishes_the_stacked_work_and_local_edits() {
    let root = scratch();
    let mut repo = Repository::init(root.path()).unwrap();
    let version = world(&mut repo, &[("shared", b"base")]);
    let dir = open(&mut repo, "work", version);
    fs::write(dir.join("shared"), b"mine").unwrap();
    stack(
        &mut repo,
        "work",
        version,
        &[("stacked.txt", b"new")],
        "publish",
    );

    repo.refresh_workspace(&id("work")).unwrap();
    let capture = repo.capture_workspace(&id("work")).unwrap().unwrap();
    assert!(!capture.behind);
    assert!(capture.uncertain.is_empty());

    repo.publish_layer(
        &op("publish-after-refresh"),
        &head(&repo, "work"),
        &PublishAttribution::Human,
        |_, _| Ok(()),
    )
    .unwrap();
    let published = repo.history().unwrap().last().unwrap().number;
    let changed: Vec<String> = repo
        .changeset(published)
        .unwrap()
        .entries()
        .keys()
        .map(|p| p.as_str().to_owned())
        .collect();
    assert_eq!(changed, ["shared", "stacked.txt"]);
}

#[test]
fn checkpoint_after_refresh_captures_the_directory() {
    let root = scratch();
    let mut repo = Repository::init(root.path()).unwrap();
    let version = world(&mut repo, &[("shared", b"base")]);
    let dir = open(&mut repo, "work", version);
    stack(
        &mut repo,
        "work",
        version,
        &[("stacked.txt", b"new")],
        "checkpoint",
    );
    repo.refresh_workspace(&id("work")).unwrap();

    // The `checkpoint` command captures through this path, which must accept a synced head.
    let capture = repo.capture_directory(&id("work"), &dir).unwrap();
    assert!(!capture.behind, "a synced stacked head must capture");
    assert!(capture.update.is_none(), "{capture:?}");
}

#[test]
fn refresh_refuses_to_overwrite_an_ignored_file_the_head_adds() {
    let root = scratch();
    let mut repo = Repository::init(root.path()).unwrap();
    let version = world(&mut repo, &[("shared", b"base")]);
    let dir = open(&mut repo, "work", version);
    fs::write(dir.join(".javelinignore"), b"new.txt\n").unwrap();
    fs::write(dir.join("new.txt"), b"local").unwrap();
    stack(
        &mut repo,
        "work",
        version,
        &[("new.txt", b"theirs")],
        "ignored",
    );

    let error = repo.refresh_workspace(&id("work")).unwrap_err();
    match error {
        Error::RefreshConflict(paths) => assert_eq!(paths, ["new.txt"]),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        contents(&dir),
        expected(&[
            ("shared", b"base"),
            (".javelinignore", b"new.txt\n"),
            ("new.txt", b"local"),
        ]),
        "a refused refresh must not write"
    );
}

#[test]
fn refresh_finishes_an_interrupted_write_to_an_ignored_file() {
    let root = scratch();
    let mut repo = Repository::init(root.path()).unwrap();
    let version = world(&mut repo, &[("shared", b"base")]);
    let dir = open(&mut repo, "work", version);
    fs::write(dir.join(".javelinignore"), b"a/new.txt\n").unwrap();
    stack(
        &mut repo,
        "work",
        version,
        &[("a/new.txt", b"theirs"), ("b/new.txt", b"theirs")],
        "ignored",
    );
    // An interrupted refresh wrote the ignored file, then failed before the next path.
    fs::create_dir(dir.join("a")).unwrap();
    fs::write(dir.join("a/new.txt"), b"theirs").unwrap();

    let refresh = repo.refresh_workspace(&id("work")).unwrap();
    assert!(refresh.refreshed);
    assert_eq!(
        refresh.applied, 2,
        "the ignored path is a head change, not a local edit"
    );
    assert_eq!(
        contents(&dir),
        expected(&[
            ("shared", b"base"),
            (".javelinignore", b"a/new.txt\n"),
            ("a/new.txt", b"theirs"),
            ("b/new.txt", b"theirs"),
        ])
    );
}

#[test]
fn refresh_refuses_an_identical_head_name_that_aliases_a_tracked_file() {
    for (tracked, alias) in [("Foo", "foo"), ("\u{e9}", "e\u{301}")] {
        let root = scratch();
        let mut repo = Repository::init(root.path()).unwrap();
        let version = world(&mut repo, &[(tracked, b"same")]);
        let dir = open(&mut repo, "work", version);
        if !dir.join(alias).exists() {
            continue;
        }
        stack(&mut repo, "work", version, &[(alias, b"same")], "alias");

        let error = repo.refresh_workspace(&id("work")).unwrap_err();
        match error {
            Error::RefreshConflict(paths) => assert_eq!(paths, [alias]),
            other => panic!("{other:?}"),
        }
        assert_eq!(contents(&dir), expected(&[(tracked, b"same")]));
    }
}

#[test]
fn refresh_refuses_a_name_that_aliases_a_local_file() {
    let root = scratch();
    let mut repo = Repository::init(root.path()).unwrap();
    let version = world(&mut repo, &[("shared", b"base")]);
    let dir = open(&mut repo, "work", version);
    fs::write(dir.join("NEW.txt"), b"local").unwrap();
    if !dir.join("new.txt").exists() {
        // A case-sensitive volume keeps both names, so there is no alias to refuse.
        return;
    }
    stack(
        &mut repo,
        "work",
        version,
        &[("new.txt", b"theirs")],
        "alias",
    );

    let error = repo.refresh_workspace(&id("work")).unwrap_err();
    match error {
        Error::RefreshConflict(paths) => assert_eq!(paths, ["new.txt"]),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        contents(&dir),
        expected(&[("shared", b"base"), ("NEW.txt", b"local")]),
        "the local file must keep its bytes"
    );
}

#[cfg(unix)]
#[test]
fn refresh_interrupted_midway_converges_when_run_again() {
    use std::os::unix::fs::PermissionsExt;

    let root = scratch();
    let mut repo = Repository::init(root.path()).unwrap();
    let version = world(&mut repo, &[("a/1", b"a1"), ("b/2", b"b2")]);
    let dir = open(&mut repo, "work", version);
    fs::write(dir.join("b/2"), b"mine").unwrap();
    stack(
        &mut repo,
        "work",
        version,
        &[("a/1", b"stacked a1"), ("a/2", b"new"), ("b/3", b"new b")],
        "interrupt",
    );

    // Refresh writes in path order: a/1 and a/2 land, then b/3 fails in the read-only b.
    let b = dir.join("b");
    fs::set_permissions(&b, fs::Permissions::from_mode(0o555)).unwrap();
    let error = repo.refresh_workspace(&id("work")).unwrap_err();
    fs::set_permissions(&b, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(error, Error::Io(_)), "{error}");
    assert_eq!(
        contents(&dir),
        expected(&[("a/1", b"stacked a1"), ("a/2", b"new"), ("b/2", b"mine")]),
        "the interrupted refresh left a partial, never a torn, state"
    );

    let refresh = repo.refresh_workspace(&id("work")).unwrap();
    assert!(refresh.refreshed);
    assert_eq!((refresh.applied, refresh.kept), (1, 1));
    assert_eq!(
        contents(&dir),
        expected(&[
            ("a/1", b"stacked a1"),
            ("a/2", b"new"),
            ("b/2", b"mine"),
            ("b/3", b"new b"),
        ])
    );
    let capture = repo.capture_workspace(&id("work")).unwrap().unwrap();
    assert!(!capture.behind);
    assert!(matches!(capture.update, Some(LayerUpdate::Applied { .. })));
    let again = repo.capture_workspace(&id("work")).unwrap().unwrap();
    assert!(again.update.is_none(), "{again:?}");
}

#[test]
fn refresh_applies_a_head_that_turns_a_file_into_a_directory() {
    let root = scratch();
    let mut repo = Repository::init(root.path()).unwrap();
    let version = world(&mut repo, &[("keep", b"keep"), ("x", b"base")]);
    let dir = open(&mut repo, "work", version);
    let entry = FileEntry::Regular {
        content: repo.put_blob(b"new").unwrap(),
        executable: false,
    };
    let changes = SparseChanges::new(vec![
        (RepoPath::parse("x").unwrap(), None),
        (RepoPath::parse("x/y").unwrap(), Some(entry)),
    ])
    .unwrap();
    stack_changes(&mut repo, "work", version, changes, "file-to-dir");

    let refresh = repo.refresh_workspace(&id("work")).unwrap();
    assert!(refresh.refreshed);
    assert_eq!(
        contents(&dir),
        expected(&[("keep", b"keep"), ("x/y", b"new")])
    );
}

#[test]
fn refresh_refuses_head_names_that_alias_each_other_on_this_volume() {
    // The volume decides which pairs alias: a case fold on one, a Unicode normalization on another.
    for (first, second) in [("Foo", "foo"), ("\u{e9}", "e\u{301}")] {
        let root = scratch();
        fs::write(root.path().join(first), b"").unwrap();
        let aliases = root.path().join(second).exists();
        let mut repo = Repository::init(root.path()).unwrap();
        let version = world(&mut repo, &[("shared", b"base")]);
        let dir = open(&mut repo, "work", version);
        stack(
            &mut repo,
            "work",
            version,
            &[(first, b"a"), (second, b"b")],
            "pair",
        );

        if !aliases {
            // A volume that keeps both names applies both.
            assert!(repo.refresh_workspace(&id("work")).unwrap().refreshed);
            assert_eq!(
                contents(&dir),
                expected(&[("shared", b"base"), (first, b"a"), (second, b"b")])
            );
            continue;
        }
        let error = repo.refresh_workspace(&id("work")).unwrap_err();
        // The later of the two in path order is refused; the earlier one already claims the entry.
        match error {
            Error::RefreshConflict(paths) => {
                assert_eq!(paths.len(), 1, "{paths:?}");
                assert!([first, second].contains(&paths[0].as_str()), "{paths:?}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            contents(&dir),
            expected(&[("shared", b"base")]),
            "a refused refresh must not write"
        );
    }
}

#[test]
fn refresh_survives_a_head_deletion_under_a_directory_that_became_a_file() {
    let root = scratch();
    let mut repo = Repository::init(root.path()).unwrap();
    let version = world(&mut repo, &[("keep", b"keep"), ("a/b/c", b"base")]);
    let dir = open(&mut repo, "work", version);
    fs::remove_dir_all(dir.join("a")).unwrap();
    fs::write(dir.join("a"), b"file").unwrap();
    let changes = SparseChanges::new(vec![
        (RepoPath::parse("a/b/c").unwrap(), None),
        (
            RepoPath::parse("new").unwrap(),
            Some(FileEntry::Regular {
                content: repo.put_blob(b"new").unwrap(),
                executable: false,
            }),
        ),
    ])
    .unwrap();
    stack_changes(&mut repo, "work", version, changes, "under-file");

    let refresh = repo.refresh_workspace(&id("work")).unwrap();
    assert!(refresh.refreshed);
    assert_eq!(
        contents(&dir),
        expected(&[("a", b"file"), ("keep", b"keep"), ("new", b"new")])
    );
}

#[test]
fn capture_through_a_relative_path_to_the_workspace_advances_its_sync_record() {
    let root = scratch();
    let mut repo = Repository::init(root.path()).unwrap();
    let version = world(&mut repo, &[("keep", b"keep")]);
    let dir = open(&mut repo, "work", version);
    stack(
        &mut repo,
        "work",
        version,
        &[("stacked.txt", b"new")],
        "relative",
    );
    repo.refresh_workspace(&id("work")).unwrap();

    fs::write(dir.join("keep"), b"edited").unwrap();
    let capture = repo.capture_directory(&id("work"), &dir.join(".")).unwrap();
    assert!(!capture.behind);
    let captured = head(&repo, "work").checkpoint();

    stack(
        &mut repo,
        "work",
        version,
        &[("stacked.txt", b"new"), ("more.txt", b"more")],
        "relative-again",
    );
    let refresh = repo.refresh_workspace(&id("work")).unwrap();
    assert_eq!(
        refresh.from, captured,
        "the capture must move the sync record"
    );
}
