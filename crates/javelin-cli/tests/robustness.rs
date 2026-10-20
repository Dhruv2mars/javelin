//! Real `javelin` processes: concurrent Publish, simulated power loss at named points
//! (`JAVELIN_FAILPOINT`, debug builds only) and lost-reply retries. docs/robustness.md
//! states the guarantees and how each test proves them.
use std::{
    fs,
    path::Path,
    process::{Child, Command, Output, Stdio},
};

use javelin::{RepoPath, Repository, VersionNumber};
use serde_json::{Value, json};
use tempfile::TempDir;

/// Exit status of a process stopped by a failpoint (`repository::failpoint`).
const CRASHED: i32 = 86;
/// Exit status of a Publish that lost to a conflicting Publish.
const CONFLICT: i32 = 3;

fn javelin(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_javelin"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}

/// Starts a process without waiting, so several can race on one repository.
fn spawn(root: &Path, args: &[&str]) -> Child {
    Command::new(env!("CARGO_BIN_EXE_javelin"))
        .current_dir(root)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// Runs `args` in a process that exits at `failpoint`. Exiting with `CRASHED` proves
/// the point was reached; any other status is a test failure.
fn crash_at(root: &Path, failpoint: &str, args: &[&str]) {
    let output = Command::new(env!("CARGO_BIN_EXE_javelin"))
        .current_dir(root)
        .env("JAVELIN_FAILPOINT", failpoint)
        .args(args)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(CRASHED),
        "{failpoint}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "{failpoint}: crashed process printed a result");
}

fn ok(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// `(exit status, parsed stdout)` for a spawned Publish.
fn finish(child: Child) -> (Option<i32>, Value) {
    let output = child.wait_with_output().unwrap();
    let value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
    (output.status.code(), value)
}

fn versions(root: &Path) -> Vec<u64> {
    Repository::open(root)
        .unwrap()
        .history()
        .unwrap()
        .iter()
        .map(|v| v.number.get())
        .collect()
}

fn valid(root: &Path) {
    Repository::open(root).unwrap().check().unwrap();
}

/// Reads the library directly: the CLI `layer history` captures the directory first,
/// which would record an extra revision and hide the crash result being tested.
fn layer_events(root: &Path, layer: &str) -> usize {
    let repository = Repository::open(root).unwrap();
    repository
        .layer_history(&javelin::LayerId::parse(layer).unwrap())
        .unwrap()
        .len()
}

fn layer_dir(root: &Path, layer: &str) -> std::path::PathBuf {
    root.join(".javelin/layers").join(layer)
}

/// An initialized repository with Layer `layer` created and holding one new file.
fn layer_with(root: &Path, layer: &str, name: &str, bytes: &[u8]) {
    ok(&javelin(root, &["init", "--json"]));
    ok(&javelin(root, &["workspace", "create", layer, "--json"]));
    fs::write(layer_dir(root, layer).join(name), bytes).unwrap();
}

fn publish(layer: &str, operation: &str) -> Vec<String> {
    ["workspace", "publish", layer, "--operation", operation, "--json"]
        .map(String::from)
        .to_vec()
}

fn args(list: &[String]) -> Vec<&str> {
    list.iter().map(String::as_str).collect()
}

#[test]
fn racing_publishers_of_one_path_have_one_winner_and_explicit_conflicts() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let layers: Vec<String> = (0..6).map(|i| format!("l{i}")).collect();
    layer_with(root, &layers[0], "shared.txt", b"0");
    for (i, layer) in layers.iter().enumerate().skip(1) {
        ok(&javelin(root, &["workspace", "create", layer, "--json"]));
        fs::write(layer_dir(root, layer).join("shared.txt"), format!("{i}")).unwrap();
    }
    let children: Vec<_> = layers
        .iter()
        .enumerate()
        .map(|(i, layer)| spawn(root, &args(&publish(layer, &format!("op-{i}")))))
        .collect();
    let results: Vec<_> = children.into_iter().map(finish).collect();
    let winners = results.iter().filter(|(code, _)| *code == Some(0)).count();
    let conflicts = results
        .iter()
        .filter(|(code, value)| {
            *code == Some(CONFLICT) && value["receipt"]["outcome"]["outcome"] == "conflict"
        })
        .count();
    assert_eq!(winners, 1, "{results:?}");
    assert_eq!(conflicts, layers.len() - 1, "{results:?}");
    assert_eq!(versions(root), vec![0, 1]);
    valid(root);
}

#[test]
fn racing_publishers_of_disjoint_paths_all_land_once_in_order() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    ok(&javelin(root, &["init", "--json"]));
    let layers: Vec<String> = (0..6).map(|i| format!("l{i}")).collect();
    for (i, layer) in layers.iter().enumerate() {
        ok(&javelin(root, &["workspace", "create", layer, "--json"]));
        fs::write(layer_dir(root, layer).join(format!("f{i}.txt")), format!("{i}")).unwrap();
    }
    let children: Vec<_> = layers
        .iter()
        .enumerate()
        .map(|(i, layer)| spawn(root, &args(&publish(layer, &format!("op-{i}")))))
        .collect();
    for child in children {
        let (code, value) = finish(child);
        assert_eq!(code, Some(0), "{value}");
    }
    assert_eq!(versions(root), (0..=6).collect::<Vec<_>>());
    let world = Repository::open(root)
        .unwrap()
        .manifest_at(VersionNumber::new(6).unwrap())
        .unwrap();
    for i in 0..6 {
        let path = RepoPath::parse(&format!("f{i}.txt")).unwrap();
        assert!(world.entries().contains_key(&path), "f{i}.txt was lost");
    }
    valid(root);
}

#[test]
fn racing_retries_of_one_operation_publish_once_with_identical_receipts() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    layer_with(root, "a", "x.txt", b"same");
    let children: Vec<_> = (0..6).map(|_| spawn(root, &args(&publish("a", "same")))).collect();
    let results: Vec<_> = children.into_iter().map(finish).collect();
    for (code, value) in &results {
        assert_eq!(*code, Some(0), "{value}");
        assert_eq!(value["receipt"], results[0].1["receipt"]);
    }
    assert_eq!(versions(root), vec![0, 1]);
    valid(root);
}

