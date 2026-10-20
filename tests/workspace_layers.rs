//! Clone-backed Layer directories on the volume under test.
//!
//! `JAVELIN_WORKSPACE_TEST_DIR` places repositories on a chosen volume, and
//! `JAVELIN_WORKSPACE_EXPECT` names the strategy that volume must use
//! (`directory_clone`, `snapshot`, `file_clone` or `copy`). CI sets both per filesystem.
use std::{collections::BTreeMap, fs, path::Path, time::Instant};

use javelin::{
    BaseSource, CloneStrategy, FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, Manifest,
    OperationId, PublicationReceipt, PublishAttribution, PublishOutcome, RepoPath, Repository,
    SparseChanges, VersionNumber, WorkspaceReport,
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

#[derive(Clone)]
enum Spec {
    File(Vec<u8>, bool),
    #[cfg_attr(not(unix), allow(dead_code))]
    Link(&'static str),
}

fn file(bytes: &[u8]) -> Spec {
    Spec::File(bytes.to_vec(), false)
}

fn entry(repo: &mut Repository, spec: &Spec) -> FileEntry {
    match spec {
        Spec::File(bytes, executable) => FileEntry::Regular {
            content: repo.put_blob(bytes).unwrap(),
            executable: *executable,
        },
        Spec::Link(target) => FileEntry::Symlink {
            target: repo.put_blob(target.as_bytes()).unwrap(),
        },
    }
}

/// Publishes the next World with exactly these paths.
fn world(repo: &mut Repository, specs: &BTreeMap<String, Spec>) -> VersionNumber {
    let entries: Vec<_> = specs
        .iter()
        .map(|(p, s)| (RepoPath::parse(p).unwrap(), entry(repo, s)))
        .collect();
    let manifest = repo.put_manifest(&Manifest::new(entries).unwrap()).unwrap();
    let head = repo.history().unwrap().last().unwrap().number;
    let operation = op(&format!("world-{}", head.get() + 1));
    match repo
        .compare_and_advance(&operation, head, manifest)
        .unwrap()
    {
        javelin::AdvanceOutcome::Accepted { version } => version.number,
        other => panic!("{other:?}"),
    }
}

fn open(repo: &mut Repository, layer: &str, version: VersionNumber) -> WorkspaceReport {
    let base = repo.layer_base(version).unwrap();
    repo.create_layer(&id(layer), &base).unwrap();
    repo.open_workspace(&id(layer)).unwrap()
}

fn head(repo: &Repository, layer: &str) -> LayerHead {
    match repo.layer_state(&id(layer)).unwrap() {
        LayerState::Active { head } => head,
        other => panic!("{other:?}"),
    }
}

/// Reads a directory tree back into the same shape as a spec.
fn tree(root: &Path) -> BTreeMap<String, (Vec<u8>, bool, bool)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .replace('\\', "/");
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_symlink() {
                let target = fs::read_link(&path).unwrap();
                out.insert(
                    name,
                    (target.to_str().unwrap().as_bytes().to_vec(), false, true),
                );
            } else {
                out.insert(name, (fs::read(&path).unwrap(), executable(&path), false));
            }
        }
    }
    out
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o100 != 0
}
#[cfg(not(unix))]
fn executable(_: &Path) -> bool {
    false
}

fn expected(specs: &BTreeMap<String, Spec>) -> BTreeMap<String, (Vec<u8>, bool, bool)> {
    specs
        .iter()
        .map(|(p, s)| {
            let value = match s {
                Spec::File(bytes, x) => (bytes.clone(), *x && cfg!(unix), false),
                Spec::Link(target) => (target.as_bytes().to_vec(), false, true),
            };
            (p.clone(), value)
        })
        .collect()
}

fn sample() -> BTreeMap<String, Spec> {
    let mut specs = BTreeMap::new();
    specs.insert("src/main.rs".into(), file(b"fn main() {}\n"));
    specs.insert("src/lib/deep/mod.rs".into(), file(b"pub fn f() {}\n"));
    specs.insert(
        "bin/run".into(),
        Spec::File(b"#!/bin/sh\necho ok\n".to_vec(), true),
    );
    specs.insert("README.md".into(), file(b"hello\n"));
    specs.insert("empty".into(), file(b""));
    #[cfg(unix)]
    specs.insert("link".into(), Spec::Link("src/main.rs"));
    specs
}

