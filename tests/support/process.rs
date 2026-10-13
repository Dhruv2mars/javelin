//! Deterministic independent-process proof, compiled into the library test binary.
//! Boundary hooks do not exist in the production library or CLI.
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

use serde_json::{Value, json};
use tempfile::TempDir;

use super::*;
use crate::{FileEntry, RepoPath};

struct Actor {
    child: Child,
    lines: Receiver<String>,
}

impl Actor {
    fn spawn(
        root: &Path,
        mode: &str,
        operation: &str,
        manifest: Option<ObjectId>,
        expected: u64,
        boundary: &str,
    ) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "repository::process_tests::actor",
                "--ignored",
                "--nocapture",
            ])
            .env("JAVELIN_TEST_ROOT", root)
            .env("JAVELIN_TEST_MODE", mode)
            .env("JAVELIN_TEST_OPERATION", operation)
            .env(
                "JAVELIN_TEST_MANIFEST",
                manifest.map(|id| id.to_string()).unwrap_or_default(),
            )
            .env("JAVELIN_TEST_EXPECTED", expected.to_string())
            .env("JAVELIN_TEST_BOUNDARY", boundary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let (send, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if send.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let mut actor = Self { child, lines };
        actor.marker("READY");
        actor
    }
    fn marker(&mut self, prefix: &str) -> String {
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(15))
                .expect("child did not reach barrier");
            if let Some(rest) = line.strip_prefix(prefix) {
                return rest.trim().into();
            }
        }
    }
    fn release(&mut self) {
        self.child.stdin.as_mut().unwrap().write_all(b"x").unwrap();
    }
    fn result(&mut self) -> Value {
        let result = serde_json::from_str(&self.marker("RESULT ")).unwrap();
        assert!(self.child.wait().unwrap().success());
        result
    }
    fn kill(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

impl Drop for Actor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "child process entry point, run by process verification tests"]
fn actor() {
    let Ok(root) = std::env::var("JAVELIN_TEST_ROOT") else {
        return;
    };
    println!("READY");
    std::io::stdout().flush().unwrap();
    std::io::stdin().read_exact(&mut [0]).unwrap();
    let mode = std::env::var("JAVELIN_TEST_MODE").unwrap();
    let result = if mode == "init" {
        let repo = Repository::init(&root).unwrap();
        json!({"initial": repo.version(VersionNumber::new(0).unwrap()).unwrap()})
    } else if mode == "check" {
        let repo = Repository::open(&root).unwrap();
        json!({"integrity": repo.check().unwrap(), "history": repo.history().unwrap()})
    } else {
        let mut repo = Repository::open(&root).unwrap();
        let operation =
            OperationId::parse(&std::env::var("JAVELIN_TEST_OPERATION").unwrap()).unwrap();
        let expected = VersionNumber::new(
            std::env::var("JAVELIN_TEST_EXPECTED")
                .unwrap()
                .parse()
                .unwrap(),
        )
        .unwrap();
        let manifest = ObjectId::parse(&std::env::var("JAVELIN_TEST_MANIFEST").unwrap()).unwrap();
        match repo.compare_and_advance(&operation, expected, manifest) {
            Ok(outcome) => json!(outcome),
            Err(error) => json!({"error": error.code()}),
        }
    };
    println!("RESULT {result}");
    std::io::stdout().flush().unwrap();
    test_boundary("after_ack");
}

fn candidate(repo: &mut Repository, bytes: &[u8]) -> ObjectId {
    let content = repo.put_blob(bytes).unwrap();
    let manifest = Manifest::new([(
        RepoPath::parse("binary").unwrap(),
        FileEntry {
            content,
            executable: false,
        },
    )])
    .unwrap();
    repo.put_manifest(&manifest).unwrap()
}

#[test]
fn concurrent_initializers_share_one_immutable_world() {
    let root = TempDir::new().unwrap();
    std::fs::write(root.path().join("existing"), [0, 255]).unwrap();
    let mut actors: Vec<_> = (0..8)
        .map(|_| Actor::spawn(root.path(), "init", "", None, 0, ""))
        .collect();
    for actor in &mut actors {
        actor.release();
    }
    let results: Vec<_> = actors.iter_mut().map(Actor::result).collect();
    assert!(results.iter().all(|r| r == &results[0]));
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(repo.check().unwrap().versions, 1);
    assert_eq!(repo.check().unwrap().objects, 1);
    assert_eq!(
        std::fs::read(root.path().join("existing")).unwrap(),
        [0, 255]
    );
}

#[test]
fn independent_publishers_have_one_winner_and_durable_losers() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let manifests: Vec<_> = (0..8).map(|i| candidate(&mut repo, &[0, 255, i])).collect();
    drop(repo);
    let mut actors: Vec<_> = manifests
        .iter()
        .enumerate()
        .map(|(i, m)| {
            Actor::spawn(
                root.path(),
                "advance",
                &format!("operation-{i}"),
                Some(*m),
                0,
                "",
            )
        })
        .collect();
    for actor in &mut actors {
        actor.release();
    }
    let results: Vec<_> = actors.iter_mut().map(Actor::result).collect();
    assert_eq!(
        results
            .iter()
            .filter(|r| r["outcome"] == "accepted")
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|r| r["outcome"] == "stale" && r["actual"] == 1)
            .count(),
        7
    );
    let mut repo = Repository::open(root.path()).unwrap();
    let report = repo.check().unwrap();
    assert_eq!((report.versions, report.attempts), (2, 8));
    for (i, manifest) in manifests.iter().enumerate() {
        let retried = repo
            .compare_and_advance(
                &OperationId::parse(&format!("operation-{i}")).unwrap(),
                VersionNumber::new(0).unwrap(),
                *manifest,
            )
            .unwrap();
        assert_eq!(json!(retried), results[i]);
    }
}

