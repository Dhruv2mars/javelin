use std::process::{Command, Output};

use javelin::{OperationId, Repository, outbox::Kind, outbox::Outbox};
use serde_json::Value;
use tempfile::TempDir;

fn status(root: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_javelin"))
        .current_dir(root.path())
        .arg("outbox")
        .arg("status")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn outbox_status_lists_queued_records_as_json_and_text() {
    let root = TempDir::new().unwrap();
    Repository::init(root.path()).unwrap();
    let mut outbox = Outbox::open(root.path(), "demo").unwrap();
    outbox
        .enqueue(
            Kind::Publish,
            "work",
            &OperationId::parse("publish-1").unwrap(),
            b"one",
        )
        .unwrap();
    outbox
        .enqueue(
            Kind::Revise,
            "review",
            &OperationId::parse("revise-1").unwrap(),
            b"two",
        )
        .unwrap();
    drop(outbox);

    let repo = root.path().to_str().unwrap();
    let json = status(&root, &["--remote", "demo", "--repo", repo, "--json"]);
    assert!(json.status.success(), "{json:?}");
    let value: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["remote"], "demo");
    assert_eq!(value["pending"].as_array().unwrap().len(), 2);
    assert_eq!(value["pending"][0]["kind"], "publish");
    assert_eq!(value["pending"][0]["scope"], "work");
    assert_eq!(value["pending"][1]["operation"], "revise-1");
    assert!(value["damage"].is_null());

    let text = status(&root, &["--remote", "demo", "--repo", repo]);
    assert!(text.status.success(), "{text:?}");
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.starts_with("2 pending in outbox demo"), "{text}");
    assert!(text.contains("seq 2 revise review revise-1"), "{text}");
}

#[test]
fn outbox_status_needs_a_remote_and_a_repository_and_creates_nothing() {
    let root = TempDir::new().unwrap();
    let repo = root.path().to_str().unwrap();
    assert!(!status(&root, &["--repo", repo]).status.success());

    // Not initialized: refused, and the status command leaves no journal behind.
    let output = status(&root, &["--remote", "demo", "--repo", repo]);
    assert!(!output.status.success());
    assert!(!root.path().join(".javelin").exists());
}
