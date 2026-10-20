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
        Self::spawn_with(root, mode, operation, manifest, expected, boundary, None)
    }

    fn spawn_with(
        root: &Path,
        mode: &str,
        operation: &str,
        manifest: Option<ObjectId>,
        expected: u64,
        boundary: &str,
        resolution: Option<ObjectId>,
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
            .env(
                "JAVELIN_TEST_RESOLUTION",
                resolution.map(|id| id.to_string()).unwrap_or_default(),
            )
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
    } else if let Some(layer) = mode.strip_prefix("publish:") {
        let mut repo = Repository::open(&root).unwrap();
        let layer = crate::LayerId::parse(layer).unwrap();
        let revision = std::env::var("JAVELIN_TEST_EXPECTED")
            .unwrap()
            .parse()
            .unwrap();
        let head = crate::layer::head_in(&repo.conn, &layer, revision).unwrap();
        assert_eq!(
            head.checkpoint().object().to_string(),
            std::env::var("JAVELIN_TEST_MANIFEST").unwrap()
        );
        let operation =
            OperationId::parse(&std::env::var("JAVELIN_TEST_OPERATION").unwrap()).unwrap();
        match repo.publish_layer(
            &operation,
            &head,
            &crate::PublishAttribution::Human,
            |_, _| {
                test_boundary("publish_guard");
                Ok(())
            },
        ) {
            Ok(receipt) => json!(receipt),
            Err(error) => json!({"error":error.code()}),
        }
    } else if let Some(layer) = mode.strip_prefix("stack:") {
        let mut repo = Repository::open(&root).unwrap();
        let layer = crate::LayerId::parse(layer).unwrap();
        let expected = std::env::var("JAVELIN_TEST_EXPECTED")
            .unwrap()
            .parse()
            .unwrap();
        let head = crate::layer::head_in(&repo.conn, &layer, expected).unwrap();
        let source =
            crate::CheckpointId::parse(&std::env::var("JAVELIN_TEST_MANIFEST").unwrap()).unwrap();
        let operation =
            OperationId::parse(&std::env::var("JAVELIN_TEST_OPERATION").unwrap()).unwrap();
        let checkpoint = repo.read_checkpoint(source).unwrap();
        let resolution = std::env::var("JAVELIN_TEST_RESOLUTION")
            .ok()
            .filter(|id| !id.is_empty())
            .map(|id| ObjectId::parse(&id).unwrap());
        match repo.commit_stack_resolved(
            &operation,
            &head,
            source,
            checkpoint.changes(),
            resolution,
        ) {
            Ok(outcome) => json!(outcome),
            Err(error) => json!({"error":error.code()}),
        }
    } else if let Some(layer) = mode.strip_prefix("publication_read:") {
        let repo = Repository::open(&root).unwrap();
        let layer = crate::LayerId::parse(layer).unwrap();
        let operation =
            OperationId::parse(&std::env::var("JAVELIN_TEST_OPERATION").unwrap()).unwrap();
        json!({"receipt":repo.publication(&operation).unwrap(),
            "state":repo.layer_state(&layer).unwrap(), "integrity":repo.check().unwrap(),
            "history":repo.history().unwrap()})
    } else if mode == "layer_capture" || mode == "layer_isolated" || mode == "layer_discard" {
        use crate::{LayerId, LayerState, SparseChanges};
        let mut repo = Repository::open(&root).unwrap();
        let operation =
            OperationId::parse(&std::env::var("JAVELIN_TEST_OPERATION").unwrap()).unwrap();
        let layer = LayerId::parse(if mode == "layer_isolated" {
            operation.as_str()
        } else {
            "shared"
        })
        .unwrap();
        let base = repo.layer_base(VersionNumber::new(0).unwrap()).unwrap();
        let initial = repo.create_layer(&layer, &base).unwrap();
        let expected: u64 = std::env::var("JAVELIN_TEST_EXPECTED")
            .unwrap()
            .parse()
            .unwrap();
        let head = if expected == 0 {
            initial
        } else {
            match repo.layer_state(&layer).unwrap() {
                LayerState::Active { head } => {
                    assert_eq!(head.revision(), expected);
                    head
                }
                _ => panic!("expected active Layer"),
            }
        };
        let result = if mode == "layer_discard" {
            repo.discard_layer(&operation, &head)
        } else {
            let content =
                ObjectId::parse(&std::env::var("JAVELIN_TEST_MANIFEST").unwrap()).unwrap();
            let changes = SparseChanges::new([(
                RepoPath::parse("binary").unwrap(),
                Some(FileEntry::Regular {
                    content,
                    executable: false,
                }),
            )])
            .unwrap();
            repo.capture_layer(&operation, &head, &changes)
        };
        match result {
            Ok(outcome) => json!(outcome),
            Err(error) => json!({"error": error.code()}),
        }
    } else if mode == "adopt" {
        let operation =
            OperationId::parse(&std::env::var("JAVELIN_TEST_OPERATION").unwrap()).unwrap();
        match Repository::adopt(&root, &operation) {
            Ok(adoption) => json!(adoption),
            Err(error) => json!({"error": error.code()}),
        }
    } else if mode == "layer_read" {
        let repo = Repository::open(&root).unwrap();
        let checkpoint =
            crate::CheckpointId::parse(&std::env::var("JAVELIN_TEST_MANIFEST").unwrap()).unwrap();
        let frozen = repo.read_checkpoint(checkpoint).unwrap();
        json!({"checkpoint":frozen.id(),"entries":repo.checkpoint_manifest(frozen.id()).unwrap().entries(),"changes":frozen.changes().entries().len(),"integrity":repo.check().unwrap()})
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
        FileEntry::Regular {
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
    assert_eq!(repo.check().unwrap().objects, 1); // the empty World tree
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
        assert_eq!(repo.check().unwrap().objects, 1); // the empty World tree
    }
}

