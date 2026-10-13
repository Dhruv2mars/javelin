use std::process::{Command, Output};

use javelin::{Repository, VersionNumber};
use serde_json::Value;
use tempfile::TempDir;

fn cli(root: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_javelin"))
        .current_dir(root.path())
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn fresh_cli_init_history_and_integrity_are_useful_and_restart_safe() {
    let root = TempDir::new().unwrap();
    std::fs::write(root.path().join("binary"), [0, 255, 42]).unwrap();
    let first = cli(&root, &["init", "--json"]);
    assert!(first.status.success(), "{:?}", first);
    assert!(first.stderr.is_empty());
    let first_json: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first_json["initial"]["number"], 0);
    assert_eq!(first_json["tracked_files"], 0);
    assert_eq!(
        std::fs::read(root.path().join("binary")).unwrap(),
        [0, 255, 42]
    );
    assert_eq!(cli(&root, &["init", "--json"]).stdout, first.stdout);
    let history = cli(&root, &["history", "--json"]);
    assert!(history.status.success());
    let history: Value = serde_json::from_slice(&history.stdout).unwrap();
    assert_eq!(history["versions"].as_array().unwrap().len(), 1);
    assert_eq!(history["versions"][0], first_json["initial"]);
    let check = cli(&root, &["check", "--json"]);
    assert!(check.status.success());
    let check: Value = serde_json::from_slice(&check.stdout).unwrap();
    assert_eq!(check["integrity"]["objects"], 1);
    let plain = cli(&root, &["history"]);
    assert!(
        String::from_utf8(plain.stdout)
            .unwrap()
            .starts_with("World 0  ")
    );
    assert!(
        String::from_utf8(cli(&root, &["check"]).stdout)
            .unwrap()
            .contains("Integrity OK")
    );
}

#[test]
fn ordinary_and_structured_failures_have_exit_codes_and_no_success_output() {
    let root = TempDir::new().unwrap();
    let missing = cli(&root, &["history", "--json"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(missing.stdout.is_empty());
    let error: Value = serde_json::from_slice(&missing.stderr).unwrap();
    assert_eq!(error["ok"], false);
    assert_eq!(error["error"]["code"], "not_initialized");
    let plain = cli(&root, &["check"]);
    assert_eq!(plain.status.code(), Some(1));
    assert!(
        String::from_utf8(plain.stderr)
            .unwrap()
            .contains("not initialized")
    );
    for args in [
        vec!["unknown", "--json"],
        vec!["init", "--bad", "--json"],
        vec!["history", "a", "b", "--json"],
    ] {
        let result = cli(&root, &args);
        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
        let error: Value = serde_json::from_slice(&result.stderr).unwrap();
        assert_eq!(error["error"]["code"], "invalid_input");
    }
    assert!(!root.path().join(".javelin").exists());
}

#[test]
fn integrity_cli_rejects_corrupt_content() {
    let root = TempDir::new().unwrap();
    let repo = Repository::init(root.path()).unwrap();
    let initial = repo.version(VersionNumber::new(0).unwrap()).unwrap();
    drop(repo);
    let db = rusqlite::Connection::open(root.path().join(".javelin/repository.sqlite3")).unwrap();
    db.execute_batch("DROP TRIGGER objects_no_UPDATE;").unwrap();
    db.execute(
        "UPDATE objects SET bytes=x'00' WHERE id=?1",
        [initial.manifest.to_string()],
    )
    .unwrap();
    let result = cli(&root, &["check", "--json"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    let error: Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(error["error"]["code"], "corrupt_object");
    assert!(!cli(&root, &["history"]).status.success());
    assert!(!cli(&root, &["init"]).status.success());
}
