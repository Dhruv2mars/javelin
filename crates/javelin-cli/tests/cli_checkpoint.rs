use std::{
    path::Path,
    process::{Command, Output},
};

use serde_json::Value;
use tempfile::TempDir;

fn invoke(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_javelin"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

fn ok(cwd: &Path, args: &[&str]) -> Value {
    let output = invoke(cwd, args);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn checkpoint_captures_the_directory_once_and_is_a_no_op_until_it_changes() {
    let root = TempDir::new().unwrap();
    let repo = root.path().to_str().unwrap();
    ok(root.path(), &["init", "--json"]);
    ok(root.path(), &["layer", "create", "work", "--json"]);
    let dir = root.path().join("work");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();

    // Run from inside the Layer directory, as an agent lifecycle hook would.
    let args = ["checkpoint", "work", "--repo", repo, "--json"];
    let first = ok(&dir, &args);
    assert_eq!(first["ok"], true);
    assert_eq!(first["command"], "checkpoint");
    assert_eq!(first["update"]["outcome"], "applied");
    assert_eq!(first["capture"]["hashed"], 1);
    assert_eq!(first["capture"]["rebuilt"], true);
    let revision = first["update"]["state"]["head"]["revision"].clone();
    assert_eq!(revision, 1);

    let again = ok(&dir, &args);
    assert_eq!(again["unchanged"], true);
    assert_eq!(again["state"]["head"]["revision"], revision);
    let diff = ok(root.path(), &["layer", "diff", "work", "--json"]);
    assert!(diff.to_string().contains("src/main.rs"), "{diff}");

    std::fs::remove_file(dir.join("src/main.rs")).unwrap();
    let path = dir.to_str().unwrap();
    let text = invoke(root.path(), &["checkpoint", "work", "--path", path]);
    assert!(text.status.success(), "{text:?}");
    assert!(String::from_utf8(text.stdout).unwrap().contains("applied"));
    let state = ok(root.path(), &["layer", "state", "work", "--json"]);
    assert_eq!(state["state"]["head"]["revision"], 2);
}

#[test]
fn checkpoint_rejects_bad_grammar_without_mutation() {
    let root = TempDir::new().unwrap();
    ok(root.path(), &["init", "--json"]);
    ok(root.path(), &["layer", "create", "work", "--json"]);
    for args in [
        vec!["checkpoint", "--json"],
        vec!["checkpoint", "work", "extra", "--json"],
        vec!["checkpoint", "work", "--operation", "x", "--json"],
        vec!["checkpoint", "missing", "--json"],
    ] {
        let output = invoke(root.path(), &args);
        assert_ne!(output.status.code(), Some(0), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
    let state = ok(root.path(), &["layer", "state", "work", "--json"]);
    assert_eq!(state["state"]["head"]["revision"], 0);
}

fn fails(cwd: &Path, args: &[&str]) -> Value {
    let output = invoke(cwd, args);
    assert_ne!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    serde_json::from_slice(&output.stderr).unwrap()
}

fn workspace(root: &Path, id: &str) -> std::path::PathBuf {
    let opened = ok(root, &["workspace", "create", id, "--json"]);
    opened["workspace"]["root"].as_str().unwrap().into()
}

#[test]
fn layer_and_workspace_commands_capture_the_workspace_first() {
    let root = TempDir::new().unwrap();
    let repo = root.path().to_str().unwrap();
    ok(root.path(), &["init", "--json"]);
    let dir = workspace(root.path(), "work");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/a.txt"), "a\n").unwrap();

    let state = ok(root.path(), &["layer", "state", "work", "--json"]);
    assert_eq!(state["capture"]["update"]["outcome"], "applied", "{state}");
    assert_eq!(state["state"]["head"]["revision"], 1);
    let again = ok(root.path(), &["layer", "state", "work", "--json"]);
    assert_eq!(again["capture"]["update"], Value::Null);
    assert_eq!(again["state"]["head"]["revision"], 1);

    // From a subdirectory, checkpoint still captures the whole Layer directory.
    std::fs::write(dir.join("src/b.txt"), "b\n").unwrap();
    let hook = ok(
        &dir.join("src"),
        &["checkpoint", "work", "--repo", repo, "--json"],
    );
    assert_eq!(hook["update"]["outcome"], "applied", "{hook}");
    let diff = ok(root.path(), &["layer", "diff", "work", "--json"]);
    let paths: Vec<_> = diff["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["path"].as_str().unwrap(), c["after"].is_null()))
        .collect();
    assert_eq!(paths, [("src/a.txt", false), ("src/b.txt", false)]);

    // Discard records uncaptured edits first, so they stay in history.
    std::fs::write(dir.join("src/c.txt"), "c\n").unwrap();
    let discard = ok(
        root.path(),
        &[
            "workspace",
            "discard",
            "work",
            "--operation",
            "drop",
            "--json",
        ],
    );
    assert_eq!(discard["capture"]["update"]["outcome"], "applied");
    assert_eq!(discard["update"]["outcome"], "applied");
    let history = ok(root.path(), &["layer", "history", "work", "--json"]);
    assert_eq!(history["history"].as_array().unwrap().len(), 5, "{history}");
}

#[test]
fn a_stacked_head_is_never_reverted_by_capture() {
    let root = TempDir::new().unwrap();
    ok(root.path(), &["init", "--json"]);
    let target = workspace(root.path(), "target");
    let source = workspace(root.path(), "source");
    std::fs::write(source.join("from-source.txt"), "s\n").unwrap();
    let state = ok(root.path(), &["layer", "state", "source", "--json"]);
    let hash = state["state"]["head"]["checkpoint"]
        .as_str()
        .unwrap()
        .to_owned();
    let stack = [
        "layer",
        "stack",
        "target",
        "--revision",
        "0",
        "--source",
        &hash,
        "--operation",
        "s1",
        "--json",
    ];
    let stacked = ok(root.path(), &stack);
    assert_eq!(stacked["update"]["outcome"], "applied", "{stacked}");

    // The directory lacks the stacked file; capturing it would delete it.
    assert!(!target.join("from-source.txt").exists());
    let state = ok(root.path(), &["layer", "state", "target", "--json"]);
    assert_eq!(state["capture"]["behind"], true, "{state}");
    assert_eq!(state["state"]["head"]["revision"], 1);
    let error = fails(root.path(), &["checkpoint", "target", "--json"]);
    assert_eq!(error["error"]["code"], "invalid_input", "{error}");
    let diff = ok(root.path(), &["layer", "diff", "target", "--json"]);
    assert!(diff.to_string().contains("from-source.txt"), "{diff}");
}