fn expect_strategy(report: &WorkspaceReport) {
    let clone = report.clone.as_ref().unwrap();
    if let Ok(name) = std::env::var("JAVELIN_WORKSPACE_EXPECT") {
        assert_eq!(clone.strategy.as_str(), name, "{clone:?}");
    }
    if clone.strategy == CloneStrategy::Copy {
        assert!(clone.fallback.is_some(), "a copy must say why: {clone:?}");
    } else {
        assert_eq!(clone.copied_bytes, 0, "{clone:?}");
    }
}

#[test]
fn layer_directory_matches_its_world_and_stays_isolated() {
    let dir = scratch();
    let mut repo = Repository::init(dir.path()).unwrap();
    let specs = sample();
    let v1 = world(&mut repo, &specs);
    let a = open(&mut repo, "a", v1);
    assert!(a.created);
    expect_strategy(&a);
    let base = a.base.as_ref().unwrap();
    assert_eq!(base.source, BaseSource::Materialized);
    assert_eq!(base.changed_paths, specs.len());
    assert_eq!(tree(&a.root), expected(&specs));
    if a.clone.as_ref().unwrap().strategy == CloneStrategy::Copy {
        let bytes: usize = specs
            .values()
            .map(|s| match s {
                Spec::File(b, _) => b.len(),
                Spec::Link(_) => 0,
            })
            .sum();
        assert_eq!(a.clone.as_ref().unwrap().copied_bytes, bytes as u64);
    }

    let workspace = repo.layer_workspace(&id("a")).unwrap();
    assert!(workspace.exists);
    assert_eq!(workspace.root, a.root);
    assert_eq!(workspace.base_version, v1);
    assert!(
        workspace
            .root
            .starts_with(dir.path().canonicalize().unwrap())
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&workspace.base_root)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0, "bases are sealed");
        assert!(fs::write(workspace.base_root.join("README.md"), b"x").is_err());
        assert!(fs::metadata(&a.root).unwrap().permissions().mode() & 0o200 != 0);
    }

    // Ordinary edits: in-place write, atomic-save rename, delete, new directories.
    fs::write(a.root.join("src/main.rs"), b"edited").unwrap();
    fs::write(a.root.join("README.md.tmp"), b"saved").unwrap();
    fs::rename(a.root.join("README.md.tmp"), a.root.join("README.md")).unwrap();
    fs::remove_file(a.root.join("empty")).unwrap();
    fs::create_dir_all(a.root.join("new/dir")).unwrap();
    fs::write(a.root.join("new/dir/x"), b"x").unwrap();
    assert_eq!(fs::read(a.root.join("README.md")).unwrap(), b"saved");

    // A second Layer and a reopen see only the World.
    let again = repo.open_workspace(&id("a")).unwrap();
    assert!(!again.created && again.base.is_none());
    let b = open(&mut repo, "b", v1);
    assert_eq!(b.base.as_ref().unwrap().source, BaseSource::Cached);
    assert_eq!(tree(&b.root), expected(&specs));
    assert_eq!(fs::read(a.root.join("src/main.rs")).unwrap(), b"edited");
}

#[test]
fn checkpoint_changes_apply_on_top_of_the_base_clone() {
    let dir = scratch();
    let mut repo = Repository::init(dir.path()).unwrap();
    let specs = sample();
    let v1 = world(&mut repo, &specs);
    let base = repo.layer_base(v1).unwrap();
    let source = repo.create_layer(&id("source"), &base).unwrap();
    let changed = entry(&mut repo, &file(b"changed"));
    let added = entry(&mut repo, &file(b"added"));
    let changes = SparseChanges::new([
        (RepoPath::parse("src/main.rs").unwrap(), Some(changed)),
        (RepoPath::parse("src/lib/deep/mod.rs").unwrap(), None),
        (
            RepoPath::parse("src/lib/other/added.rs").unwrap(),
            Some(added),
        ),
    ])
    .unwrap();
    let head = match repo
        .capture_layer(&op("capture"), &source, &changes)
        .unwrap()
    {
        LayerUpdate::Applied {
            state: LayerState::Active { head },
        } => head,
        other => panic!("{other:?}"),
    };
    repo.clone_layer(&id("copy"), head.checkpoint()).unwrap();
    let report = repo.open_workspace(&id("copy")).unwrap();
    assert_eq!(report.applied_changes, 3);
    let checkpoint = repo.read_checkpoint(head.checkpoint()).unwrap();
    let mut want = specs.clone();
    want.insert("src/main.rs".into(), file(b"changed"));
    want.remove("src/lib/deep/mod.rs");
    want.insert("src/lib/other/added.rs".into(), file(b"added"));
    assert_eq!(
        repo.checkpoint_manifest(checkpoint.id())
            .unwrap()
            .entries()
            .len(),
        want.len()
    );
    assert_eq!(tree(&report.root), expected(&want));
    assert!(!report.root.join("src/lib/deep").exists());
}