fn write_project(root: &Path) {
    std::fs::create_dir_all(root.join("src/deep")).unwrap();
    std::fs::write(root.join("src/deep/a.txt"), "alpha\n").unwrap();
    std::fs::write(root.join("readme.md"), "# proj\n").unwrap();
}

#[test]
fn a_crashed_adoption_resumes_only_under_its_own_operation() {
    let crashed = TempDir::new().unwrap();
    write_project(crashed.path());
    let mut actor = Actor::spawn(
        crashed.path(),
        "adopt",
        "adopt-first",
        None,
        0,
        "adopt_after_capture",
    );
    actor.release();
    actor.marker("BOUNDARY adopt_after_capture");
    actor.kill();

    let mut other = Actor::spawn(crashed.path(), "adopt", "adopt-other", None, 0, "");
    other.release();
    let refused = other.result();
    assert!(refused.get("error").is_some(), "{refused}");

    let mut retry = Actor::spawn(crashed.path(), "adopt", "adopt-first", None, 0, "");
    retry.release();
    assert_eq!(retry.result()["files"], 2);
}

#[test]
fn adoption_retry_after_a_crash_yields_the_world_of_a_clean_adoption() {
    let operation = "adopt-first";
    for boundary in ["init_before_commit", "adopt_after_capture"] {
        let crashed = TempDir::new().unwrap();
        write_project(crashed.path());
        let mut actor = Actor::spawn(crashed.path(), "adopt", operation, None, 0, boundary);
        actor.release();
        actor.marker(&format!("BOUNDARY {boundary}"));
        actor.kill();

        let mut retry = Actor::spawn(crashed.path(), "adopt", operation, None, 0, "");
        retry.release();
        let adopted = retry.result();
        assert_eq!(adopted["files"], 2, "{boundary}: {adopted}");
        let repo = Repository::open(crashed.path()).unwrap();
        assert_eq!(repo.check().unwrap().versions, 2, "{boundary}");

        let clean = TempDir::new().unwrap();
        write_project(clean.path());
        let fresh =
            Repository::adopt(clean.path(), &OperationId::parse(operation).unwrap()).unwrap();
        assert_eq!(fresh.files, Some(2));
        let fresh_repo = Repository::open(clean.path()).unwrap();
        let number = VersionNumber::new(1).unwrap();
        assert_eq!(
            repo.version(number).unwrap().id,
            fresh_repo.version(number).unwrap().id,
            "{boundary}"
        );
    }
}

fn layer_fixture() -> (TempDir, ObjectId) {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let base = repo.layer_base(VersionNumber::new(0).unwrap()).unwrap();
    repo.create_layer(&crate::LayerId::parse("shared").unwrap(), &base)
        .unwrap();
    let content = repo.put_blob(b"private bytes").unwrap();
    (root, content)
}

