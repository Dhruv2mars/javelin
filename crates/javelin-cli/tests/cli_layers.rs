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
        vec!["workspace", "mount", "work"],
        vec!["workspace", "recover", "extra"],
        vec!["workspace", "publish", "work"],
        vec!["workspace", "open", "work", "--root", "/missing"],
        vec!["workspace", "open", "work", "--helper", "/missing"],
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
        vec![
            "--actor",
            "agent",
            "--trace",
            "missing",
            "--allow-missing-trace",
        ],
        vec![
            "--actor",
            "agent",
            "--context",
            &"a".repeat(64),
            "--allow-missing-trace",
        ],
    ] {
        let mut args = vec!["workspace", "publish", "work", "--operation", "publish"];
        args.extend(extra);
        args.push("--json");
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
            "publish ID --operation ID",
            "no mount or daemon",
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
    // Seed a receipt through the library to inspect it without a workspace.
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
fn stack_fixture() -> (TempDir, Repository) {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let content = repo.put_blob(b"first\nanchor\nlast\n").unwrap();
    let manifest = repo
        .put_manifest(
            &Manifest::new([(
                path("file"),
                FileEntry::Regular {
                    content,
                    executable: false,
                },
            )])
            .unwrap(),
        )
        .unwrap();
    repo.compare_and_advance(&operation("base"), VersionNumber::new(0).unwrap(), manifest)
        .unwrap();
    for name in ["source", "target"] {
        ok(root.path(), &["layer", "create", name, "--world", "1"]);
    }
    (root, repo)
}

fn capture_text(repo: &mut Repository, layer: &str, bytes: &[u8], op: &str) {
    let content = repo.put_blob(bytes).unwrap();
    capture_fixture(
        repo,
        layer,
        vec![(
            path("file"),
            Some(FileEntry::Regular {
                content,
                executable: false,
            }),
        )],
        op,
    );
}

#[test]
fn binary_stack_merges_frozen_source_and_replays_original_inputs_and_ancestry() {
    let (root, mut repo) = stack_fixture();
    capture_text(&mut repo, "source", b"first\nanchor\nLAST\n", "save-source");
    capture_text(&mut repo, "target", b"FIRST\nanchor\nlast\n", "save-target");
    let source = ok(root.path(), &["layer", "state", "source"])["state"]["head"]["checkpoint"]
        .as_str()
        .unwrap()
        .to_owned();
    // Advance the live source before Stack: the CLI must use the supplied frozen hash.
    capture_text(&mut repo, "source", b"later live source", "later-source");
    let args = [
        "layer",
        "stack",
        "target",
        "--revision",
        "1",
        "--source",
        &source,
        "--operation",
        "stack",
    ];
    let result = ok(root.path(), &args);
    assert_eq!(result["source"], source);
    assert_eq!(result["target"]["revision"], 1);
    assert_eq!(result["update"]["state"]["head"]["revision"], 2);
    let checkpoint = javelin::CheckpointId::parse(
        result["update"]["state"]["head"]["checkpoint"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let frozen = repo.read_checkpoint(checkpoint).unwrap();
    assert_eq!(
        frozen.stack_sources(),
        &[javelin::CheckpointId::parse(&source).unwrap()]
    );
    assert_eq!(
        repo.read_blob(
            repo.checkpoint_manifest(frozen.id()).unwrap().entries()[&path("file")].content(),
        )
        .unwrap(),
        b"FIRST\nanchor\nLAST\n"
    );
    assert_eq!(ok(root.path(), &args), result);
    let plain = invoke(root.path(), &args);
    assert!(plain.status.success());
    assert!(
        String::from_utf8(plain.stdout)
            .unwrap()
            .contains("Operation stack: applied")
    );
    capture_text(&mut repo, "target", b"later target", "later-target");
    let state = ok(root.path(), &["layer", "state", "target"]);
    assert_eq!(ok(root.path(), &args), result);
    assert_eq!(ok(root.path(), &["layer", "state", "target"]), state);
    let latest = state["state"]["head"]["checkpoint"].as_str().unwrap();
    assert_eq!(
        repo.read_checkpoint(javelin::CheckpointId::parse(latest).unwrap())
            .unwrap()
            .stack_sources(),
        frozen.stack_sources()
    );
    for (revision, source) in [("2", source.as_str()), ("1", latest)] {
        let reused = value(
            invoke(
                root.path(),
                &[
                    "layer",
                    "stack",
                    "target",
                    "--revision",
                    revision,
                    "--source",
                    source,
                    "--operation",
                    "stack",
                    "--json",
                ],
            ),
            1,
        );
        assert_eq!(reused["error"]["code"], "operation_id_reused");
    }
    let stale_args = [
        "layer",
        "stack",
        "target",
        "--revision",
        "1",
        "--source",
        &source,
        "--operation",
        "stale-stack",
        "--json",
    ];
    let stale = value(invoke(root.path(), &stale_args), 3);
    assert_eq!(stale["update"]["outcome"], "stale");
    capture_text(&mut repo, "target", b"later again", "later-again");
    assert_eq!(value(invoke(root.path(), &stale_args), 3), stale);
    assert_eq!(repo.read_checkpoint(checkpoint).unwrap(), frozen);
    assert_eq!(repo.history().unwrap().len(), 2);
    ok(root.path(), &["check"]);
}

#[test]
fn binary_stack_reports_conflicts_and_resource_or_loader_errors_without_mutation() {
    for bytes in [
        b"ours".to_vec(),
        b"line\n".repeat(8193),
        vec![b'x'; 1024 * 1024 + 1],
    ] {
        let (root, mut repo) = stack_fixture();
        capture_text(&mut repo, "source", b"theirs", "save-source");
        capture_text(&mut repo, "target", &bytes, "save-target");
        let source = ok(root.path(), &["layer", "state", "source"])["state"]["head"]["checkpoint"]
            .as_str()
            .unwrap()
            .to_owned();
        let before = ok(root.path(), &["layer", "state", "target"]);
        let integrity = ok(root.path(), &["check"]);
        let args = [
            "layer",
            "stack",
            "target",
            "--revision",
            "1",
            "--source",
            &source,
            "--operation",
            "stack",
            "--json",
        ];
        if bytes == b"ours" {
            let conflict = value(invoke(root.path(), &args), 3);
            assert_eq!(conflict["conflicts"][0]["path"], "file");
            assert_eq!(conflict["conflicts"][0]["kind"], "Content");
            assert!(conflict["conflicts"][0]["ours"].is_object());
            assert_eq!(value(invoke(root.path(), &args), 3), conflict);
            let plain = invoke(root.path(), &args[..args.len() - 1]);
            assert_eq!(plain.status.code(), Some(3));
            assert!(
                String::from_utf8(plain.stdout)
                    .unwrap()
                    .contains("conflicts; nothing applied")
            );
        } else {
            let error = value(invoke(root.path(), &args), 1);
            assert_eq!(error["error"]["code"], "stack_merge_error");
            assert!(error["error"]["message"].as_str().unwrap().contains(
                if bytes.len() > 1024 * 1024 {
                    "merge loader:"
                } else {
                    "merge resource Lines:"
                }
            ));
        }
        assert_eq!(ok(root.path(), &["layer", "state", "target"]), before);
        assert_eq!(ok(root.path(), &["check"]), integrity);
        // Refusals did not reserve the operation ID.
        ok(
            root.path(),
            &[
                "layer",
                "stack",
                "target",
                "--revision",
                "1",
                "--source",
                before["state"]["head"]["checkpoint"].as_str().unwrap(),
                "--operation",
                "stack",
            ],
        );
    }
}

#[test]
fn binary_stack_requires_explicit_inputs_before_opening_repository() {
    let root = TempDir::new().unwrap();
    for args in [
        vec!["layer", "stack", "target"],
        vec![
            "layer",
            "stack",
            "target",
            "--revision",
            "1",
            "--source",
            "live-layer",
            "--operation",
            "stack",
        ],
        vec![
            "layer",
            "stack",
            "target",
            "--revision",
            "1",
            "--source",
            &"0".repeat(64),
        ],
    ] {
        let mut args = args;
        args.push("--json");
        assert_eq!(
            value(invoke(root.path(), &args), 2)["error"]["code"],
            "invalid_input"
        );
    }
    assert!(!root.path().join(".javelin").exists());
}
