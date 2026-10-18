//! `javelin workspace`: clone-backed Layer directories.
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use javelin::{FileEntry, Manifest, OperationId, RepoPath, Repository, VersionNumber};
use serde_json::Value;
use tempfile::TempDir;

fn scratch() -> TempDir {
    match std::env::var_os("JAVELIN_WORKSPACE_TEST_DIR") {
        Some(dir) => {
            fs::create_dir_all(&dir).unwrap();
            TempDir::new_in(dir).unwrap()
        }
        None => TempDir::new().unwrap(),
    }
}

fn invoke(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_javelin"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}

fn json(root: &Path, args: &[&str], code: i32) -> Value {
    let mut args = args.to_vec();
    args.push("--json");
    let output = invoke(root, &args);
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    serde_json::from_slice(if code == 0 || code == 3 {
        &output.stdout
    } else {
        &output.stderr
    })
    .unwrap()
}

#[test]
fn workspace_commands_open_report_and_discard_layer_directories() {
    let root = scratch();
    let mut repo = Repository::init(root.path()).unwrap();
    let content = repo.put_blob(b"hello\n").unwrap();
    let path = RepoPath::parse("docs/readme.txt").unwrap();
    let entry = FileEntry::Regular {
        content,
        executable: false,
    };
    let manifest = repo
        .put_manifest(&Manifest::new([(path, entry)]).unwrap())
        .unwrap();
    let operation = OperationId::parse("world-1").unwrap();
    repo.compare_and_advance(&operation, VersionNumber::new(0).unwrap(), manifest)
        .unwrap();
    drop(repo);

    let created = json(root.path(), &["workspace", "create", "work"], 0);
    let workspace = &created["workspace"];
    assert_eq!(workspace["created"], true);
    assert_eq!(workspace["base"]["version"], 1);
    assert_eq!(workspace["base"]["source"], "materialized");
    let strategy = workspace["clone"]["strategy"].as_str().unwrap().to_owned();
    if let Ok(expected) = std::env::var("JAVELIN_WORKSPACE_EXPECT") {
        assert_eq!(strategy, expected);
    }
    assert!(workspace["clone"]["copied_bytes"].is_u64());
    let dir = Path::new(workspace["root"].as_str().unwrap()).to_path_buf();
    assert_eq!(fs::read(dir.join("docs/readme.txt")).unwrap(), b"hello\n");
    fs::write(dir.join("docs/readme.txt"), b"edited\n").unwrap();

    // Text output names the strategy, and a copy says so.
    let text = invoke(root.path(), &["workspace", "create", "other"]);
    assert!(text.status.success(), "{text:?}");
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains(&format!("Strategy {strategy}")), "{text}");
    assert_eq!(
        text.contains("Warning: this volume cannot clone"),
        strategy == "copy"
    );

    let reopened = json(root.path(), &["workspace", "open", "work"], 0);
    assert_eq!(reopened["workspace"]["created"], false);
    assert_eq!(fs::read(dir.join("docs/readme.txt")).unwrap(), b"edited\n");
    let located = json(root.path(), &["workspace", "path", "work"], 0);
    assert_eq!(located["workspace"]["root"], workspace["root"]);
    assert_eq!(located["workspace"]["exists"], true);
    assert_eq!(located["workspace"]["base_version"], 1);
    let printed = invoke(root.path(), &["workspace", "path", "work"]);
    assert_eq!(
        String::from_utf8(printed.stdout).unwrap().trim(),
        dir.display().to_string()
    );

    let args = ["workspace", "discard", "work", "--operation", "drop-work"];
    let discarded = json(root.path(), &args, 0);
    assert_eq!(discarded["update"]["outcome"], "applied");
    assert_eq!(discarded["trashed"], true);
    assert!(!dir.exists());
    let retried = json(root.path(), &args, 0);
    assert_eq!(retried["update"], discarded["update"]);
    let refused = json(root.path(), &["workspace", "open", "work"], 2);
    assert_eq!(refused["error"]["code"], "invalid_input");
    let recovered = json(root.path(), &["workspace", "recover"], 0);
    assert_eq!(recovered["recovery"]["remaining"], 0);

    let unknown = json(root.path(), &["workspace", "mount", "work"], 2);
    assert!(
        unknown["error"]["message"]
            .as_str()
            .unwrap()
            .contains("publish")
    );
}