#[test]
fn discard_is_deferred_idempotent_and_recovers_after_interruption() {
    let dir = scratch();
    let mut repo = Repository::init(dir.path()).unwrap();
    let v1 = world(&mut repo, &sample());
    let store = dir.path().canonicalize().unwrap().join(".javelin");
    let trash = || fs::read_dir(store.join("trash")).unwrap().count();

    let a = open(&mut repo, "a", v1);
    fs::write(a.root.join("unsaved"), b"discard me").unwrap();
    let expected = head(&repo, "a");
    let first = repo.discard_workspace(&op("discard-a"), &expected).unwrap();
    assert!(matches!(first.update, LayerUpdate::Applied { .. }));
    assert!(first.trashed && !a.root.exists());
    assert_eq!(trash(), 0, "small trash is reclaimed within the budget");
    let retry = repo.discard_workspace(&op("discard-a"), &expected).unwrap();
    assert_eq!(retry.update, first.update);
    assert!(!retry.trashed);
    assert!(repo.open_workspace(&id("a")).is_err());

    // Interrupted after the database decided but before the rename.
    let b = open(&mut repo, "b", v1);
    repo.discard_layer(&op("discard-b"), &head(&repo, "b"))
        .unwrap();
    assert!(b.root.exists());
    let recovery = repo.recover_workspaces().unwrap();
    assert_eq!(recovery.trashed, 1);
    assert!(!b.root.exists());
    assert_eq!(trash(), 0);

    // An active Layer's directory is never deleted, even if found in trash.
    let c = open(&mut repo, "c", v1);
    let held = store.join("trash").join("layer~c~manual");
    fs::rename(&c.root, &held).unwrap();
    let recovery = repo.recover_workspaces().unwrap();
    assert_eq!(recovery.retained, vec!["layer~c~manual".to_string()]);
    assert!(held.join("README.md").exists());
    fs::rename(&held, &c.root).unwrap();

    // Abandoned staging is swept; large trash is reclaimed in bounded steps.
    fs::create_dir_all(store.join("tmp/layer~x~stale/sub")).unwrap();
    let big = store.join("trash").join("tmp~big");
    fs::create_dir_all(&big).unwrap();
    for n in 0..5000 {
        fs::write(big.join(n.to_string()), b"").unwrap();
    }
    let d = open(&mut repo, "d", v1);
    assert_eq!(d.recovery.trashed, 1);
    assert_eq!(d.recovery.reclaimed, 4096);
    assert_eq!(d.recovery.remaining, 1);
    let recovery = repo.recover_workspaces().unwrap();
    assert_eq!(recovery.remaining, 0);
    assert_eq!(trash(), 0);
    assert!(fs::read_dir(store.join("tmp")).unwrap().next().is_none());

    // A crash between publishing and sealing a base leaves it open; recovery reseals.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let base = repo.layer_workspace(&id("d")).unwrap().base_root;
        fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
        repo.recover_workspaces().unwrap();
        assert_eq!(fs::metadata(&base).unwrap().permissions().mode() & 0o777, 0);
    }
}

