use std::{
    path::Path,
    process::{Command, Output},
};

use javelin::{
    FileEntry, LayerId, LayerState, Manifest, OperationId, RepoPath, Repository, SparseChanges,
    VersionNumber,
};
use serde_json::Value;
use tempfile::TempDir;

fn invoke(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_javelin"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}

fn value(output: Output, code: i32) -> Value {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    if code == 0 || code == 3 {
        assert!(output.stderr.is_empty(), "{output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    } else {
        assert!(output.stdout.is_empty(), "{output:?}");
        serde_json::from_slice(&output.stderr).unwrap()
    }
}

fn ok(root: &Path, args: &[&str]) -> Value {
    let mut args = args.to_vec();
    args.push("--json");
    let result = value(invoke(root, &args), 0);
    assert_eq!(result["ok"], true);
    result
}

fn operation(id: &str) -> OperationId {
    OperationId::parse(id).unwrap()
}
fn path(path: &str) -> RepoPath {
    RepoPath::parse(path).unwrap()
}

fn capture_fixture(
    repo: &mut Repository,
    layer: &str,
    entries: Vec<(RepoPath, Option<FileEntry>)>,
    op: &str,
) {
    let LayerState::Active { head } = repo.layer_state(&LayerId::parse(layer).unwrap()).unwrap()
    else {
        panic!("expected active Layer")
    };
    repo.capture_layer(&operation(op), &head, &SparseChanges::new(entries).unwrap())
        .unwrap();
}

#[test]
fn binary_creates_exact_worlds_and_clones_a_frozen_checkpoint() {
    let root = TempDir::new().unwrap();
    std::fs::write(root.path().join("existing.txt"), b"do not ingest").unwrap();
    ok(root.path(), &["init"]);
    let mut repo = Repository::open(root.path()).unwrap();
    let blob = repo.put_blob(b"base").unwrap();
    let original = FileEntry::Regular {
        content: blob,
        executable: false,
    };
    let manifest = repo
        .put_manifest(&Manifest::new([(path("old.txt"), original.clone())]).unwrap())
        .unwrap();
    repo.compare_and_advance(
        &operation("fixture-world"),
        VersionNumber::new(0).unwrap(),
        manifest,
    )
    .unwrap();

    let current = ok(root.path(), &["layer", "create", "source"]);
    let head = &current["state"]["head"];
    let checkpoint = head["checkpoint"].as_str().unwrap();
    assert_eq!(head["revision"], 0);
    assert_eq!(
        repo.read_checkpoint(javelin::CheckpointId::parse(checkpoint).unwrap())
            .unwrap()
            .base()
            .version()
            .get(),
        1
    );
    assert_eq!(
        ok(root.path(), &["layer", "create", "source", "--world", "1"]),
        current
    );
    let empty = ok(root.path(), &["layer", "create", "empty", "--world", "0"]);
    assert_ne!(empty["state"]["head"]["checkpoint"], head["checkpoint"]);
    let failed = value(
        invoke(
            root.path(),
            &["layer", "create", "source", "--world", "0", "--json"],
        ),
        2,
    );
    assert_eq!(failed["error"]["code"], "invalid_input");

    let clone = ok(
        root.path(),
        &["layer", "clone", "copy", "--checkpoint", checkpoint],
    );
    assert_eq!(clone["state"]["head"]["checkpoint"], checkpoint);
    assert_eq!(
        ok(
            root.path(),
            &["layer", "clone", "copy", "--checkpoint", checkpoint]
        ),
        clone
    );
    let replacement = repo.put_blob(b"later").unwrap();
    capture_fixture(
        &mut repo,
        "source",
        vec![
            (path("old.txt"), None),
            (
                path("new\nname.txt"),
                Some(FileEntry::Regular {
                    content: replacement,
                    executable: true,
                }),
            ),
        ],
        "fixture-capture",
    );
    let diff = ok(root.path(), &["layer", "diff", "source"]);
    assert_eq!(diff["changes"].as_array().unwrap().len(), 2);
    assert_eq!(diff["changes"][0]["path"], "new\nname.txt");
    assert_eq!(diff["changes"][0]["after"]["executable"], true);
    assert_eq!(
        diff["changes"][1]["before"],
        serde_json::to_value(original).unwrap()
    );
    assert!(diff["changes"][1]["after"].is_null());
    let plain = invoke(root.path(), &["layer", "diff", "source"]);
    assert!(plain.status.success());
    assert_eq!(
        String::from_utf8(plain.stdout).unwrap(),
        "A \"new\\nname.txt\"\nD \"old.txt\"\n"
    );
    assert!(
        ok(root.path(), &["layer", "diff", "source", "--revision", "0"])["changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        ok(root.path(), &["layer", "diff", "copy"])["changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        ok(root.path(), &["layer", "state", "copy"])["state"],
        clone["state"]
    );
    let history = ok(root.path(), &["layer", "history", "source"]);
    assert_eq!(history["history"].as_array().unwrap().len(), 2);
    assert_eq!(history["history"][0], current["state"]);
    assert_eq!(ok(root.path(), &["check"])["integrity"]["layers"], 3);
    assert_eq!(
        std::fs::read(root.path().join("existing.txt")).unwrap(),
        b"do not ingest"
    );
}

#[test]
fn binary_discard_uses_exact_revision_and_replays_after_terminal_state() {
    let root = TempDir::new().unwrap();
    ok(root.path(), &["init"]);
    let initial = ok(root.path(), &["layer", "create", "work"]);
    let mut repo = Repository::open(root.path()).unwrap();
    let blob = repo.put_blob(b"checkpointed").unwrap();
    capture_fixture(
        &mut repo,
        "work",
        vec![(
            path("kept"),
            Some(FileEntry::Regular {
                content: blob,
                executable: false,
            }),
        )],
        "fixture-capture",
    );
    let before = ok(root.path(), &["layer", "state", "work"]);
    let stale_args = [
        "layer",
        "discard",
        "work",
        "--revision",
        "0",
        "--operation",
        "stale-discard",
        "--json",
    ];
    let stale = value(invoke(root.path(), &stale_args), 3);
    assert_eq!(stale["ok"], false);
    assert_eq!(stale["update"]["outcome"], "stale");
    assert_eq!(ok(root.path(), &["layer", "state", "work"]), before);
    let changed = value(
        invoke(
            root.path(),
            &[
                "layer",
                "discard",
                "work",
                "--revision",
                "1",
                "--operation",
                "stale-discard",
                "--json",
            ],
        ),
        1,
    );
    assert_eq!(changed["error"]["code"], "operation_id_reused");
    let args = [
        "layer",
        "discard",
        "work",
        "--revision",
        "1",
        "--operation",
        "discard-work",
    ];
    let discarded = ok(root.path(), &args);
    assert_eq!(discarded["update"]["state"]["state"], "discarded");
    assert_eq!(ok(root.path(), &args), discarded);
    assert_eq!(value(invoke(root.path(), &stale_args), 3), stale);
    let history = ok(root.path(), &["layer", "history", "work"]);
    assert_eq!(history["history"].as_array().unwrap().len(), 3);
    assert_eq!(history["history"][0], initial["state"]);
    assert_eq!(history["history"][2]["revision"], 2);
    assert_eq!(
        ok(root.path(), &["layer", "diff", "work"])["changes"][0]["path"],
        "kept"
    );
    let checkpoint = before["state"]["head"]["checkpoint"].as_str().unwrap();
    ok(
        root.path(),
        &["layer", "clone", "recovered", "--checkpoint", checkpoint],
    );
    assert_eq!(
        ok(root.path(), &["layer", "diff", "recovered"])["changes"][0]["path"],
        "kept"
    );
    assert_eq!(
        ok(root.path(), &["history"])["versions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    ok(root.path(), &["check"]);
}

#[test]
fn binary_rejects_bad_grammar_before_mutation_and_explains_help() {
    let root = TempDir::new().unwrap();
    for args in [
        vec!["layer"],
        vec!["layer", "publish", "work"],
        vec!["layer", "create"],
        vec!["layer", "create", "work", "extra"],
        vec!["layer", "create", "work", "--world"],
        vec!["layer", "create", "work", "--world", "x"],
        vec!["layer", "create", "work", "--world", "+1"],
        vec!["layer", "create", "work", "--world", "18446744073709551616"],
        vec!["layer", "create", "work", "--world", "0", "--world", "0"],
        vec!["layer", "state", "work", "--operation", "unused"],
        vec!["layer", "clone", "copy"],
        vec!["layer", "clone", "copy", "--checkpoint", "bad"],
        vec!["layer", "discard", "work", "--operation", "discard"],
        vec!["layer", "discard", "work", "--revision", "0"],
        vec![
            "workspace",
            "checkpoint",
            "work",
            "--root",
            "/missing",
            "--helper",
            "/missing",
        ],
        vec![
            "workspace",
            "recover",
            "extra",
            "--root",
            "/missing",
            "--helper",
            "/missing",
        ],
    ] {
        let mut args = args;
        args.push("--json");
        let failure = value(invoke(root.path(), &args), 2);
        assert_eq!(failure["error"]["code"], "invalid_input", "{args:?}");
    }
    for extra in [
        vec!["--actor", "robot"],
        vec!["--context", "bad"],
        vec!["--actor", "human", "--allow-missing-trace"],
        vec!["--trace", "missing"],
        vec![
            "--actor",
            "agent",
            "--context",
            &"a".repeat(64),
            "--trace",
            "missing",
        ],
        vec![
            "--actor",
            "agent",
            "--allow-missing-trace",
            "--allow-missing-trace",
        ],
    ] {
        let mut args = vec![
            "workspace",
            "publish",
            "work",
            "--root",
            "/missing",
            "--helper",
            "/missing",
            "--operation",
            "publish",
            "--json",
        ];
        args.extend(extra);
        assert_eq!(
            value(invoke(root.path(), &args), 2)["error"]["code"],
            "invalid_input"
        );
    }
    assert!(!root.path().join(".javelin").exists());
    for args in [
        vec!["--help"],
        vec!["layer", "--help"],
        vec!["workspace", "--help"],
    ] {
        let help = invoke(root.path(), &args);
        assert!(help.status.success());
        let help = String::from_utf8(help.stdout).unwrap();
        assert!(help.starts_with("Javelin native repository foundation\n"));
        for text in [
            "create",
            "--checkpoint",
            "--operation",
            "recover",
            "Linux",
            "Autosave is later",
        ] {
            assert!(help.contains(text), "{text}: {help}");
        }
    }
}

#[test]
fn binary_repo_option_works_outside_the_project() {
    let parent = TempDir::new().unwrap();
    let root = parent.path().join("project with spaces");
    std::fs::create_dir(&root).unwrap();
    let repo = root.to_str().unwrap();
    ok(parent.path(), &["init", repo]);
    let created = ok(
        parent.path(),
        &["layer", "create", "outside", "--repo", repo],
    );
    assert_eq!(
        ok(
            parent.path(),
            &["layer", "state", "outside", "--repo", repo]
        )["state"],
        created["state"]
    );
}

#[test]
fn binary_inspects_and_clones_a_published_layer_in_format_three() {
    let root = TempDir::new().unwrap();
    ok(root.path(), &["init"]);
    ok(
        root.path(),
        &["layer", "create", "published", "--world", "0"],
    );
    let mut repo = Repository::open(root.path()).unwrap();
    let content = repo.put_blob(b"published content").unwrap();
    capture_fixture(
        &mut repo,
        "published",
        vec![(
            path("kept.txt"),
            Some(FileEntry::Regular {
                content,
                executable: false,
            }),
        )],
        "capture-before-publish",
    );
    let before = ok(root.path(), &["layer", "diff", "published"]);
    let LayerState::Active { head } = repo
        .layer_state(&LayerId::parse("published").unwrap())
        .unwrap()
    else {
        panic!("expected active Layer");
    };
    // Seed a receipt for platform-independent inspection without mounting a view.
    repo.publish_layer(
        &operation("fixture-publish"),
        &head,
        &javelin::PublishAttribution::Human,
        |_, _| Ok(()),
    )
    .unwrap();
    let receipt = ok(
        root.path(),
        &["layer", "publication", "--operation", "fixture-publish"],
    );
    assert_eq!(receipt["receipt"]["outcome"]["outcome"], "published");
    assert_eq!(receipt["receipt"]["attribution"]["kind"], "human");
    assert!(
        ok(
            root.path(),
            &["layer", "publication", "--operation", "unknown"]
        )["receipt"]
            .is_null()
    );
    let stale_receipt = repo
        .publish_layer(
            &operation("stale-inspection"),
            &head,
            &javelin::PublishAttribution::Human,
            |_, _| Ok(()),
        )
        .unwrap();
    let stale = value(
        invoke(
            root.path(),
            &[
                "layer",
                "publication",
                "--operation",
                "stale-inspection",
                "--json",
            ],
        ),
        3,
    );
    assert_eq!(
        stale["receipt"],
        serde_json::to_value(stale_receipt).unwrap()
    );
    assert_eq!(stale["receipt"]["outcome"]["outcome"], "layer_stale");
    let plain_receipt = invoke(
        root.path(),
        &["layer", "publication", "--operation", "fixture-publish"],
    );
    assert!(plain_receipt.status.success());
    assert!(
        String::from_utf8(plain_receipt.stdout)
            .unwrap()
            .contains("published as World 1")
    );
    let state = repo.layer_state(head.layer()).unwrap();
    let inspected = ok(root.path(), &["layer", "state", "published"]);
    assert_eq!(inspected["state"], serde_json::to_value(&state).unwrap());
    assert_eq!(inspected["state"]["state"], "published");
    assert_eq!(inspected["state"]["version"]["number"], 1);
    assert_eq!(inspected["state"]["operation"], "fixture-publish");
    let plain = invoke(root.path(), &["layer", "state", "published"]);
    assert!(plain.status.success());
    assert_eq!(
        String::from_utf8(plain.stdout).unwrap(),
        format!(
            "Layer published published as World 1 operation fixture-publish revision 1 checkpoint {}\n",
            head.checkpoint().object()
        )
    );
    let history = ok(root.path(), &["layer", "history", "published"]);
    assert_eq!(history["history"].as_array().unwrap().len(), 3);
    assert_eq!(history["history"][2], inspected["state"]);
    let history_plain = invoke(root.path(), &["layer", "history", "published"]);
    assert!(history_plain.status.success());
    assert!(
        String::from_utf8(history_plain.stdout)
            .unwrap()
            .contains("published as World 1")
    );
    assert_eq!(ok(root.path(), &["layer", "diff", "published"]), before);
    assert_eq!(
        ok(
            root.path(),
            &["layer", "diff", "published", "--revision", "1"]
        ),
        before
    );
    let checkpoint = head.checkpoint().object().to_string();
    let cloned = ok(
        root.path(),
        &[
            "layer",
            "clone",
            "copy-published",
            "--checkpoint",
            &checkpoint,
        ],
    );
    assert_eq!(cloned["state"]["state"], "active");
    assert_eq!(cloned["state"]["head"]["checkpoint"], checkpoint);
    assert_eq!(
        ok(root.path(), &["layer", "diff", "copy-published"])["changes"],
        before["changes"]
    );
    let created = ok(root.path(), &["layer", "create", "next-world"]);
    let next =
        javelin::CheckpointId::parse(created["state"]["head"]["checkpoint"].as_str().unwrap())
            .unwrap();
    assert_eq!(
        repo.read_checkpoint(next).unwrap().base().version().get(),
        1
    );
    let refused = value(
        invoke(
            root.path(),
            &[
                "layer",
                "discard",
                "published",
                "--revision",
                "1",
                "--operation",
                "discard-published",
                "--json",
            ],
        ),
        1,
    );
    assert_eq!(refused["error"]["code"], "layer_published");
    assert_eq!(ok(root.path(), &["layer", "state", "published"]), inspected);
    assert_eq!(
        ok(root.path(), &["history"])["versions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    ok(root.path(), &["check"]);
}

#[cfg(not(target_os = "linux"))]
#[test]
fn binary_workspace_commands_return_unsupported_without_touching_paths() {
    let root = TempDir::new().unwrap();
    for action in [
        "init",
        "mount",
        "path",
        "unmount",
        "checkpoint",
        "publish",
        "discard",
        "recover",
    ] {
        let mut args = vec!["workspace", action];
        if !["init", "recover"].contains(&action) {
            args.push("work");
        }
        args.extend([
            "--root",
            "/does-not-exist",
            "--helper",
            "/does-not-exist",
            "--json",
        ]);
        if ["checkpoint", "discard", "publish"].contains(&action) {
            args.extend(["--operation", "test-op"]);
        }
        let failure = value(invoke(root.path(), &args), 1);
        assert_eq!(failure["error"]["code"], "unsupported");
        assert!(
            failure["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Linux OverlayFS")
        );
    }
    assert!(!root.path().join(".javelin").exists());
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "real mounts only on explicitly enabled disposable hosted Linux"]
fn linux_cli_path_checkpoint_clone_and_recovery() {
    assert_eq!(std::env::var("GITHUB_ACTIONS").as_deref(), Ok("true"));
    assert_eq!(
        std::env::var("RUNNER_ENVIRONMENT").as_deref(),
        Ok("github-hosted")
    );
    assert_eq!(std::env::var("JAVELIN_CLI_MOUNT_TEST").as_deref(), Ok("1"));
    assert_ne!(unsafe { libc::getuid() }, 0);
    let root = TempDir::new().unwrap();
    let runtime = std::env::var("JAVELIN_CLI_RUNTIME").unwrap();
    let helper = std::env::var("JAVELIN_CLI_HELPER").unwrap();
    let workspace = |action: &str, layer: Option<&str>, op: Option<&str>| {
        let mut args = vec!["workspace", action, "--root", &runtime, "--helper", &helper];
        if let Some(layer) = layer {
            args.push(layer);
        }
        if let Some(op) = op {
            args.extend(["--operation", op]);
        }
        ok(root.path(), &args)
    };
    ok(root.path(), &["init"]);
    ok(root.path(), &["layer", "create", "source"]);
    workspace("init", None, None);
    let mounted = workspace("mount", Some("source"), None);
    let mounted = Path::new(mounted["path"].as_str().unwrap());
    assert_eq!(
        workspace("path", Some("source"), None)["path"],
        mounted.to_str().unwrap()
    );
    let tools = Command::new("sh").current_dir(mounted).args(["-eu", "-c", "printf 'int main(void) { return 0; }\\n' > main.c; cc main.c -o app; ./app; rm app; printf 'one\\n' > keep; printf 'gone\\n' > delete; ln -s keep link"]).output().unwrap();
    assert!(tools.status.success(), "{tools:?}");
    assert!(
        ok(root.path(), &["layer", "diff", "source"])["changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let checkpoint = workspace("checkpoint", Some("source"), Some("capture-one"));
    let frozen = checkpoint["capture"]["update"]["state"]["head"]["checkpoint"]
        .as_str()
        .unwrap();
    assert_eq!(checkpoint["capture"]["changed_paths"], 4);
    assert_eq!(
        workspace("checkpoint", Some("source"), Some("capture-one"))["capture"]["replayed"],
        true
    );
    std::fs::write(mounted.join("keep"), b"two\n").unwrap();
    std::fs::remove_file(mounted.join("delete")).unwrap();
    workspace("checkpoint", Some("source"), Some("capture-two"));
    let replay = workspace("checkpoint", Some("source"), Some("capture-one"));
    assert_eq!(replay["capture"]["update"]["state"]["head"]["revision"], 1);
    assert_eq!(
        ok(root.path(), &["layer", "state", "source"])["state"]["head"]["revision"],
        2
    );
    ok(
        root.path(),
        &["layer", "clone", "copy", "--checkpoint", frozen],
    );
    let clone = workspace("mount", Some("copy"), None);
    let clone = Path::new(clone["path"].as_str().unwrap());
    assert_eq!(std::fs::read(clone.join("keep")).unwrap(), b"one\n");
    assert_eq!(std::fs::read(clone.join("delete")).unwrap(), b"gone\n");
    std::fs::write(clone.join("keep"), b"private\n").unwrap();
    assert_eq!(std::fs::read(mounted.join("keep")).unwrap(), b"two\n");
    let held = std::fs::OpenOptions::new()
        .write(true)
        .open(mounted.join("keep"))
        .unwrap();
    let busy = value(
        invoke(
            root.path(),
            &[
                "workspace",
                "checkpoint",
                "source",
                "--root",
                &runtime,
                "--helper",
                &helper,
                "--operation",
                "busy-retry",
                "--json",
            ],
        ),
        1,
    );
    assert_eq!(busy["error"]["code"], "busy");
    drop(held);
    workspace("checkpoint", Some("source"), Some("busy-retry"));
    workspace("unmount", Some("source"), None);
    let absent = value(
        invoke(
            root.path(),
            &[
                "workspace",
                "path",
                "source",
                "--root",
                &runtime,
                "--helper",
                &helper,
                "--json",
            ],
        ),
        2,
    );
    assert_eq!(absent["error"]["code"], "invalid_input");
    workspace("mount", Some("source"), None);
    assert_eq!(std::fs::read(mounted.join("keep")).unwrap(), b"two\n");
    workspace("discard", Some("source"), Some("discard-source"));
    assert_eq!(
        ok(root.path(), &["layer", "state", "source"])["state"]["state"],
        "discarded"
    );
    assert_eq!(std::fs::read(clone.join("keep")).unwrap(), b"private\n");
    // Native retry has an exact historical CAS token even after runtime cleanup.
    ok(
        root.path(),
        &[
            "layer",
            "discard",
            "source",
            "--revision",
            "3",
            "--operation",
            "discard-source",
        ],
    );
    workspace("recover", None, None);
    let broken = Path::new(&runtime).join("state/broken.json");
    std::fs::write(&broken, b"{}").unwrap();
    let partial = invoke(
        root.path(),
        &[
            "workspace",
            "recover",
            "--root",
            &runtime,
            "--helper",
            &helper,
            "--json",
        ],
    );
    assert_eq!(partial.status.code(), Some(1), "{partial:?}");
    assert!(partial.stderr.is_empty());
    let partial: Value = serde_json::from_slice(&partial.stdout).unwrap();
    assert_eq!(partial["ok"], false);
    assert_eq!(partial["recovery"]["failures"].as_array().unwrap().len(), 1);
    std::fs::remove_file(broken).unwrap();
    workspace("recover", None, None);
    workspace("discard", Some("copy"), Some("discard-copy"));
    assert_eq!(
        ok(root.path(), &["history"])["versions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    ok(root.path(), &["check"]);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "real mounts only on explicitly enabled disposable hosted Linux"]
fn linux_cli_whole_layer_publish() {
    assert_eq!(std::env::var("GITHUB_ACTIONS").as_deref(), Ok("true"));
    assert_eq!(
        std::env::var("RUNNER_ENVIRONMENT").as_deref(),
        Ok("github-hosted")
    );
    assert_eq!(std::env::var("JAVELIN_CLI_MOUNT_TEST").as_deref(), Ok("1"));
    assert_ne!(unsafe { libc::getuid() }, 0);
    let root = TempDir::new().unwrap();
    let runtime = format!("{}-publish", std::env::var("JAVELIN_CLI_RUNTIME").unwrap());
    let helper = std::env::var("JAVELIN_CLI_HELPER").unwrap();
    let call = |action: &str, layer: Option<&str>, extras: &[&str], code| {
        let mut args = vec![
            "workspace",
            action,
            "--root",
            &runtime,
            "--helper",
            &helper,
            "--json",
        ];
        if let Some(layer) = layer {
            args.push(layer);
        }
        args.extend_from_slice(extras);
        value(invoke(root.path(), &args), code)
    };
    ok(root.path(), &["init"]);
    for layer in ["work", "competing", "agent"] {
        ok(root.path(), &["layer", "create", layer, "--world", "0"]);
    }
    call("init", None, &[], 0);
    let mounted = call("mount", Some("work"), &[], 0);
    let view = Path::new(mounted["path"].as_str().unwrap());
    std::fs::write(view.join("file"), b"accepted").unwrap();
    let publish = ["--operation", "publish-work"];
    let uncaptured = call("publish", Some("work"), &publish, 2);
    assert_eq!(uncaptured["error"]["code"], "invalid_input");
    assert!(
        ok(
            root.path(),
            &["layer", "publication", "--operation", "publish-work"]
        )["receipt"]
            .is_null()
    );
    call("checkpoint", Some("work"), &["--operation", "save-work"], 0);
    let held = std::fs::OpenOptions::new()
        .write(true)
        .open(view.join("file"))
        .unwrap();
    assert_eq!(
        call("publish", Some("work"), &publish, 1)["error"]["code"],
        "busy"
    );
    drop(held);
    let accepted = call("publish", Some("work"), &publish, 0);
    assert_eq!(accepted["receipt"]["outcome"]["version"]["number"], 1);
    assert_eq!(accepted["receipt"]["warnings"], serde_json::json!([]));
    assert_eq!(call("publish", Some("work"), &publish, 0), accepted);
    assert_eq!(std::fs::read(view.join("file")).unwrap(), b"accepted");
    let error = std::fs::write(view.join("file"), b"clobber").unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EROFS));
    assert_eq!(
        ok(root.path(), &["layer", "state", "work"])["state"]["state"],
        "published"
    );
    assert_eq!(
        call(
            "publish",
            Some("work"),
            &[
                "--operation",
                "publish-work",
                "--actor",
                "agent",
                "--allow-missing-trace"
            ],
            1
        )["error"]["code"],
        "operation_id_reused"
    );

    // A conflicting whole Layer never clobbers the accepted file. Its durable
    // receipt remains the original result even after another checkpoint.
    let competing = call("mount", Some("competing"), &[], 0);
    assert_eq!(
        call("publish", Some("competing"), &publish, 1)["error"]["code"],
        "operation_id_reused"
    );
    let competing = Path::new(competing["path"].as_str().unwrap());
    std::fs::write(competing.join("file"), b"conflict").unwrap();
    call(
        "checkpoint",
        Some("competing"),
        &["--operation", "save-competing"],
        0,
    );
    let conflict = call(
        "publish",
        Some("competing"),
        &["--operation", "conflict"],
        3,
    );
    assert_eq!(
        conflict["receipt"]["outcome"]["paths"],
        serde_json::json!(["file"])
    );
    std::fs::write(competing.join("file"), b"still writable").unwrap();
    call(
        "checkpoint",
        Some("competing"),
        &["--operation", "save-competing-two"],
        0,
    );
    assert_eq!(
        call(
            "publish",
            Some("competing"),
            &["--operation", "conflict"],
            3
        ),
        conflict
    );
    assert_eq!(std::fs::read(view.join("file")).unwrap(), b"accepted");
    assert_eq!(
        value(
            invoke(
                root.path(),
                &["layer", "publication", "--operation", "conflict", "--json"]
            ),
            3
        )["receipt"],
        conflict["receipt"]
    );

    let agent = call("mount", Some("agent"), &[], 0);
    let agent = Path::new(agent["path"].as_str().unwrap());
    std::fs::write(agent.join("agent.txt"), b"agent change").unwrap();
    call(
        "checkpoint",
        Some("agent"),
        &["--operation", "save-agent"],
        0,
    );
    let missing = ["--operation", "publish-agent", "--actor", "agent"];
    assert_eq!(
        call("publish", Some("agent"), &missing, 1)["error"]["code"],
        "missing_agent_trace"
    );
    let mut override_args = missing.to_vec();
    override_args.push("--allow-missing-trace");
    let overridden = call("publish", Some("agent"), &override_args, 0);
    assert_eq!(
        overridden["receipt"]["warnings"],
        serde_json::json!(["missing_agent_trace"])
    );
    assert_eq!(
        call("publish", Some("agent"), &override_args, 0),
        overridden
    );
    assert_eq!(call("publish", Some("work"), &publish, 0), accepted);

    for (op, code, expected_text) in [
        ("publish-work", 0, "published as World 1"),
        ("conflict", 3, "conflict; nothing published"),
        ("publish-agent", 0, "Warning: \"missing_agent_trace\""),
    ] {
        let plain = invoke(root.path(), &["layer", "publication", "--operation", op]);
        assert_eq!(plain.status.code(), Some(code));
        assert!(plain.stderr.is_empty());
        assert!(
            String::from_utf8(plain.stdout)
                .unwrap()
                .contains(expected_text)
        );
    }

    // Continue from the current World, keeping opaque raw context in a Blob.
    ok(root.path(), &["layer", "create", "next"]);
    let next = call("mount", Some("next"), &[], 0);
    let next_view = Path::new(next["path"].as_str().unwrap());
    assert_eq!(std::fs::read(next_view.join("file")).unwrap(), b"accepted");
    std::fs::write(next_view.join("next.txt"), b"next").unwrap();
    call("checkpoint", Some("next"), &["--operation", "save-next"], 0);
    let trace_path = root.path().join("trace.bin");
    let raw = b"opaque\0trace\xff";
    std::fs::write(&trace_path, raw).unwrap();
    let trace = trace_path.to_str().unwrap();
    let traced = call(
        "publish",
        Some("next"),
        &[
            "--operation",
            "publish-next",
            "--actor",
            "agent",
            "--trace",
            trace,
        ],
        0,
    );
    let context = traced["receipt"]["attribution"]["context"]
        .as_str()
        .unwrap();
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(
        repo.read_blob(javelin::ObjectId::parse(context).unwrap())
            .unwrap(),
        raw
    );
    let referenced = call(
        "publish",
        Some("next"),
        &[
            "--operation",
            "publish-next",
            "--actor",
            "agent",
            "--context",
            context,
        ],
        0,
    );
    assert_eq!(referenced, traced);
    std::fs::write(&trace_path, b"changed").unwrap();
    assert_eq!(
        call(
            "publish",
            Some("next"),
            &[
                "--operation",
                "publish-next",
                "--actor",
                "agent",
                "--trace",
                trace
            ],
            1
        )["error"]["code"],
        "operation_id_reused"
    );
    call("unmount", Some("work"), &[], 0);
    assert_eq!(call("publish", Some("work"), &publish, 0), accepted);
    call("recover", None, &[], 0);
    call("mount", Some("work"), &[], 0);
    assert_eq!(
        std::fs::write(view.join("file"), b"remount clobber")
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EROFS)
    );
    let checkpoint = traced["receipt"]["expected"]["checkpoint"]
        .as_str()
        .unwrap();
    ok(
        root.path(),
        &["layer", "clone", "continued", "--checkpoint", checkpoint],
    );
    call("mount", Some("continued"), &[], 0);
    call(
        "discard",
        Some("continued"),
        &["--operation", "discard-continued"],
        0,
    );
    call(
        "discard",
        Some("competing"),
        &["--operation", "discard-competing"],
        0,
    );
    for layer in ["work", "agent", "next"] {
        call("unmount", Some(layer), &[], 0);
    }
    call("recover", None, &[], 0);
    assert_eq!(
        ok(root.path(), &["history"])["versions"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    ok(root.path(), &["check"]);
}
