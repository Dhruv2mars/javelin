//! `javelin attach` against a loopback node: the head arrives whole, the rolling base is
//! warm, failures write nothing, and no Git runs.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

use javelin::{
    AdvanceOutcome, FileEntry, Manifest, ObjectId, OperationId, RepoPath, Repository, VersionNumber,
};
use javelin_node::{FileWorldLog, bind_loopback, serve};
use javelin_protocol::{
    Message,
    codec::{decode, encode},
};
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

/// Reads one HTTP request and returns its body.
fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
    }
    let text = String::from_utf8(head).unwrap();
    let length: usize = text
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    body
}

/// A node that runs `answer` on each request. Returns its URL.
fn fake_node(answer: impl Fn(&mut TcpStream, &[u8]) + Send + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let body = read_request(&mut stream);
            answer(&mut stream, &body);
        }
    });
    format!("http://{addr}/demo")
}

fn reply(stream: &mut TcpStream, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

/// Answers `Resolve` with World 1, then drops every other request without a reply.
fn resolve_then_drop(stream: &mut TcpStream, body: &[u8]) {
    if let Ok(Message::Resolve { .. }) = decode(body) {
        let resolved = Message::Resolved {
            number: v(1),
            id: ObjectId::for_blob(b"chain"),
            tree: ObjectId::for_blob(b"tree"),
        };
        reply(stream, &encode(&resolved).unwrap());
    }
}

/// Bytes the oversized node managed to send before the client hung up.
static SENT: AtomicUsize = AtomicUsize::new(0);

/// Answers every request with 64 MiB, far more than one frame. Stops when the client hangs up.
fn oversized(stream: &mut TcpStream, _body: &[u8]) {
    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n");
    let chunk = vec![0u8; 1 << 20];
    for _ in 0..64 {
        if stream.write_all(&chunk).is_err() {
            break;
        }
        SENT.fetch_add(chunk.len(), Ordering::SeqCst);
    }
}

#[test]
fn attach_writes_nothing_when_the_node_is_lost_after_resolve() {
    let url = fake_node(resolve_then_drop);
    let root = TempDir::new().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();

    let error = attach(root.path(), &url, 1);
    assert_eq!(error["error"]["code"], "node_unavailable");
    assert_eq!(std::fs::read_dir(&project).unwrap().count(), 0);
}

#[test]
fn attach_leaves_a_store_another_writer_created_alone() {
    let root = TempDir::new().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    // Another writer creates its store while the fetch runs, then the node is lost.
    let other = project.clone();
    let url = fake_node(move |stream, body| {
        if let Ok(Message::Resolve { .. }) = decode(body) {
            resolve_then_drop(stream, body);
        } else {
            std::fs::create_dir_all(other.join(".javelin")).unwrap();
            std::fs::write(other.join(".javelin/keep.txt"), b"theirs").unwrap();
        }
    });

    let error = attach(root.path(), &url, 1);
    assert_eq!(error["error"]["code"], "node_unavailable");
    assert_eq!(
        std::fs::read(project.join(".javelin/keep.txt")).unwrap(),
        b"theirs"
    );
    let names: Vec<_> = std::fs::read_dir(&project)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![".javelin"]);
}

#[test]
fn attach_refuses_a_reply_larger_than_one_frame() {
    let url = fake_node(oversized);
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("project")).unwrap();

    let error = attach(root.path(), &url, 1);
    assert_eq!(error["error"]["code"], "limit_exceeded");
    assert_eq!(
        std::fs::read_dir(root.path().join("project"))
            .unwrap()
            .count(),
        0
    );
    // The client read at most about one frame, so the node stopped well short of 64 MiB.
    assert!(
        SENT.load(Ordering::SeqCst) < 64 << 20,
        "sent {}",
        SENT.load(Ordering::SeqCst)
    );
}

#[test]
fn attach_refuses_matching_history_it_did_not_fetch() {
    let node = TempDir::new().unwrap();
    let url = start_node(node.path(), SMALL);
    let root = TempDir::new().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    for i in 0..SMALL {
        let dir = project.join(format!("d{:03}", i / 100));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("f{i:05}.txt")), b"shared").unwrap();
    }
    json(
        root.path(),
        &["adopt", "project", "--operation", "adopt-1"],
        0,
    );

    let error = attach(root.path(), &url, 2);
    assert_eq!(error["error"]["code"], "invalid_input");
    assert!(!project.join(".javelin/attach.json").exists());
}

#[test]
fn attach_resumes_an_interrupted_attach_after_the_remote_moved() {
    let first = TempDir::new().unwrap();
    let old_url = start_node(first.path(), SMALL);
    let second = TempDir::new().unwrap();
    let new_url = start_node(second.path(), LARGE);
    let root = TempDir::new().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    attach(root.path(), &old_url, 0);
    // Interrupted after the World advanced and before attach.json was written.
    std::fs::remove_file(project.join(".javelin/attach.json")).unwrap();

    let out = attach(root.path(), &new_url, 0);
    assert_eq!(out["head"]["number"], 1);
    let local = Repository::open(&project).unwrap();
    assert_eq!(
        local.manifest_at(v(2)).unwrap().entries(),
        manifest(LARGE).entries()
    );
    let attached = local.attachment().unwrap().unwrap();
    assert_eq!(attached.node, new_url.trim_end_matches("/demo"));
    assert_eq!(attached.head.number, 1);
}

#[test]
fn attach_refuses_an_incomplete_javelin_directory_before_fetching() {
    let node = TempDir::new().unwrap();
    let url = start_node(node.path(), SMALL);
    let root = TempDir::new().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join(".javelin")).unwrap();

    let error = attach(root.path(), &url, 2);
    assert_eq!(error["error"]["code"], "invalid_input");
    let names: Vec<_> = std::fs::read_dir(&project)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![".javelin"]);
}

#[test]
fn attach_sweeps_staging_an_earlier_attach_left_behind() {
    let node = TempDir::new().unwrap();
    let url = start_node(node.path(), SMALL);
    let root = TempDir::new().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let stale = project.join(".javelin-attach-4242-1");
    std::fs::create_dir_all(stale.join("bases")).unwrap();
    std::fs::write(stale.join("left.txt"), b"left").unwrap();

    attach(root.path(), &url, 0);
    let names: Vec<_> = std::fs::read_dir(&project)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![".javelin"]);
}