/// Entries in the bases directory of the repository, as seen through `layer`'s base path.
fn base_entries(repo: &Repository, layer: &str) -> Vec<String> {
    let rolling = repo.layer_workspace(&id(layer)).unwrap().base_root;
    let mut names: Vec<String> = fs::read_dir(rolling.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn one_rolling_base_advances_in_place_and_older_worlds_rewind_a_clone() {
    let dir = scratch();
    let mut repo = Repository::init(dir.path()).unwrap();
    let v1_specs = sample();
    let mut specs = v1_specs.clone();
    let v1 = world(&mut repo, &specs);
    let one = open(&mut repo, "one", v1);
    assert_eq!(one.base.as_ref().unwrap().source, BaseSource::Materialized);
    let rolling = repo.layer_workspace(&id("one")).unwrap().base_root;

    specs.insert("src/main.rs".into(), file(b"fn main() { v2() }\n"));
    specs.remove("src/lib/deep/mod.rs");
    specs.insert("src/new/added.rs".into(), file(b"added\n"));
    specs.insert(
        "README.md".into(),
        Spec::File(b"now executable\n".to_vec(), true),
    );
    #[cfg(unix)]
    specs.insert("empty".into(), Spec::Link("README.md"));
    let v2 = world(&mut repo, &specs);
    let two = open(&mut repo, "two", v2);
    let base = two.base.as_ref().unwrap();
    assert_eq!(base.source, BaseSource::Advanced);
    assert_eq!(base.derived_from, Some(v1));
    assert_eq!(base.changed_paths, if cfg!(unix) { 5 } else { 4 });
    assert_eq!(tree(&two.root), expected(&specs));
    assert!(!two.root.join("src/lib").exists(), "emptied directories go");
    assert_eq!(repo.layer_workspace(&id("two")).unwrap().base_root, rolling);
    assert_eq!(base_entries(&repo, "two"), ["rolling"]);
    // A Layer cloned before the advance keeps its own World.
    assert_eq!(tree(&one.root), expected(&v1_specs));

    let twin = open(&mut repo, "twin", v2);
    assert_eq!(twin.base.as_ref().unwrap().source, BaseSource::Cached);

    // An older World rewinds its own clone; the rolling base stays at v2.
    let old = open(&mut repo, "old", v1);
    let base = old.base.as_ref().unwrap();
    assert_eq!(base.source, BaseSource::Derived);
    assert_eq!(base.derived_from, Some(v2));
    assert_eq!(base.changed_paths, if cfg!(unix) { 5 } else { 4 });
    assert!(base.clone.is_some());
    assert_eq!(tree(&old.root), expected(&v1_specs));
    assert_eq!(
        tree(&two.root),
        expected(&specs),
        "a rewind touches no other Layer"
    );
    let again = open(&mut repo, "again", v2);
    assert_eq!(again.base.as_ref().unwrap().source, BaseSource::Cached);
    assert_eq!(base_entries(&repo, "two"), ["rolling"]);

    // Many Worlds later the same single base advances again, with nothing kept per version.
    let mut head = v2;
    for n in 0..3 {
        specs.insert(format!("more/{n}"), file(format!("{n}").as_bytes()));
        head = world(&mut repo, &specs);
    }
    let latest = open(&mut repo, "latest", head);
    let base = latest.base.as_ref().unwrap();
    assert_eq!(base.source, BaseSource::Advanced);
    assert_eq!(base.derived_from, Some(v2));
    assert_eq!(tree(&latest.root), expected(&specs));
    assert_eq!(base_entries(&repo, "two"), ["rolling"]);
}

#[test]
fn an_older_layer_with_checkpoint_changes_applies_them_on_the_rewound_clone() {
    let dir = scratch();
    let mut repo = Repository::init(dir.path()).unwrap();
    let v1_specs = sample();
    let v1 = world(&mut repo, &v1_specs);
    let mut newer = v1_specs.clone();
    newer.insert("src/main.rs".into(), file(b"newer"));
    newer.insert("only/new".into(), file(b"n"));
    let v2 = world(&mut repo, &newer);
    open(&mut repo, "ahead", v2);

    let base = repo.layer_base(v1).unwrap();
    let source = repo.create_layer(&id("source"), &base).unwrap();
    let changes = SparseChanges::new([
        (
            RepoPath::parse("src/main.rs").unwrap(),
            Some(entry(&mut repo, &file(b"changed"))),
        ),
        (RepoPath::parse("src/lib/deep/mod.rs").unwrap(), None),
        (
            RepoPath::parse("src/lib/other/added.rs").unwrap(),
            Some(entry(&mut repo, &file(b"added"))),
        ),
    ])
    .unwrap();
    let head = match repo
        .capture_layer(&op("capture"), &source, &changes)
        .unwrap()
    {
        LayerUpdate::Applied {
            state: LayerState::Active { head },
        } => head,
        other => panic!("{other:?}"),
    };
    repo.clone_layer(&id("copy"), head.checkpoint()).unwrap();
    let report = repo.open_workspace(&id("copy")).unwrap();
    let base = report.base.as_ref().unwrap();
    assert_eq!(base.source, BaseSource::Derived);
    assert_eq!(base.derived_from, Some(v2));
    assert_eq!(report.applied_changes, 3);
    let mut want = v1_specs.clone();
    want.insert("src/main.rs".into(), file(b"changed"));
    want.remove("src/lib/deep/mod.rs");
    want.insert("src/lib/other/added.rs".into(), file(b"added"));
    assert_eq!(tree(&report.root), expected(&want));
    assert!(!report.root.join("src/lib/deep").exists());
    assert!(
        !report.root.join("only").exists(),
        "paths added after v1 are rewound"
    );
}

#[test]
fn layer_creates_wait_for_an_advance_and_stay_consistent() {
    let dir = scratch();
    let root = dir.path().canonicalize().unwrap();
    let mut repo = Repository::init(&root).unwrap();
    let mut specs = sample();
    let mut worlds = vec![(world(&mut repo, &specs), specs.clone())];
    for n in 0..4 {
        specs.insert(format!("gen/{n}"), file(&[b'a' + n as u8; 2048]));
        worlds.push((world(&mut repo, &specs), specs.clone()));
    }
    open(&mut repo, "seed", worlds[0].0);
    drop(repo);

    // An advance in progress holds the workspace lock, so every create queues behind it.
    let held = fs::OpenOptions::new()
        .write(true)
        .open(root.join(".javelin").join("workspace.lock"))
        .unwrap();
    held.lock().unwrap();
    let threads: Vec<_> = worlds
        .iter()
        .enumerate()
        .map(|(n, (version, want))| {
            let (root, version, want) = (root.clone(), *version, want.clone());
            std::thread::spawn(move || {
                let mut repo = Repository::open(&root).unwrap();
                let report = open(&mut repo, &format!("t{n}"), version);
                assert_eq!(tree(&report.root), expected(&want));
            })
        })
        .collect();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        threads.iter().all(|t| !t.is_finished()),
        "creates must wait for the advance"
    );
    held.unlock().unwrap();
    for thread in threads {
        thread.join().unwrap();
    }
    let names: Vec<_> = fs::read_dir(root.join(".javelin/bases"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["rolling"]);
}

/// Entries under `path`, including `path` itself.
fn count_nodes(path: &Path) -> usize {
    if !path.is_dir() {
        return 1;
    }
    1 + fs::read_dir(path)
        .unwrap()
        .map(|e| count_nodes(&e.unwrap().path()))
        .sum::<usize>()
}

/// Captures the Layer's directory and publishes it the way the CLI does.
fn publish(repo: &mut Repository, layer: &str, operation: &str) -> PublicationReceipt {
    repo.capture_workspace(&id(layer)).unwrap();
    let expected = head(repo, layer);
    repo.publish_workspace(
        &op(operation),
        &expected,
        &PublishAttribution::Human,
        |_, _| Ok(()),
    )
    .unwrap()
}

#[test]
fn a_published_layer_directory_is_reclaimed_after_publish() {
    let dir = scratch();
    let mut repo = Repository::init(dir.path()).unwrap();
    let v1 = world(&mut repo, &sample());
    let a = open(&mut repo, "a", v1);
    fs::write(a.root.join("README.md"), b"published\n").unwrap();
    let receipt = publish(&mut repo, "a", "publish-a");
    assert!(
        matches!(receipt.outcome, PublishOutcome::Published { .. }),
        "{receipt:?}"
    );
    assert!(!a.root.exists(), "the published directory is reclaimed");
    let store = dir.path().canonicalize().unwrap().join(".javelin");
    assert_eq!(fs::read_dir(store.join("trash")).unwrap().count(), 0);
    assert_eq!(
        repo.history().unwrap().last().unwrap().number.get(),
        v1.get() + 1
    );

    // The Layer stays Published with no directory, and its base is untouched.
    assert!(matches!(
        repo.layer_state(&id("a")).unwrap(),
        LayerState::Published { .. }
    ));
    assert!(repo.open_workspace(&id("a")).is_err());
    assert!(!repo.layer_workspace(&id("a")).unwrap().exists);
    assert_eq!(base_entries(&repo, "a"), ["rolling"]);

    // A retry replays the same receipt and has nothing left to reclaim.
    let again = repo
        .publish_workspace(
            &op("publish-a"),
            &receipt.expected,
            &PublishAttribution::Human,
            |_, _| panic!("a replay runs no guard"),
        )
        .unwrap();
    assert_eq!(again, receipt);
}

#[test]
fn a_crash_between_publish_and_reclaim_is_finished_by_the_next_command() {
    let dir = scratch();
    let mut repo = Repository::init(dir.path()).unwrap();
    let v1 = world(&mut repo, &sample());
    let a = open(&mut repo, "a", v1);
    fs::write(a.root.join("README.md"), b"published\n").unwrap();
    repo.capture_workspace(&id("a")).unwrap();
    let expected = head(&repo, "a");
    // The publish commits; the process dies before its reclaim step runs.
    let receipt = repo
        .publish_layer(
            &op("publish-a"),
            &expected,
            &PublishAttribution::Human,
            |_, _| Ok(()),
        )
        .unwrap();
    assert!(matches!(receipt.outcome, PublishOutcome::Published { .. }));
    assert!(a.root.exists(), "nothing was reclaimed before the crash");

    let recovery = repo.recover_workspaces().unwrap();
    assert_eq!(recovery.trashed, 1);
    assert!(!a.root.exists());
    let store = dir.path().canonicalize().unwrap().join(".javelin");
    assert_eq!(fs::read_dir(store.join("trash")).unwrap().count(), 0);
    assert!(matches!(
        repo.layer_state(&id("a")).unwrap(),
        LayerState::Published { .. }
    ));
}

#[test]
fn a_crash_mid_reclaim_is_finished_by_recovery() {
    let dir = scratch();
    let mut repo = Repository::init(dir.path()).unwrap();
    let v1 = world(&mut repo, &sample());
    let a = open(&mut repo, "a", v1);
    for n in 0..5000 {
        fs::write(a.root.join(format!("f{n}")), b"").unwrap();
    }
    let total = count_nodes(&a.root);
    let receipt = publish(&mut repo, "a", "publish-a");
    assert!(matches!(receipt.outcome, PublishOutcome::Published { .. }));

    // One command reclaims up to its budget of 4096 entries, leaving the rest in trash.
    let store = dir.path().canonicalize().unwrap().join(".javelin");
    let trashed: Vec<_> = fs::read_dir(store.join("trash"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(trashed.len(), 1);
    assert_eq!(count_nodes(&trashed[0]), total - 4096);

    // A killed reclaim had removed some files and nothing else.
    let files: Vec<_> = fs::read_dir(&trashed[0])
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().unwrap().is_file())
        .take(300)
        .collect();
    assert_eq!(files.len(), 300);
    for entry in files {
        fs::remove_file(entry.path()).unwrap();
    }
    let left = count_nodes(&trashed[0]);
    let recovery = repo.recover_workspaces().unwrap();
    assert_eq!(recovery.reclaimed, left);
    assert_eq!(recovery.remaining, 0);
    assert!(!trashed[0].exists());
    assert_eq!(fs::read_dir(store.join("trash")).unwrap().count(), 0);
    assert!(matches!(
        repo.layer_state(&id("a")).unwrap(),
        LayerState::Published { .. }
    ));
}

#[test]
fn a_crashed_advance_is_dropped_and_the_next_create_rebuilds_the_base() {
    let dir = scratch();
    let mut repo = Repository::init(dir.path()).unwrap();
    let mut specs = sample();
    let v1 = world(&mut repo, &specs);
    open(&mut repo, "one", v1);
    specs.insert("src/main.rs".into(), file(b"fn main() { v2() }\n"));
    let v2 = world(&mut repo, &specs);
    let store = dir.path().canonicalize().unwrap().join(".javelin");
    // The record says an advance to v2 started and never finished.
    fs::write(
        store.join("base.json"),
        format!(r#"{{"version":{},"advancing":{}}}"#, v1.get(), v2.get()),
    )
    .unwrap();

    let two = open(&mut repo, "two", v2);
    assert_eq!(two.recovery.trashed, 1, "the partial base is dropped");
    assert_eq!(two.base.as_ref().unwrap().source, BaseSource::Materialized);
    assert_eq!(tree(&two.root), expected(&specs));
    assert_eq!(base_entries(&repo, "two"), ["rolling"]);
    let again = open(&mut repo, "again", v2);
    assert_eq!(again.base.as_ref().unwrap().source, BaseSource::Cached);
}

/// Whether the volume under `dir` treats `a` and `b` as the same name.
fn folds(dir: &Path, a: &str, b: &str) -> bool {
    let probe = dir.join("probe");
    fs::create_dir_all(&probe).unwrap();
    fs::write(probe.join(a), b"").unwrap();
    let same = probe.join(b).exists();
    fs::remove_dir_all(&probe).unwrap();
    same
}

#[test]
fn unrepresentable_worlds_are_rejected_with_an_explicit_error() {
    let dir = scratch();
    let mut repo = Repository::init(dir.path()).unwrap();
    let cases: [(&str, &[&str], bool); 4] = [
        ("case", &["Foo", "foo"], folds(dir.path(), "Foo", "foo")),
        ("dirs", &["Dir/a", "dir/b"], folds(dir.path(), "Dir", "dir")),
        (
            "unicode",
            &["caf\u{e9}", "cafe\u{301}"],
            folds(dir.path(), "caf\u{e9}", "cafe\u{301}"),
        ),
        ("windows", &["CON", "a.", "ok"], cfg!(windows)),
    ];
    for (name, paths, rejected) in cases {
        let specs = paths
            .iter()
            .map(|p| (p.to_string(), file(p.as_bytes())))
            .collect();
        let version = world(&mut repo, &specs);
        let base = repo.layer_base(version).unwrap();
        repo.create_layer(&id(name), &base).unwrap();
        match repo.open_workspace(&id(name)) {
            Ok(report) => {
                assert!(!rejected, "{name} should be rejected");
                assert_eq!(tree(&report.root), expected(&specs));
            }
            Err(error) => {
                assert!(rejected, "{name}: {error}");
                assert_eq!(error.code(), "unsupported", "{error}");
                assert!(
                    error.to_string().contains("cannot be materialized"),
                    "{error}"
                );
                assert!(!repo.layer_workspace(&id(name)).unwrap().exists);
            }
        }
    }
    assert!(repo.recover_workspaces().unwrap().remaining == 0);
}

/// Free bytes on the volume holding `dir`, after flushing.
#[cfg(unix)]
fn free(dir: &Path) -> i64 {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path = CString::new(dir.as_os_str().as_bytes()).unwrap();
    let read = || {
        unsafe { libc::sync() };
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::statvfs(path.as_ptr(), &mut stat) }, 0);
        (stat.f_bavail as i64) * (stat.f_frsize as i64)
    };
    // Free space settles after delayed allocation; wait until two reads agree.
    let mut last = read();
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let now = read();
        if (now - last).abs() < 1 << 20 {
            return now;
        }
        last = now;
    }
    last
}

/// Bytes of the file's data shared with another file, or None where unmeasurable.
#[cfg(target_os = "macos")]
fn shared_bytes(path: &Path) -> Option<u64> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let c = CString::new(path.as_os_str().as_bytes()).unwrap();
    let mut list: libc::attrlist = unsafe { std::mem::zeroed() };
    list.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    list.forkattr = libc::ATTR_CMNEXT_PRIVATESIZE;
    let mut buf = [0u8; 16];
    let options = libc::FSOPT_NOFOLLOW | libc::FSOPT_ATTR_CMN_EXTENDED;
    let rc = unsafe {
        libc::getattrlist(
            c.as_ptr(),
            (&mut list as *mut libc::attrlist).cast(),
            buf.as_mut_ptr().cast(),
            16,
            options,
        )
    };
    assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
    let private = i64::from_ne_bytes(buf[4..12].try_into().unwrap()) as u64;
    Some(fs::metadata(path).unwrap().len().saturating_sub(private))
}