fn seed_world(root: &Path) {
    let mut repo = Repository::init(root).unwrap();
    let content = repo.put_blob(b"hello\n").unwrap();
    let entry = FileEntry::Regular {
        content,
        executable: false,
    };
    let manifest = Manifest::new([(RepoPath::parse("docs/readme.txt").unwrap(), entry)]).unwrap();
    let manifest = repo.put_manifest(&manifest).unwrap();
    let operation = OperationId::parse("world-1").unwrap();
    repo.compare_and_advance(&operation, VersionNumber::new(0).unwrap(), manifest)
        .unwrap();
}

fn world_file(root: &Path, version: u64, path: &str) -> Option<Vec<u8>> {
    let repo = Repository::open(root).unwrap();
    let manifest = repo
        .manifest_at(VersionNumber::new(version).unwrap())
        .unwrap();
    match manifest.entries().get(&RepoPath::parse(path).unwrap())? {
        FileEntry::Regular { content, .. } => Some(repo.read_blob(*content).unwrap()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn workspace_publish_captures_then_publishes_the_whole_layer_with_exact_retries() {
    let root = scratch();
    seed_world(root.path());
    let dir = |layer: &str| {
        let created = json(root.path(), &["workspace", "create", layer], 0);
        Path::new(created["workspace"]["root"].as_str().unwrap()).to_path_buf()
    };
    let work = dir("work");
    let rival = dir("rival");
    let agent = dir("agent");

    // Uncaptured edits are captured first, so Publish carries exactly the directory.
    fs::write(work.join("docs/readme.txt"), b"edited\n").unwrap();
    fs::write(work.join("new.txt"), b"new\n").unwrap();
    let args = [
        "workspace",
        "publish",
        "work",
        "--operation",
        "publish-work",
    ];
    let published = json(root.path(), &args, 0);
    assert_eq!(published["capture"]["update"]["outcome"], "applied");
    let receipt = &published["receipt"];
    assert_eq!(receipt["outcome"]["outcome"], "published");
    assert_eq!(receipt["outcome"]["version"]["number"], 2);
    assert_eq!(receipt["attribution"]["kind"], "human");
    assert_eq!(
        world_file(root.path(), 2, "docs/readme.txt").unwrap(),
        b"edited\n"
    );
    assert_eq!(world_file(root.path(), 2, "new.txt").unwrap(), b"new\n");

    // A retry replays the durable receipt; other input under the same ID is refused.
    assert_eq!(json(root.path(), &args, 0)["receipt"], *receipt);
    let mut reused = args.to_vec();
    reused.extend(["--actor", "agent", "--allow-missing-trace"]);
    let reused = json(root.path(), &reused, 1);
    assert_eq!(reused["error"]["code"], "operation_id_reused");
    let again = [
        "workspace",
        "publish",
        "work",
        "--operation",
        "publish-work-2",
    ];
    let stale = json(root.path(), &again, 3);
    assert_eq!(stale["receipt"]["outcome"]["outcome"], "layer_stale");

    // A concurrent edit of the same path is a durable conflict; nothing is published.
    fs::write(rival.join("docs/readme.txt"), b"rival\n").unwrap();
    let args = [
        "workspace",
        "publish",
        "rival",
        "--operation",
        "publish-rival",
    ];
    let conflict = json(root.path(), &args, 3);
    assert_eq!(conflict["receipt"]["outcome"]["outcome"], "conflict");
    assert_eq!(
        conflict["receipt"]["outcome"]["paths"][0],
        "docs/readme.txt"
    );
    assert_eq!(
        json(root.path(), &["history"], 0)["versions"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    // Agent attribution stores the trace file as the context Blob, and a disjoint
    // change composes onto the advanced World.
    fs::write(agent.join("agent.txt"), b"agent\n").unwrap();
    let trace = root.path().join("trace.json");
    fs::write(&trace, b"{\"steps\":1}").unwrap();
    let args = [
        "workspace",
        "publish",
        "agent",
        "--operation",
        "publish-agent",
        "--actor",
        "agent",
        "--trace",
        trace.to_str().unwrap(),
    ];
    let published = json(root.path(), &args, 0);
    let receipt = &published["receipt"];
    assert_eq!(receipt["outcome"]["version"]["number"], 3);
    assert_eq!(receipt["attribution"]["kind"], "agent");
    assert!(receipt["attribution"]["context"].is_string());
    assert_eq!(world_file(root.path(), 3, "agent.txt").unwrap(), b"agent\n");
    assert_eq!(world_file(root.path(), 3, "new.txt").unwrap(), b"new\n");
    assert_eq!(json(root.path(), &["check"], 0)["ok"], true);
}