#[test]
fn layer_create_crash_points_leave_no_partial_layer_and_retries_are_idempotent() {
    for (point, committed) in [
        ("layer_create_before_commit", false),
        ("layer_create_after_commit", true),
    ] {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        ok(&javelin(root, &["init", "--json"]));
        crash_at(root, point, &["layer", "create", "a", "--json"]);
        let state = javelin(root, &["layer", "state", "a", "--json"]);
        assert_eq!(state.status.success(), committed, "{point}");
        ok(&javelin(root, &["layer", "create", "a", "--json"]));
        let state = ok(&javelin(root, &["layer", "state", "a", "--json"]));
        assert_eq!(state["state"]["head"]["revision"], 0, "{point}");
        assert_eq!(layer_events(root, "a"), 1, "{point}");
        assert_eq!(versions(root), vec![0], "{point}");
        valid(root);
    }
}

#[test]
fn workspace_open_crash_before_rename_leaves_no_directory_and_retry_opens() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    ok(&javelin(root, &["init", "--json"]));
    crash_at(
        root,
        "workspace_open_before_rename",
        &["workspace", "create", "a", "--json"],
    );
    assert!(!layer_dir(root, "a").exists());
    let retry = ok(&javelin(root, &["workspace", "open", "a", "--json"]));
    assert_eq!(retry["workspace"]["created"], true);
    assert!(layer_dir(root, "a").is_dir());
    let staging = root.join(".javelin/tmp");
    let leftovers = fs::read_dir(staging).map(|d| d.count()).unwrap_or(0);
    assert_eq!(leftovers, 0, "abandoned staging is swept by recovery");
    valid(root);
}

#[test]
fn capture_crash_points_never_leave_a_half_recorded_checkpoint() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    layer_with(root, "a", "x.txt", b"first");
    let checkpoint = ["checkpoint", "a", "--json"];
    crash_at(root, "layer_capture_before_commit", &checkpoint);
    assert_eq!(layer_events(root, "a"), 1);
    ok(&javelin(root, &checkpoint));
    assert_eq!(layer_events(root, "a"), 2);

    fs::write(layer_dir(root, "a").join("y.txt"), b"second").unwrap();
    crash_at(root, "layer_capture_after_commit", &checkpoint);
    assert_eq!(layer_events(root, "a"), 3, "the committed capture is recorded once");
    ok(&javelin(root, &checkpoint));
    assert_eq!(layer_events(root, "a"), 3, "a retried capture adds no revision");
    valid(root);
}