/// Sums FIEMAP extents flagged SHARED (btrfs snapshots and reflinks set it).
#[cfg(target_os = "linux")]
fn shared_bytes(path: &Path) -> Option<u64> {
    use std::os::fd::AsRawFd;
    const FS_IOC_FIEMAP: libc::Ioctl = libc::_IOWR::<[u64; 4]>(0x66, 11);
    const SHARED: u32 = 0x2000;
    const EXTENTS: usize = 512;
    let file = fs::File::open(path).unwrap();
    // struct fiemap header (32 bytes) followed by 56-byte struct fiemap_extent records.
    let mut buf = vec![0u64; 4 + EXTENTS * 7];
    buf[1] = u64::MAX; // fm_length
    buf[2] = 1; // fm_flags = FIEMAP_FLAG_SYNC; fm_mapped_extents is the high half
    buf[3] = EXTENTS as u64; // fm_extent_count
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), FS_IOC_FIEMAP, buf.as_mut_ptr()) };
    assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
    let mapped = ((buf[2] >> 32) as usize).min(EXTENTS);
    Some(
        (0..mapped)
            .map(|i| &buf[4 + i * 7..4 + i * 7 + 7])
            .filter(|e| (e[5] as u32) & SHARED != 0) // fe_flags
            .map(|e| e[2])
            .sum(),
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn shared_bytes(_: &Path) -> Option<u64> {
    None
}

#[test]
fn clone_strategies_share_unchanged_data() {
    let dir = scratch();
    let mut repo = Repository::init(dir.path()).unwrap();
    let mut bytes = vec![0u8; 8 << 20];
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for chunk in bytes.chunks_mut(8) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        chunk.copy_from_slice(&state.to_le_bytes());
    }
    let specs = BTreeMap::from([("big.bin".to_string(), file(&bytes))]);
    let v1 = world(&mut repo, &specs);
    let report = open(&mut repo, "layer", v1);
    let clone = report.clone.as_ref().unwrap();
    let path = report.root.join("big.bin");
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let Some(shared) = shared_bytes(&path) else {
        println!("{}: sharing not measurable here", clone.strategy.as_str());
        return;
    };
    println!(
        "{}: {shared} of {} bytes shared",
        clone.strategy.as_str(),
        bytes.len()
    );
    if clone.strategy == CloneStrategy::Copy {
        assert_eq!(shared, 0);
        assert_eq!(clone.copied_bytes, bytes.len() as u64);
    } else {
        assert_eq!(shared, bytes.len() as u64, "{clone:?}");
    }
    // Writing a Layer file unshares only that file's data; the base is untouched.
    fs::write(&path, b"private").unwrap();
    let other = open(&mut repo, "other", v1);
    assert_eq!(fs::read(other.root.join("big.bin")).unwrap(), bytes);
}

