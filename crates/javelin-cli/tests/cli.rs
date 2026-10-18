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
    assert_eq!(check["integrity"]["objects"], 0);
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
    let mut repo = Repository::init(root.path()).unwrap();
    let blob = repo.put_blob(b"content").unwrap();
    let file = javelin::FileEntry::Regular {
        content: blob,
        executable: false,
    };
    let manifest = javelin::Manifest::new([(javelin::RepoPath::parse("file").unwrap(), file)]);
    let manifest = repo.put_manifest(&manifest.unwrap()).unwrap();
    let operation = javelin::OperationId::parse("first").unwrap();
    repo.compare_and_advance(&operation, VersionNumber::new(0).unwrap(), manifest)
        .unwrap();
    drop(repo);
    let db = rusqlite::Connection::open(root.path().join(".javelin/repository.sqlite3")).unwrap();
    db.execute_batch("DROP TRIGGER objects_no_UPDATE;").unwrap();
    db.execute(
        "UPDATE objects SET bytes=x'00' WHERE id=?1",
        [blob.to_string()],
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

#[cfg(unix)]
#[test]
fn explicit_project_directory_works_from_a_deleted_working_directory() {
    if let Some(root) = std::env::var_os("JAVELIN_TEST_EXPLICIT_ROOT") {
        let directory = TempDir::new().unwrap();
        std::env::set_current_dir(directory.path()).unwrap();
        std::fs::remove_dir(directory.path()).unwrap();
        for command in ["init", "history", "check"] {
            let result = Command::new(env!("CARGO_BIN_EXE_javelin"))
                .arg(command)
                .arg(&root)
                .arg("--json")
                .output()
                .unwrap();
            assert!(result.status.success(), "{command}: {:?}", result);
            let value: Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(value["ok"], true);
        }
        return;
    }
    // Only this isolated test process changes cwd; parallel parent tests are unaffected.
    let root = TempDir::new().unwrap();
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "explicit_project_directory_works_from_a_deleted_working_directory",
            "--nocapture",
        ])
        .env("JAVELIN_TEST_EXPLICIT_ROOT", root.path())
        .output()
        .unwrap();
    assert!(result.status.success(), "{:?}", result);
}
