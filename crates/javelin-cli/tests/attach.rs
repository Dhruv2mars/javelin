//! `javelin attach` against a loopback node: the head arrives whole, the rolling base is
//! warm, failures write nothing, and no Git runs.

use std::{
    net::TcpListener,
    path::Path,
    process::{Command, Output},
    thread,
};

use javelin::{
    AdvanceOutcome, FileEntry, Manifest, ObjectId, OperationId, RepoPath, Repository, VersionNumber,
};
use javelin_node::{FileWorldLog, bind_loopback, serve};
use serde_json::Value;
use tempfile::TempDir;

/// Enough files that the tree spans several chunks at more than one level.
const LARGE: usize = 2_000;
const SMALL: usize = 10;

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}

fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}

/// Every file has the same content, so one Blob serves the whole World.
fn manifest(files: usize) -> Manifest {
    let content = ObjectId::for_blob(b"shared");
    Manifest::new((0..files).map(|i| {
        let path = RepoPath::parse(&format!("d{:03}/f{:05}.txt", i / 100, i)).unwrap();
        (
            path,
            FileEntry::Regular {
                content,
                executable: false,
            },
        )
    }))
    .unwrap()
}

/// Seeds a node at `dir` with World 1 holding `files` files, then serves it on loopback
/// as repository `demo`. The server thread runs until the test process exits.
fn start_node(dir: &Path, files: usize) -> String {
    let mut repo = Repository::init(dir).unwrap();
    repo.put_blob(b"shared").unwrap();
    let tree = repo.put_snapshot(&manifest(files)).unwrap();
    let outcome = repo.advance_to_tree(&op("seed-1"), v(0), tree).unwrap();
    assert!(matches!(outcome, AdvanceOutcome::Accepted { .. }));
    drop(repo);

    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let dir = dir.to_path_buf();
    thread::spawn(move || {
        let mut log = FileWorldLog::open(&dir).unwrap();
        serve(&listener, "demo", &mut log, None).unwrap();
    });
    format!("http://{addr}/demo")
}

/// A port with nothing listening on it.
fn closed_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}/demo")
}

fn javelin(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_javelin"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

/// Runs with `--json` and returns the structured output for the expected exit code.
fn json(cwd: &Path, args: &[&str], code: i32) -> Value {
    let mut args = args.to_vec();
    args.push("--json");
    let output = javelin(cwd, &args);
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    let bytes = if code == 0 {
        &output.stdout
    } else {
        &output.stderr
    };
    serde_json::from_slice(bytes).unwrap()
}

fn attach(root: &Path, url: &str, code: i32) -> Value {
    json(root, &["attach", url, "project"], code)
}

#[test]
fn attach_fetches_the_head_and_warms_the_base() {
    let node = TempDir::new().unwrap();
    let url = start_node(node.path(), LARGE);
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("project")).unwrap();

    let out = attach(root.path(), &url, 0);
    assert_eq!(out["attached"], true);
    assert_eq!(out["head"]["number"], 1);

    let project = root.path().join("project");
    let local = Repository::open(&project).unwrap();
    let remote = manifest(LARGE);
    assert_eq!(local.manifest_at(v(1)).unwrap().entries(), remote.entries());
    let attached = local.attachment().unwrap().unwrap();
    assert_eq!(attached.node, url.trim_end_matches("/demo"));
    assert_eq!(attached.repository, "demo");
    assert_eq!(attached.head.number, 1);

    // The base was warmed by attach, so a Layer clones it instead of materializing it.
    let created = json(&project, &["workspace", "create", "work"], 0);
    assert!(created.to_string().contains("\"cached\""), "{created}");
}

#[test]
fn attach_to_a_node_that_is_down_writes_nothing() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("project")).unwrap();
    let url = closed_url();

    let error = attach(root.path(), &url, 1);
    assert_eq!(error["error"]["code"], "node_unavailable");
    assert!(!root.path().join("project/.javelin").exists());
}

#[test]
fn attach_again_changes_nothing() {
    let node = TempDir::new().unwrap();
    let url = start_node(node.path(), SMALL);
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("project")).unwrap();

    let first = attach(root.path(), &url, 0);
    assert_eq!(first["attached"], true);
    let before = std::fs::read(root.path().join("project/.javelin/attach.json")).unwrap();

    let second = attach(root.path(), &url, 0);
    assert_eq!(second["attached"], false);
    assert_eq!(second["already_attached"], true);
    let after = std::fs::read(root.path().join("project/.javelin/attach.json")).unwrap();
    assert_eq!(before, after);
}

#[test]
fn attach_refuses_a_repository_with_its_own_world_history() {
    let node = TempDir::new().unwrap();
    let url = start_node(node.path(), SMALL);
    let root = TempDir::new().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("local.txt"), b"mine").unwrap();
    json(
        root.path(),
        &["adopt", "project", "--operation", "adopt-1"],
        0,
    );

    let error = attach(root.path(), &url, 2);
    assert_eq!(error["error"]["code"], "invalid_input");
    assert!(!project.join(".javelin/attach.json").exists());
    assert!(project.join("local.txt").exists());
}

#[cfg(unix)]
#[test]
fn attach_runs_without_git() {
    use std::os::unix::fs::PermissionsExt;

    let node = TempDir::new().unwrap();
    let url = start_node(node.path(), SMALL);
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("project")).unwrap();

    // A failing `git` first on PATH: any Git call would fail, and leave a marker behind.
    let bin = TempDir::new().unwrap();
    let marker = bin.path().join("git-ran");
    let git = bin.path().join("git");
    std::fs::write(
        &git,
        format!("#!/bin/sh\n: > '{}'\nexit 99\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Positive control: the stand-in really runs when something invokes `git`.
    let control = Command::new("git")
        .env("PATH", bin.path())
        .status()
        .unwrap();
    assert!(!control.success());
    assert!(marker.exists());
    std::fs::remove_file(&marker).unwrap();

    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_javelin"))
        .current_dir(root.path())
        .env("PATH", path)
        .args(["attach", &url, "project", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(!marker.exists(), "attach invoked git");
}