#[test]
fn independent_layer_writers_have_one_winner_and_durable_stale_retries() {
    let (root, _) = layer_fixture();
    let mut repo = Repository::open(root.path()).unwrap();
    let content: Vec<_> = (0..8).map(|i| repo.put_blob(&[i, 255]).unwrap()).collect();
    drop(repo);
    let mut actors: Vec<_> = content
        .iter()
        .enumerate()
        .map(|(i, c)| {
            Actor::spawn(
                root.path(),
                "layer_capture",
                &format!("capture-{i}"),
                Some(*c),
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
        results.iter().filter(|r| r["outcome"] == "applied").count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|r| r["outcome"] == "stale" && r["state"]["head"]["revision"] == 1)
            .count(),
        7
    );
    for (i, c) in content.iter().enumerate() {
        let mut retry = Actor::spawn(
            root.path(),
            "layer_capture",
            &format!("capture-{i}"),
            Some(*c),
            0,
            "",
        );
        retry.release();
        assert_eq!(retry.result(), results[i]);
    }
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(repo.history().unwrap().len(), 1);
    repo.check().unwrap();
}

#[test]
fn simultaneous_layer_retries_append_once_and_input_reuse_is_rejected() {
    let (root, content) = layer_fixture();
    let mut actors: Vec<_> = (0..8)
        .map(|_| Actor::spawn(root.path(), "layer_capture", "same", Some(content), 0, ""))
        .collect();
    for actor in &mut actors {
        actor.release();
    }
    let results: Vec<_> = actors.iter_mut().map(Actor::result).collect();
    assert!(
        results
            .iter()
            .all(|r| r == &results[0] && r["outcome"] == "applied")
    );
    let mut repo = Repository::open(root.path()).unwrap();
    let different = repo.put_blob(b"different").unwrap();
    drop(repo);
    let mut retry = Actor::spawn(root.path(), "layer_capture", "same", Some(different), 0, "");
    retry.release();
    assert_eq!(retry.result()["error"], "operation_id_reused");
    let repo = Repository::open(root.path()).unwrap();
    match repo
        .layer_state(&crate::LayerId::parse("shared").unwrap())
        .unwrap()
    {
        crate::LayerState::Active { head } => assert_eq!(head.revision(), 1),
        _ => panic!("expected active head"),
    }
    repo.check().unwrap();
}

#[test]
fn simultaneous_independent_layer_heads_do_not_invalidate_each_other() {
    let (root, content) = layer_fixture();
    let mut actors: Vec<_> = (0..8)
        .map(|i| {
            Actor::spawn(
                root.path(),
                "layer_isolated",
                &format!("layer-{i}"),
                Some(content),
                0,
                "",
            )
        })
        .collect();
    for actor in &mut actors {
        actor.release();
    }
    let results: Vec<_> = actors.iter_mut().map(Actor::result).collect();
    assert!(
        results
            .iter()
            .all(|r| r["outcome"] == "applied" && r["state"]["head"]["revision"] == 1)
    );
    assert!(
        results
            .iter()
            .all(|r| r["state"]["head"]["checkpoint"] == results[0]["state"]["head"]["checkpoint"])
    );
    let repo = Repository::open(root.path()).unwrap();
    let report = repo.check().unwrap();
    assert_eq!(report.layers, 9);
    assert_eq!(report.objects, 4); // empty tree and checkpoint, blob, changed checkpoint
    assert_eq!(report.versions, 1);
}

#[test]
fn concurrent_discard_and_capture_choose_one_terminal_or_active_head() {
    let (root, content) = layer_fixture();
    let mut capture = Actor::spawn(
        root.path(),
        "layer_capture",
        "capture",
        Some(content),
        0,
        "",
    );
    let mut discard = Actor::spawn(root.path(), "layer_discard", "discard", None, 0, "");
    capture.release();
    discard.release();
    let captured = capture.result();
    let discarded = discard.result();
    assert_eq!(
        [&captured, &discarded]
            .iter()
            .filter(|r| r["outcome"] == "applied")
            .count(),
        1
    );
    assert_eq!(
        [&captured, &discarded]
            .iter()
            .filter(|r| r["outcome"] == "stale")
            .count(),
        1
    );
    assert_eq!(captured["state"], discarded["state"]);
    for (mode, operation, object, expected_result) in [
        ("layer_capture", "capture", Some(content), captured),
        ("layer_discard", "discard", None, discarded),
    ] {
        let mut retry = Actor::spawn(root.path(), mode, operation, object, 0, "");
        retry.release();
        assert_eq!(retry.result(), expected_result);
    }
    Repository::open(root.path()).unwrap().check().unwrap();
}

#[test]
fn layer_capture_killed_before_commit_has_no_partial_graph_and_retries() {
    let (root, content) = layer_fixture();
    let mut writer = Actor::spawn(
        root.path(),
        "layer_capture",
        "interrupted",
        Some(content),
        0,
        "layer_before_commit",
    );
    writer.release();
    writer.marker("BOUNDARY layer_before_commit");
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(repo.check().unwrap().objects, 3); // with the empty World tree
    assert_eq!(indexed(&repo), 0);
    match repo
        .layer_state(&crate::LayerId::parse("shared").unwrap())
        .unwrap()
    {
        crate::LayerState::Active { head } => assert_eq!(head.revision(), 0),
        _ => panic!("expected initial head"),
    }
    drop(repo);
    writer.kill();
    let mut retry = Actor::spawn(
        root.path(),
        "layer_capture",
        "interrupted",
        Some(content),
        0,
        "",
    );
    retry.release();
    assert_eq!(retry.result()["outcome"], "applied");
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(repo.check().unwrap().objects, 4);
    assert_eq!(indexed(&repo), 1);
}

#[test]
fn layer_lost_and_received_ack_survive_process_exit_and_later_heads() {
    for boundary in ["layer_after_commit", "after_ack"] {
        let (root, content) = layer_fixture();
        let mut writer = Actor::spawn(
            root.path(),
            "layer_capture",
            "original",
            Some(content),
            0,
            boundary,
        );
        writer.release();
        let acknowledged = if boundary == "after_ack" {
            Some(serde_json::from_str::<Value>(&writer.marker("RESULT ")).unwrap())
        } else {
            None
        };
        writer.marker(&format!("BOUNDARY {boundary}"));
        writer.kill();
        let mut repo = Repository::open(root.path()).unwrap();
        let later = repo.put_blob(b"later private bytes").unwrap();
        drop(repo);
        let mut next = Actor::spawn(root.path(), "layer_capture", "later", Some(later), 1, "");
        next.release();
        assert_eq!(next.result()["state"]["head"]["revision"], 2);
        let mut retry = Actor::spawn(
            root.path(),
            "layer_capture",
            "original",
            Some(content),
            0,
            "",
        );
        retry.release();
        let original = retry.result();
        assert_eq!(original["state"]["head"]["revision"], 1);
        if let Some(ack) = acknowledged {
            assert_eq!(original, ack);
        }
        let frozen =
            ObjectId::parse(original["state"]["head"]["checkpoint"].as_str().unwrap()).unwrap();
        let mut reader = Actor::spawn(root.path(), "layer_read", "", Some(frozen), 0, "");
        reader.release();
        let read = reader.result();
        assert_eq!(read["entries"]["binary"]["content"], content.to_string());
        assert_eq!(read["changes"], 1);
        assert_eq!(read["integrity"]["versions"], 1);
    }
}

/// Rows in the radar's live-Layer path index.
fn indexed(repo: &Repository) -> i64 {
    repo.conn
        .query_row("SELECT count(*) FROM layer_paths", [], |r| r.get(0))
        .unwrap()
}

/// Rows of World trace blame.
fn blamed(repo: &Repository) -> i64 {
    repo.conn
        .query_row("SELECT count(*) FROM blame", [], |r| r.get(0))
        .unwrap()
}

fn publish_fixture(overlap: bool, count: usize) -> (TempDir, Vec<crate::LayerHead>) {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let base = repo.layer_base(VersionNumber::new(0).unwrap()).unwrap();
    let mut heads = Vec::new();
    for i in 0..count {
        let layer = crate::LayerId::parse(&format!("publisher-{i}")).unwrap();
        let initial = repo.create_layer(&layer, &base).unwrap();
        let content = repo.put_blob(&[i as u8, 255]).unwrap();
        let path = if overlap {
            "binary".into()
        } else {
            format!("file-{i}")
        };
        let delta = crate::SparseChanges::new([(
            RepoPath::parse(&path).unwrap(),
            Some(FileEntry::Regular {
                content,
                executable: false,
            }),
        )])
        .unwrap();
        match repo
            .capture_layer(
                &OperationId::parse(&format!("capture-{i}")).unwrap(),
                &initial,
                &delta,
            )
            .unwrap()
        {
            crate::LayerUpdate::Applied {
                state: crate::LayerState::Active { head },
            } => heads.push(head),
            other => panic!("fixture capture failed: {other:?}"),
        }
    }
    (root, heads)
}

fn publisher(root: &Path, head: &crate::LayerHead, operation: &str, boundary: &str) -> Actor {
    Actor::spawn(
        root,
        &format!("publish:{}", head.layer().as_str()),
        operation,
        Some(head.checkpoint().object()),
        head.revision(),
        boundary,
    )
}

#[test]
fn whole_publish_disjoint_processes_all_compose_contiguously_and_overlap_has_one_winner() {
    for overlap in [false, true] {
        let (root, heads) = publish_fixture(overlap, 4);
        let mut actors: Vec<_> = heads
            .iter()
            .enumerate()
            .map(|(i, head)| publisher(root.path(), head, &format!("publish-{i}"), ""))
            .collect();
        for actor in &mut actors {
            actor.release();
        }
        let results: Vec<_> = actors.iter_mut().map(Actor::result).collect();
        assert!(
            results.iter().all(|r| r.get("error").is_none()),
            "{results:?}"
        );
        let winners = results
            .iter()
            .filter(|r| r["outcome"]["outcome"] == "published")
            .count();
        assert_eq!(winners, if overlap { 1 } else { 4 });
        assert_eq!(
            results
                .iter()
                .filter(|r| r["outcome"]["outcome"] == "conflict")
                .count(),
            if overlap { 3 } else { 0 }
        );
        let mut repo = Repository::open(root.path()).unwrap();
        let history = repo.history().unwrap();
        assert_eq!(
            history.iter().map(|w| w.number.get()).collect::<Vec<_>>(),
            (0..=winners as u64).collect::<Vec<_>>()
        );
        let final_manifest = repo.manifest_at(history.last().unwrap().number).unwrap();
        assert_eq!(final_manifest.entries().len(), winners);
        if !overlap {
            for (i, head) in heads.iter().enumerate() {
                assert_eq!(
                    final_manifest
                        .entries()
                        .get(&RepoPath::parse(&format!("file-{i}")).unwrap()),
                    repo.checkpoint_manifest(head.checkpoint())
                        .unwrap()
                        .entries()
                        .get(&RepoPath::parse(&format!("file-{i}")).unwrap())
                );
            }
        }
        let later = candidate(&mut repo, b"later World");
        repo.compare_and_advance(
            &OperationId::parse("later").unwrap(),
            history.last().unwrap().number,
            later,
        )
        .unwrap();
        drop(repo);
        for (i, head) in heads.iter().enumerate() {
            let mut retry = publisher(root.path(), head, &format!("publish-{i}"), "");
            retry.release();
            assert_eq!(retry.result(), results[i]);
        }
        Repository::open(root.path()).unwrap().check().unwrap();
    }
}

#[test]
fn whole_publish_world_advance_after_prepare_rebases_or_conflicts_at_the_cas() {
    for overlap in [false, true] {
        let (root, heads) = publish_fixture(overlap, 2);
        let mut slow = publisher(root.path(), &heads[0], "slow", "publish_prepared");
        slow.release();
        slow.marker("BOUNDARY publish_prepared");
        let mut fast = publisher(root.path(), &heads[1], "fast", "");
        fast.release();
        let fast = fast.result();
        assert_eq!(fast["outcome"]["version"]["number"], 1);
        slow.release();
        let slow = slow.result();
        // The slow plan composed against World 0; the short write transaction saw
        // World 1 and re-prepared instead of advancing from a stale head.
        assert_eq!(slow["against"]["number"], 1, "{slow}");
        if overlap {
            assert_eq!(slow["outcome"]["outcome"], "conflict");
        } else {
            assert_eq!(slow["outcome"]["version"]["number"], 2);
        }
        let repo = Repository::open(root.path()).unwrap();
        let history = repo.history().unwrap();
        assert_eq!(history.len(), if overlap { 2 } else { 3 });
        assert_eq!(
            repo.manifest_at(history.last().unwrap().number)
                .unwrap()
                .entries()
                .len(),
            if overlap { 1 } else { 2 }
        );
        repo.check().unwrap();
    }
}

#[test]
fn whole_publish_same_operation_process_retries_commit_one_receipt() {
    let (root, heads) = publish_fixture(false, 1);
    let mut actors: Vec<_> = (0..4)
        .map(|_| publisher(root.path(), &heads[0], "same", ""))
        .collect();
    for actor in &mut actors {
        actor.release();
    }
    let results: Vec<_> = actors.iter_mut().map(Actor::result).collect();
    assert!(
        results
            .iter()
            .all(|r| r == &results[0] && r["outcome"]["outcome"] == "published")
    );
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(repo.check().unwrap().versions, 2);
    assert_eq!(
        repo.conn
            .query_row("SELECT count(*) FROM publications", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn whole_publish_kill_before_commit_readers_see_no_partial_references() {
    let (root, heads) = publish_fixture(false, 1);
    let head = &heads[0];
    let before = Repository::open(root.path())
        .unwrap()
        .check()
        .unwrap()
        .objects;
    let mut writer = publisher(root.path(), head, "interrupted", "publish_before_commit");
    writer.release();
    writer.marker("BOUNDARY publish_before_commit");
    let mut reader = Actor::spawn(
        root.path(),
        "publication_read:publisher-0",
        "interrupted",
        None,
        0,
        "",
    );
    reader.release();
    let read = reader.result();
    assert!(read["receipt"].is_null());
    assert_eq!(read["state"]["state"], "active");
    assert_eq!(read["integrity"]["versions"], 1);
    assert_eq!(read["integrity"]["objects"], before);
    writer.kill();
    let repo = Repository::open(root.path()).unwrap();
    assert!(
        repo.publication(&OperationId::parse("interrupted").unwrap())
            .unwrap()
            .is_none()
    );
    assert_eq!(repo.check().unwrap().objects, before);
    assert_eq!(indexed(&repo), 1, "the Layer stays live in the radar");
    assert_eq!(blamed(&repo), 0);
    drop(repo);
    let mut retry = publisher(root.path(), head, "interrupted", "");
    retry.release();
    assert_eq!(retry.result()["outcome"]["outcome"], "published");
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(repo.check().unwrap().versions, 2);
    assert_eq!(indexed(&repo), 0, "Publish clears the Layer");
    assert_eq!(blamed(&repo), 1, "Publish attributes its one change");
    assert!(matches!(
        repo.layer_state(head.layer()).unwrap(),
        crate::LayerState::Published { .. }
    ));
}

#[test]
fn whole_publish_lost_and_received_ack_survive_kill_and_later_world() {
    for boundary in ["publish_after_commit", "after_ack"] {
        let (root, heads) = publish_fixture(false, 1);
        let head = &heads[0];
        let mut writer = publisher(root.path(), head, "durable", boundary);
        writer.release();
        let ack = if boundary == "after_ack" {
            Some(serde_json::from_str::<Value>(&writer.marker("RESULT ")).unwrap())
        } else {
            None
        };
        writer.marker(&format!("BOUNDARY {boundary}"));
        writer.kill();
        let mut reader = Actor::spawn(
            root.path(),
            "publication_read:publisher-0",
            "durable",
            None,
            0,
            "",
        );
        reader.release();
        let read = reader.result();
        assert_eq!(read["receipt"]["outcome"]["outcome"], "published");
        assert_eq!(read["state"]["state"], "published");
        assert_eq!(read["integrity"]["versions"], 2);
        assert_eq!(read["state"]["head"], read["receipt"]["expected"]);
        assert_eq!(read["history"][1], read["receipt"]["outcome"]["version"]);
        let mut repo = Repository::open(root.path()).unwrap();
        assert_eq!(indexed(&repo), 0, "the committed Publish cleared the Layer");
        assert_eq!(blamed(&repo), 1);
        let later = candidate(&mut repo, b"later");
        repo.compare_and_advance(
            &OperationId::parse("later").unwrap(),
            VersionNumber::new(1).unwrap(),
            later,
        )
        .unwrap();
        drop(repo);
        let mut retry = publisher(root.path(), head, "durable", "");
        retry.release();
        let receipt = retry.result();
        assert_eq!(receipt, read["receipt"]);
        if let Some(ack) = ack {
            assert_eq!(receipt, ack);
        }
        Repository::open(root.path()).unwrap().check().unwrap();
    }
}

#[test]
fn whole_publish_and_raw_same_operation_process_race_bind_exactly_one_api() {
    let (root, heads) = publish_fixture(false, 1);
    let head = &heads[0];
    let mut repo = Repository::open(root.path()).unwrap();
    let raw = candidate(&mut repo, b"raw");
    drop(repo);
    let mut publish = publisher(root.path(), head, "collision", "");
    let mut advance = Actor::spawn(root.path(), "advance", "collision", Some(raw), 0, "");
    publish.release();
    advance.release();
    let results = [publish.result(), advance.result()];
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
fn whole_publish_serializes_capture_and_discard_without_mutating_terminal_layer() {
    for mode in ["layer_capture", "layer_discard"] {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let base = repo.layer_base(VersionNumber::new(0).unwrap()).unwrap();
        let head = repo
            .create_layer(&crate::LayerId::parse("shared").unwrap(), &base)
            .unwrap();
        let content = repo.put_blob(b"capture").unwrap();
        drop(repo);
        let mut publish = publisher(root.path(), &head, "publish", "");
        let mut update = Actor::spawn(root.path(), mode, "update", Some(content), 0, "");
        publish.release();
        update.release();
        let published = publish.result();
        let updated = update.result();
        if published["outcome"]["outcome"] == "published" {
            assert_eq!(updated["error"], "layer_published");
        } else {
            assert_eq!(published["outcome"]["outcome"], "layer_stale");
            assert_eq!(updated["outcome"], "applied");
        }
        Repository::open(root.path()).unwrap().check().unwrap();
    }
}

fn stack_fixture() -> (TempDir, crate::LayerHead, crate::LayerHead) {
    let (root, content) = layer_fixture();
    let mut repo = Repository::open(root.path()).unwrap();
    let base = repo.layer_base(VersionNumber::new(0).unwrap()).unwrap();
    let target = repo
        .create_layer(&crate::LayerId::parse("shared").unwrap(), &base)
        .unwrap();
    let source = repo
        .create_layer(&crate::LayerId::parse("source").unwrap(), &base)
        .unwrap();
    let changes = crate::SparseChanges::new([(
        RepoPath::parse("binary").unwrap(),
        Some(FileEntry::Regular {
            content,
            executable: false,
        }),
    )])
    .unwrap();
    let crate::LayerUpdate::Applied {
        state: crate::LayerState::Active { head: source },
    } = repo
        .capture_layer(
            &OperationId::parse("source-capture").unwrap(),
            &source,
            &changes,
        )
        .unwrap()
    else {
        panic!("source capture")
    };
    (root, target, source)
}

fn stacker(root: &Path, source: &crate::LayerHead, boundary: &str) -> Actor {
    Actor::spawn(
        root,
        "stack:shared",
        "stack",
        Some(source.checkpoint().object()),
        0,
        boundary,
    )
}

fn resolved_stacker(
    root: &Path,
    source: &crate::LayerHead,
    resolution: ObjectId,
    boundary: &str,
) -> Actor {
    Actor::spawn_with(
        root,
        "stack:shared",
        "stack",
        Some(source.checkpoint().object()),
        0,
        boundary,
        Some(resolution),
    )
}

/// Writes the canonical resolution object a CLI Stack would name for one `ours` choice.
fn resolution_object(root: &Path) -> ObjectId {
    let mut repo = Repository::open(root).unwrap();
    repo.put_blob(br#"{"resolutions":[{"choice":"ours","path":"binary"}]}"#)
        .unwrap()
}

#[test]
fn stack_kill_before_commit_has_no_partial_checkpoint_ancestry_or_attempt() {
    let (root, target, source) = stack_fixture();
    let repo = Repository::open(root.path()).unwrap();
    let before = repo.check().unwrap().objects;
    drop(repo);
    let mut actor = stacker(root.path(), &source, "stack_before_commit");
    actor.release();
    actor.marker("BOUNDARY stack_before_commit");
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(repo.check().unwrap().objects, before);
    assert_eq!(
        repo.stack_operation(
            &OperationId::parse("stack").unwrap(),
            &target,
            source.checkpoint()
        )
        .unwrap(),
        None
    );
    assert_eq!(
        repo.layer_state(target.layer()).unwrap(),
        crate::LayerState::Active { head: target }
    );
    drop(repo);
    actor.kill();
    let mut retry = stacker(root.path(), &source, "");
    retry.release();
    assert_eq!(retry.result()["outcome"], "applied");
    Repository::open(root.path()).unwrap().check().unwrap();
}

#[test]
fn stack_lost_ack_survives_kill_source_discard_and_later_target_capture() {
    let (root, target, source) = stack_fixture();
    let mut actor = stacker(root.path(), &source, "stack_after_commit");
    actor.release();
    actor.marker("BOUNDARY stack_after_commit");
    actor.kill();
    let mut repo = Repository::open(root.path()).unwrap();
    let original = repo
        .stack_operation(
            &OperationId::parse("stack").unwrap(),
            &target,
            source.checkpoint(),
        )
        .unwrap()
        .unwrap();
    let crate::LayerUpdate::Applied {
        state: crate::LayerState::Active { head },
    } = &original
    else {
        panic!("{original:?}")
    };
    assert_eq!(
        repo.read_checkpoint(head.checkpoint())
            .unwrap()
            .stack_sources(),
        &[source.checkpoint()]
    );
    repo.discard_layer(&OperationId::parse("discard-source").unwrap(), &source)
        .unwrap();
    repo.capture_layer(
        &OperationId::parse("later").unwrap(),
        head,
        &crate::SparseChanges::new([]).unwrap(),
    )
    .unwrap();
    drop(repo);
    let mut retry = stacker(root.path(), &source, "");
    retry.release();
    assert_eq!(retry.result(), json!(original));
    Repository::open(root.path()).unwrap().check().unwrap();
}

#[test]
fn stack_and_concurrent_target_capture_have_one_winner_and_durable_stale_retry() {
    let (root, _, source) = stack_fixture();
    let mut repo = Repository::open(root.path()).unwrap();
    let content = repo.put_blob(b"concurrent owner edit").unwrap();
    drop(repo);
    let mut stack = stacker(root.path(), &source, "");
    let mut capture = Actor::spawn(
        root.path(),
        "layer_capture",
        "capture",
        Some(content),
        0,
        "",
    );
    stack.release();
    capture.release();
    let stacked = stack.result();
    let captured = capture.result();
    assert_eq!(
        [&stacked, &captured]
            .iter()
            .filter(|r| r["outcome"] == "applied")
            .count(),
        1
    );
    assert_eq!(
        [&stacked, &captured]
            .iter()
            .filter(|r| r["outcome"] == "stale")
            .count(),
        1
    );
    assert_eq!(stacked["state"], captured["state"]);
    let mut retry = stacker(root.path(), &source, "");
    retry.release();
    assert_eq!(retry.result(), stacked);
    let mut retry = Actor::spawn(
        root.path(),
        "layer_capture",
        "capture",
        Some(content),
        0,
        "",
    );
    retry.release();
    assert_eq!(retry.result(), captured);
    Repository::open(root.path()).unwrap().check().unwrap();
}

#[test]
fn simultaneous_stack_retries_append_one_checkpoint_with_one_source_binding() {
    let (root, target, source) = stack_fixture();
    let mut actors: Vec<_> = (0..4).map(|_| stacker(root.path(), &source, "")).collect();
    for actor in &mut actors {
        actor.release();
    }
    let results: Vec<_> = actors.iter_mut().map(Actor::result).collect();
    assert!(
        results
            .iter()
            .all(|r| r == &results[0] && r["outcome"] == "applied")
    );
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(repo.layer_history(target.layer()).unwrap().len(), 2);
    assert_eq!(
        repo.conn
            .query_row("SELECT count(*) FROM stacks", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    repo.check().unwrap();
}

#[test]
fn resolved_stack_killed_before_commit_leaves_no_attempt_and_retries_with_the_same_choice() {
    let (root, target, source) = stack_fixture();
    let resolution = resolution_object(root.path());
    let repo = Repository::open(root.path()).unwrap();
    let before = repo.check().unwrap().objects;
    drop(repo);
    let mut actor = resolved_stacker(root.path(), &source, resolution, "stack_before_commit");
    actor.release();
    actor.marker("BOUNDARY stack_before_commit");
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(repo.check().unwrap().objects, before);
    assert_eq!(
        repo.stack_operation_resolved(
            &OperationId::parse("stack").unwrap(),
            &target,
            source.checkpoint(),
            Some(resolution),
        )
        .unwrap(),
        None
    );
    drop(repo);
    actor.kill();
    let mut retry = resolved_stacker(root.path(), &source, resolution, "");
    retry.release();
    let applied = retry.result();
    assert_eq!(applied["outcome"], "applied");
    // The same choice replays the applied outcome; a dropped choice is a different input.
    let mut replay = resolved_stacker(root.path(), &source, resolution, "");
    replay.release();
    assert_eq!(replay.result(), applied);
    let mut other = stacker(root.path(), &source, "");
    other.release();
    assert_eq!(other.result()["error"], "operation_id_reused");
    Repository::open(root.path()).unwrap().check().unwrap();
}

#[test]
fn resolved_stack_lost_ack_replays_the_original_choice_after_a_crash() {
    let (root, _target, source) = stack_fixture();
    let resolution = resolution_object(root.path());
    let mut actor = resolved_stacker(root.path(), &source, resolution, "stack_after_commit");
    actor.release();
    actor.marker("BOUNDARY stack_after_commit");
    actor.kill();
    let mut retry = resolved_stacker(root.path(), &source, resolution, "");
    retry.release();
    let applied = retry.result();
    assert_eq!(applied["outcome"], "applied");
    let mut replay = resolved_stacker(root.path(), &source, resolution, "");
    replay.release();
    assert_eq!(replay.result(), applied);
    Repository::open(root.path()).unwrap().check().unwrap();
}