/// Measures cold base materialization separately from warm Layer creation.
/// `JAVELIN_WORKSPACE_BENCH_FILES` sets the size (default 100,000 files).
#[test]
#[ignore = "benchmark; run with --ignored --nocapture"]
fn workspace_bench() {
    let files: usize =
        std::env::var("JAVELIN_WORKSPACE_BENCH_FILES").map_or(100_000, |n| n.parse().unwrap());
    let dir = scratch();
    let mut repo = Repository::init(dir.path()).unwrap();
    let started = Instant::now();
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    // Bulk-load blobs in one unsynced transaction; put_blob syncs each call.
    let mut db =
        rusqlite::Connection::open(dir.path().join(".javelin/repository.sqlite3")).unwrap();
    db.execute_batch("PRAGMA synchronous=OFF").unwrap();
    let tx = db.transaction().unwrap();
    let mut entries = Vec::with_capacity(files);
    let mut total = 0;
    for i in 0..files {
        // Log-normal sizes: median about 3 KB with a long tail, as in tools/layer-bench.
        let u = ((next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64;
        let v = (next() >> 11) as f64 / (1u64 << 53) as f64;
        let z = (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos();
        let size = ((8.0 + 1.3 * z).exp() as usize).min(2_000_000);
        let bytes: Vec<u8> = (0..size.div_ceil(8))
            .flat_map(|_| next().to_le_bytes())
            .take(size)
            .collect();
        total += size;
        let content = javelin::ObjectId::for_blob(&bytes);
        tx.execute(
            "INSERT OR IGNORE INTO objects(id,kind,bytes) VALUES(?1,1,?2)",
            rusqlite::params![content.to_string(), bytes],
        )
        .unwrap();
        let path = format!("d{}/e{}/f{i}.txt", i / 2000, i / 20);
        entries.push((
            RepoPath::parse(&path).unwrap(),
            FileEntry::Regular {
                content,
                executable: false,
            },
        ));
    }
    tx.commit().unwrap();
    drop(db);
    let manifest = repo.put_manifest(&Manifest::new(entries).unwrap()).unwrap();
    repo.compare_and_advance(&op("bench"), VersionNumber::new(0).unwrap(), manifest)
        .unwrap();
    println!(
        "built World: {files} files, {} MB in {:?}",
        total >> 20,
        started.elapsed()
    );
    let v1 = VersionNumber::new(1).unwrap();
    let cold = open(&mut repo, "cold", v1);
    let base = cold.base.as_ref().unwrap();
    println!(
        "cold base: {:?} {} ms ({} MB written); first Layer {} ms via {}",
        base.source,
        base.elapsed_ms,
        base.written_bytes >> 20,
        cold.elapsed_ms,
        cold.clone.as_ref().unwrap().strategy.as_str()
    );
    for n in 0..3 {
        #[cfg(unix)]
        let before = free(dir.path());
        let started = Instant::now();
        let base = repo.layer_base(v1).unwrap();
        let layer = id(&format!("warm{n}"));
        repo.create_layer(&layer, &base).unwrap();
        let recorded = started.elapsed();
        let warm = repo.open_workspace(&layer).unwrap();
        let total = started.elapsed();
        println!(
            "  record Layer {recorded:?}; open_workspace {:?}",
            total - recorded
        );
        #[cfg(unix)]
        let used = (before - free(dir.path())) >> 20;
        #[cfg(not(unix))]
        let used = "n/a";
        let clone = warm.clone.unwrap();
        println!(
            "warm Layer {n}: clone+apply {} ms, whole call {total:?}, {} copied bytes, disk delta {used} MB via {}",
            warm.elapsed_ms,
            clone.copied_bytes,
            clone.strategy.as_str()
        );
    }
    let started = Instant::now();
    let expected = head(&repo, "warm0");
    repo.discard_workspace(&op("discard-warm0"), &expected)
        .unwrap();
    println!("discard (deferred): {:?}", started.elapsed());
    let started = Instant::now();
    repo.recover_workspaces().unwrap();
    println!("reclaim all trash: {:?}", started.elapsed());
}
