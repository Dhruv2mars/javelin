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