#[test]
fn publish_crash_before_commit_publishes_once_on_retry() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    layer_with(root, "a", "x.txt", b"x");
    crash_at(root, "publish_before_commit", &args(&publish("a", "p1")));
    assert_eq!(versions(root), vec![0]);
    let retry = ok(&javelin(root, &args(&publish("a", "p1"))));
    assert_eq!(retry["receipt"]["outcome"]["outcome"], "published");
    assert_eq!(versions(root), vec![0, 1]);
    valid(root);
}

#[test]
fn publish_lost_reply_retry_returns_the_same_version_and_never_applies_twice() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    layer_with(root, "a", "x.txt", b"x");
    crash_at(root, "publish_after_commit", &args(&publish("a", "p1")));
    assert_eq!(versions(root), vec![0, 1]);
    let history = Repository::open(root).unwrap().history().unwrap();
    let retry = ok(&javelin(root, &args(&publish("a", "p1"))));
    assert_eq!(retry["receipt"]["outcome"]["version"]["id"], json!(history[1].id.to_string()));
    assert_eq!(versions(root), vec![0, 1]);
    // The Layer is published, so a different operation cannot publish it again.
    let other = javelin(root, &args(&publish("a", "p2")));
    assert!(!other.status.success());
    assert_eq!(versions(root), vec![0, 1]);
    valid(root);
}

#[test]
fn layer_discard_crash_points_are_atomic_and_retries_end_discarded_once() {
    for (point, committed) in [
        ("layer_discard_before_commit", false),
        ("layer_discard_after_commit", true),
    ] {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        ok(&javelin(root, &["init", "--json"]));
        ok(&javelin(root, &["layer", "create", "a", "--json"]));
        let discard = ["layer", "discard", "a", "--revision", "0", "--operation", "d1", "--json"];
        crash_at(root, point, &discard);
        let state = ok(&javelin(root, &["layer", "state", "a", "--json"]));
        assert_eq!(state["state"]["state"] == "discarded", committed, "{point}");
        ok(&javelin(root, &discard));
        let state = ok(&javelin(root, &["layer", "state", "a", "--json"]));
        assert_eq!(state["state"]["state"], "discarded", "{point}");
        assert_eq!(layer_events(root, "a"), 2, "{point}");
        valid(root);
    }
}

#[test]
fn workspace_discard_crash_before_trash_is_finished_by_recovery_and_retry_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    layer_with(root, "a", "x.txt", b"x");
    let discard = ["workspace", "discard", "a", "--operation", "d1", "--json"];
    crash_at(root, "workspace_discard_before_trash", &discard);
    let state = ok(&javelin(root, &["layer", "state", "a", "--json"]));
    assert_eq!(state["state"]["state"], "discarded");
    assert!(layer_dir(root, "a").exists(), "the directory waits for recovery");
    let recovered = ok(&javelin(root, &["workspace", "recover", "--json"]));
    assert_eq!(recovered["recovery"]["trashed"], 1);
    assert!(!layer_dir(root, "a").exists());
    let retry = ok(&javelin(root, &discard));
    assert_eq!(retry["update"]["outcome"], "applied");
    assert_eq!(retry["trashed"], false);
    valid(root);
}

/// A writer holding the repository past the 5 second busy timeout gets `busy`, and the
/// refused Publish changes nothing: the retry after release publishes exactly once.
#[test]
fn lock_held_past_the_timeout_is_busy_and_retry_publishes_once() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    layer_with(root, "one", "a.txt", b"a");
    let holder = rusqlite::Connection::open(root.join(".javelin/repository.sqlite3")).unwrap();
    holder.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let command = publish("one", "publish-1");
    let refused = javelin(root, &args(&command));
    assert_eq!(refused.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&refused.stderr).unwrap();
    assert_eq!(error["error"]["code"], "busy", "{error}");
    holder.execute_batch("ROLLBACK").unwrap();
    drop(holder);
    assert_eq!(versions(root), [0]);
    ok(&javelin(root, &args(&command)));
    assert_eq!(versions(root), [0, 1]);
    valid(root);
}
