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

#[test]
fn radar_warns_at_checkpoint_and_clears_after_discard() {
    let root = TempDir::new().unwrap();
    let repo = root.path().to_str().unwrap();
    ok(root.path(), &["init", "--json"]);
    for (layer, file) in [
        ("a", "src/main.rs"),
        ("b", "src/main.rs"),
        ("c", "docs/x.md"),
    ] {
        ok(root.path(), &["layer", "create", layer, "--json"]);
        let dir = root.path().join(format!("dir-{layer}"));
        let file = dir.join(file);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, layer).unwrap();
        let path = dir.to_str().unwrap();
        let reply = ok(
            root.path(),
            &[
                "checkpoint",
                layer,
                "--path",
                path,
                "--repo",
                repo,
                "--json",
            ],
        );
        let expected: &[&str] = if layer == "b" { &["a"] } else { &[] };
        let warned: Vec<_> = reply["radar"]["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["layer"].as_str().unwrap())
            .collect();
        assert_eq!(warned, expected, "{reply}");
    }
    let radar = ok(root.path(), &["layer", "radar", "a", "--json"]);
    assert_eq!(radar["clear"], false);
    assert_eq!(radar["radar"]["world"]["paths"], serde_json::json!([]));
    assert_eq!(
        radar["radar"]["layers"],
        serde_json::json!([{"layer": "b", "paths": [{"ours": "src/main.rs", "theirs": "src/main.rs"}]}])
    );
    let text = invoke(root.path(), &["layer", "radar", "a"]);
    assert_eq!(text.status.code(), Some(0));
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(
        text.contains("Layer b if it publishes first: \"src/main.rs\""),
        "{text}"
    );

    ok(
        root.path(),
        &[
            "layer",
            "discard",
            "b",
            "--revision",
            "1",
            "--operation",
            "drop-b",
            "--json",
        ],
    );
    let radar = ok(root.path(), &["layer", "radar", "a", "--json"]);
    assert_eq!(radar["clear"], true);
    let refused = invoke(root.path(), &["layer", "radar", "b", "--json"]);
    assert_eq!(refused.status.code(), Some(2));
    ok(root.path(), &["check", "--json"]);
}

#[test]
fn checkpoint_records_harness_provenance_and_blame_reports_it() {
    use javelin::{LayerId, LayerState, OperationId, PublishAttribution, Repository};
    let root = TempDir::new().unwrap();
    let repo = root.path().to_str().unwrap();
    ok(root.path(), &["init", "--json"]);
    ok(root.path(), &["layer", "create", "work", "--json"]);
    let dir = root.path().join("dir");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("f"), "one\ntwo\n").unwrap();
    let path = dir.to_str().unwrap();
    // The harness sets the environment; a flag overrides one field.
    let output = Command::new(env!("CARGO_BIN_EXE_javelin"))
        .current_dir(root.path())
        .args(["checkpoint", "work", "--path", path, "--repo", repo])
        .args(["--session", "s-42", "--json"])
        .env("JAVELIN_AGENT", "claude-code/opus")
        .env("JAVELIN_SESSION", "ignored")
        .env("JAVELIN_REFERENCE", "tool-7")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");

    let mut native = Repository::open(root.path()).unwrap();
    let LayerState::Active { head } = native
        .layer_state(&LayerId::parse("work").unwrap())
        .unwrap()
    else {
        panic!()
    };
    native
        .publish_layer(
            &OperationId::parse("publish").unwrap(),
            &head,
            &PublishAttribution::Human,
            |_, _| Ok(()),
        )
        .unwrap();
    let blame = ok(root.path(), &["blame", "f", "--json"]);
    let origin = &blame["blame"]["origin"];
    assert_eq!(origin["version"]["number"], 1);
    assert_eq!(origin["layer"], "work");
    assert_eq!(
        origin["provenance"],
        serde_json::json!({"actor": "agent", "agent": "claude-code/opus", "session": "s-42", "reference": "tool-7"})
    );
    let lines = ok(root.path(), &["blame", "f", "--lines", "--json"]);
    assert_eq!(lines["lines"]["lines"][1]["text"], "two");
    let text = invoke(root.path(), &["blame", "f", "--lines"]);
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(
        text.contains("claude-code/opus session s-42 ref tool-7"),
        "{text}"
    );
    let text = String::from_utf8(invoke(root.path(), &["blame", "f"]).stdout).unwrap();
    assert!(
        text.starts_with("\"f\" last changed in World 1 Layer work"),
        "{text}"
    );
    let missing = ok(root.path(), &["blame", "nope", "--json"]);
    assert!(missing["blame"].is_null());
    let bad = invoke(
        root.path(),
        &[
            "checkpoint",
            "work",
            "--path",
            path,
            "--actor",
            "robot",
            "--json",
        ],
    );
    assert_eq!(bad.status.code(), Some(2));
}