#[test]
fn simultaneous_same_identity_retries_append_once() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let manifest = candidate(&mut repo, &[255, 0, 42]);
    drop(repo);
    let mut actors: Vec<_> = (0..8)
        .map(|_| Actor::spawn(root.path(), "advance", "same", Some(manifest), 0, ""))
        .collect();
    for actor in &mut actors {
        actor.release();
    }
    let results: Vec<_> = actors.iter_mut().map(Actor::result).collect();
    assert!(
        results
            .iter()
            .all(|r| r == &results[0] && r["outcome"] == "accepted")
    );
    let report = Repository::open(root.path()).unwrap().check().unwrap();
    assert_eq!((report.versions, report.attempts), (2, 1));
}

#[test]
fn simultaneous_different_input_with_same_identity_is_rejected() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let a = candidate(&mut repo, b"a");
    let b = candidate(&mut repo, b"b");
    drop(repo);
    let mut first = Actor::spawn(root.path(), "advance", "same", Some(a), 0, "");
    let mut second = Actor::spawn(root.path(), "advance", "same", Some(b), 0, "");
    first.release();
    second.release();
    let results = [first.result(), second.result()];
    assert_eq!(
        results
            .iter()
            .filter(|r| r["outcome"] == "accepted")
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|r| r["error"] == "operation_id_reused")
            .count(),
        1
    );
    assert_eq!(
        Repository::open(root.path())
            .unwrap()
            .check()
            .unwrap()
            .versions,
        2
    );
}

#[test]
fn killed_before_commit_exposes_no_partial_references_and_retries() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let manifest = candidate(&mut repo, b"prepared");
    drop(repo);
    let mut actor = Actor::spawn(
        root.path(),
        "advance",
        "interrupted",
        Some(manifest),
        0,
        "before_commit",
    );
    actor.release();
    actor.marker("BOUNDARY before_commit");
    let repo = Repository::open(root.path()).unwrap();
    let report = repo.check().unwrap();
    assert_eq!((report.versions, report.attempts), (1, 0));
    drop(repo);
    actor.kill();
    let mut retry = Actor::spawn(root.path(), "advance", "interrupted", Some(manifest), 0, "");
    retry.release();
    assert_eq!(retry.result()["outcome"], "accepted");
    assert_eq!(
        Repository::open(root.path())
            .unwrap()
            .check()
            .unwrap()
            .versions,
        2
    );
}

#[test]
fn killed_after_commit_lost_ack_survives_later_advance_and_process_retry() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let manifest = candidate(&mut repo, b"durable");
    drop(repo);
    let mut actor = Actor::spawn(
        root.path(),
        "advance",
        "lost-ack",
        Some(manifest),
        0,
        "after_commit",
    );
    actor.release();
    actor.marker("BOUNDARY after_commit");
    actor.kill();
    let mut later = Actor::spawn(root.path(), "advance", "later", Some(manifest), 1, "");
    later.release();
    assert_eq!(later.result()["version"]["number"], 2);
    let mut retry = Actor::spawn(root.path(), "advance", "lost-ack", Some(manifest), 0, "");
    retry.release();
    let outcome = retry.result();
    assert_eq!(outcome["version"]["number"], 1);
    assert_eq!(outcome["version"]["operation"], "lost-ack");
    let mut reader = Actor::spawn(root.path(), "check", "", None, 0, "");
    reader.release();
    let result = reader.result();
    assert_eq!(result["integrity"]["versions"], 3);
    assert_eq!(result["history"][0]["number"], 0);
    assert_eq!(result["history"][2]["number"], 2);
}

#[test]
fn acknowledged_result_survives_abrupt_process_exit() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let manifest = candidate(&mut repo, b"acknowledged");
    drop(repo);
    let mut actor = Actor::spawn(
        root.path(),
        "advance",
        "ack",
        Some(manifest),
        0,
        "after_ack",
    );
    actor.release();
    let result: Value = serde_json::from_str(&actor.marker("RESULT ")).unwrap();
    assert_eq!(result["version"]["number"], 1);
    actor.marker("BOUNDARY after_ack");
    actor.kill();
    let mut retry = Actor::spawn(root.path(), "advance", "ack", Some(manifest), 0, "");
    retry.release();
    assert_eq!(retry.result(), result);
    assert_eq!(
        Repository::open(root.path())
            .unwrap()
            .history()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn initialization_recovers_on_both_sides_of_commit() {
    for boundary in ["init_before_commit", "init_after_commit"] {
        let root = TempDir::new().unwrap();
        let mut actor = Actor::spawn(root.path(), "init", "", None, 0, boundary);
        actor.release();
        actor.marker(&format!("BOUNDARY {boundary}"));
        actor.kill();
        let mut retry = Actor::spawn(root.path(), "init", "", None, 0, "");
        retry.release();
        assert_eq!(retry.result()["initial"]["number"], 0);
        let repo = Repository::open(root.path()).unwrap();
        assert_eq!(repo.check().unwrap().versions, 1);
        assert_eq!(repo.check().unwrap().objects, 1);
    }
}
