//! `javelin layer stack --resolutions` and `javelin workspace refresh/publish` through
//! each structural conflict class, driven through the real binary.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use javelin::{
    CheckpointId, FileEntry, LayerId, LayerState, Manifest, OperationId, RepoPath, Repository,
    SparseChanges, VersionNumber,
};
use serde_json::{Value, json};
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

/// One edit to a Layer. `Remove` deletes the path from the Layer.
enum Edit {
    Text(&'static [u8]),
    Exec(&'static [u8]),
    Link(&'static str),
    Remove,
}

/// World 1 holds `files`; Layers `source` and `target` are created from it.
fn fixture(files: &[(&str, &[u8])]) -> TempDir {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let entries: Vec<_> = files
        .iter()
        .map(|(name, bytes)| {
            let content = repo.put_blob(bytes).unwrap();
            (
                path(name),
                FileEntry::Regular {
                    content,
                    executable: false,
                },
            )
        })
        .collect();
    let manifest = repo.put_manifest(&Manifest::new(entries).unwrap()).unwrap();
    repo.compare_and_advance(
        &operation("world-1"),
        VersionNumber::new(0).unwrap(),
        manifest,
    )
    .unwrap();
    drop(repo);
    for name in ["source", "target"] {
        ok(root.path(), &["layer", "create", name, "--world", "1"]);
    }
    root
}

fn capture(root: &Path, layer: &str, edits: &[(&str, Edit)], op: &str) {
    let mut repo = Repository::open(root).unwrap();
    let entries: Vec<_> = edits
        .iter()
        .map(|(name, edit)| {
            let entry = match edit {
                Edit::Text(bytes) => Some(FileEntry::Regular {
                    content: repo.put_blob(bytes).unwrap(),
                    executable: false,
                }),
                Edit::Exec(bytes) => Some(FileEntry::Regular {
                    content: repo.put_blob(bytes).unwrap(),
                    executable: true,
                }),
                Edit::Link(target) => Some(FileEntry::Symlink {
                    target: repo.put_blob(target.as_bytes()).unwrap(),
                }),
                Edit::Remove => None,
            };
            (path(name), entry)
        })
        .collect();
    let LayerState::Active { head } = repo.layer_state(&LayerId::parse(layer).unwrap()).unwrap()
    else {
        panic!("expected active Layer")
    };
    repo.capture_layer(&operation(op), &head, &SparseChanges::new(entries).unwrap())
        .unwrap();
}

/// The `(checkpoint, revision)` of a Layer's current head.
fn head(root: &Path, layer: &str) -> (String, u64) {
    let state = ok(root, &["layer", "state", layer]);
    let head = &state["state"]["head"];
    (
        head["checkpoint"].as_str().unwrap().to_owned(),
        head["revision"].as_u64().unwrap(),
    )
}

/// Stacks the current `source` head into `target`'s current head.
fn stack(root: &Path, op: &str, resolutions: Option<&Path>, code: i32) -> Value {
    let (_, revision) = head(root, "target");
    stack_at(root, revision, op, resolutions, code)
}

/// Stacks the current `source` head into `target` at exact revision `revision`.
fn stack_at(root: &Path, revision: u64, op: &str, resolutions: Option<&Path>, code: i32) -> Value {
    let (source, _) = head(root, "source");
    let revision = revision.to_string();
    let resolutions = resolutions.map(|file| file.display().to_string());
    let mut args = vec![
        "layer",
        "stack",
        "target",
        "--revision",
        &revision,
        "--source",
        &source,
        "--operation",
        op,
        "--json",
    ];
    if let Some(file) = &resolutions {
        args.extend(["--resolutions", file]);
    }
    value(invoke(root, &args), code)
}

fn write_resolutions(dir: &Path, name: &str, resolutions: Value) -> PathBuf {
    let file = dir.join(name);
    fs::write(&file, resolutions.to_string()).unwrap();
    file
}

/// The entry at `name` in the checkpoint `checkpoint`.
fn entry(root: &Path, checkpoint: &str, name: &str) -> Option<FileEntry> {
    let repo = Repository::open(root).unwrap();
    let id = CheckpointId::parse(checkpoint).unwrap();
    repo.checkpoint_manifest(id)
        .unwrap()
        .entries()
        .get(&path(name))
        .cloned()
}

fn text_at(root: &Path, checkpoint: &str, name: &str) -> Option<Vec<u8>> {
    let repo = Repository::open(root).unwrap();
    match entry(root, checkpoint, name)? {
        FileEntry::Regular { content, .. } => Some(repo.read_blob(content).unwrap()),
        other => panic!("{other:?}"),
    }
}

fn conflict_kinds(stacked: &Value) -> Vec<(String, String)> {
    stacked["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["kind"].as_str().unwrap().to_owned(),
                c["path"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn content_conflict_reports_then_resolves_to_a_side_or_supplied_bytes() {
    let root = fixture(&[("file", b"a\nb\nc\n")]);
    capture(
        root.path(),
        "source",
        &[("file", Edit::Text(b"a\nSOURCE\nc\n"))],
        "s",
    );
    capture(
        root.path(),
        "target",
        &[("file", Edit::Text(b"a\nTARGET\nc\n"))],
        "t",
    );
    let dir = TempDir::new().unwrap();

    // Conflicts change nothing and reserve no operation ID.
    let conflict = stack(root.path(), "stack", None, 3);
    assert_eq!(
        conflict_kinds(&conflict),
        [("Content".into(), "file".into())]
    );
    assert!(conflict["conflicts"][0]["base"].is_object());
    assert_eq!(head(root.path(), "target").1, 1);

    let ours = write_resolutions(dir.path(), "ours.json", json!({"file": "ours"}));
    let applied = stack(root.path(), "stack", Some(&ours), 0);
    assert_eq!(applied["update"]["outcome"], "applied");
    let (checkpoint, _) = head(root.path(), "target");
    assert_eq!(
        text_at(root.path(), &checkpoint, "file").unwrap(),
        b"a\nTARGET\nc\n"
    );

    // Supplied content is recorded as given, not as either side.
    let root = fixture(&[("file", b"a\nb\nc\n")]);
    capture(
        root.path(),
        "source",
        &[("file", Edit::Text(b"a\nSOURCE\nc\n"))],
        "s",
    );
    capture(
        root.path(),
        "target",
        &[("file", Edit::Text(b"a\nTARGET\nc\n"))],
        "t",
    );
    let content = dir.path().join("merged.txt");
    fs::write(&content, b"a\nMERGED\nc\n").unwrap();
    let supplied = write_resolutions(
        dir.path(),
        "content.json",
        json!({"file": {"file": content, "executable": false}}),
    );
    let applied = stack(root.path(), "stack", Some(&supplied), 0);
    assert_eq!(applied["update"]["outcome"], "applied");
    let (checkpoint, _) = head(root.path(), "target");
    assert_eq!(
        text_at(root.path(), &checkpoint, "file").unwrap(),
        b"a\nMERGED\nc\n"
    );
    ok(root.path(), &["check"]);
}

#[test]
fn modify_delete_conflict_resolves_to_keep_or_remove() {
    for (choice, present) in [("ours", true), ("theirs", false)] {
        let root = fixture(&[("file", b"one\n")]);
        capture(root.path(), "source", &[("file", Edit::Remove)], "s");
        capture(
            root.path(),
            "target",
            &[("file", Edit::Text(b"two\n"))],
            "t",
        );
        let dir = TempDir::new().unwrap();
        let conflict = stack(root.path(), "stack", None, 3);
        assert_eq!(
            conflict_kinds(&conflict),
            [("ModifyDelete".into(), "file".into())]
        );
        let file = write_resolutions(dir.path(), "r.json", json!({"file": choice}));
        let applied = stack(root.path(), "stack", Some(&file), 0);
        assert_eq!(applied["update"]["outcome"], "applied");
        let (checkpoint, _) = head(root.path(), "target");
        assert_eq!(entry(root.path(), &checkpoint, "file").is_some(), present);
        ok(root.path(), &["check"]);
    }
}

#[test]
fn symlink_against_regular_file_is_entry_type_and_resolves_to_the_symlink() {
    let root = fixture(&[("file", b"text\n")]);
    capture(
        root.path(),
        "source",
        &[("file", Edit::Link("elsewhere"))],
        "s",
    );
    capture(
        root.path(),
        "target",
        &[("file", Edit::Text(b"changed\n"))],
        "t",
    );
    let dir = TempDir::new().unwrap();
    let conflict = stack(root.path(), "stack", None, 3);
    assert_eq!(
        conflict_kinds(&conflict),
        [("EntryType".into(), "file".into())]
    );
    let file = write_resolutions(dir.path(), "r.json", json!({"file": "theirs"}));
    let applied = stack(root.path(), "stack", Some(&file), 0);
    assert_eq!(applied["update"]["outcome"], "applied");
    let (checkpoint, _) = head(root.path(), "target");
    assert!(matches!(
        entry(root.path(), &checkpoint, "file"),
        Some(FileEntry::Symlink { .. })
    ));
    ok(root.path(), &["check"]);
}

#[test]
fn mode_change_and_content_change_on_different_sides_combine_cleanly() {
    let root = fixture(&[("file", b"text\n")]);
    capture(
        root.path(),
        "source",
        &[("file", Edit::Exec(b"text\n"))],
        "s",
    );
    capture(
        root.path(),
        "target",
        &[("file", Edit::Text(b"changed\n"))],
        "t",
    );
    let stacked = stack(root.path(), "stack", None, 0);
    assert_eq!(stacked["update"]["outcome"], "applied");
    let (checkpoint, _) = head(root.path(), "target");
    assert_eq!(
        entry(root.path(), &checkpoint, "file"),
        Some(FileEntry::Regular {
            content: Repository::open(root.path())
                .unwrap()
                .put_blob(b"changed\n")
                .unwrap(),
            executable: true,
        })
    );
    ok(root.path(), &["check"]);
}

#[test]
fn rename_rename_conflict_is_reported_and_resolved_per_destination() {
    let root = fixture(&[("old.txt", b"same\n"), ("keep.txt", b"keep\n")]);
    capture(
        root.path(),
        "source",
        &[
            ("old.txt", Edit::Remove),
            ("left.txt", Edit::Text(b"same\n")),
        ],
        "s",
    );
    capture(
        root.path(),
        "target",
        &[
            ("old.txt", Edit::Remove),
            ("right.txt", Edit::Text(b"same\n")),
        ],
        "t",
    );
    let conflict = stack(root.path(), "stack", None, 3);
    let kinds = conflict_kinds(&conflict);
    assert!(
        kinds.iter().all(|(kind, _)| kind == "RenameRename"),
        "{kinds:?}"
    );
    assert_eq!(kinds.len(), 2, "{kinds:?}");
    assert!(conflict["conflicts"][0]["with"].is_string());
    // Keep `left.txt` by taking its side, and drop `right.txt` by taking the side without it.
    let dir = TempDir::new().unwrap();
    let file = write_resolutions(
        dir.path(),
        "r.json",
        json!({"left.txt": "theirs", "right.txt": "theirs"}),
    );
    let applied = stack(root.path(), "stack", Some(&file), 0);
    assert_eq!(applied["update"]["outcome"], "applied");
    let (checkpoint, _) = head(root.path(), "target");
    assert!(entry(root.path(), &checkpoint, "left.txt").is_some());
    assert!(entry(root.path(), &checkpoint, "right.txt").is_none());
    assert!(entry(root.path(), &checkpoint, "old.txt").is_none());
    assert!(entry(root.path(), &checkpoint, "keep.txt").is_some());
    ok(root.path(), &["check"]);
}

#[test]
fn file_and_directory_collision_is_path_prefix_and_resolves_per_path() {
    let root = fixture(&[("keep.txt", b"keep\n")]);
    capture(
        root.path(),
        "source",
        &[("dir/inner.txt", Edit::Text(b"inner\n"))],
        "s",
    );
    capture(
        root.path(),
        "target",
        &[("dir", Edit::Text(b"file\n"))],
        "t",
    );
    let dir = TempDir::new().unwrap();
    let conflict = stack(root.path(), "stack", None, 3);
    assert!(
        conflict_kinds(&conflict)
            .iter()
            .any(|(kind, _)| kind == "PathPrefix"),
        "{conflict}"
    );
    let file = write_resolutions(
        dir.path(),
        "r.json",
        json!({"dir": "theirs", "dir/inner.txt": "theirs"}),
    );
    let applied = stack(root.path(), "stack", Some(&file), 0);
    assert_eq!(applied["update"]["outcome"], "applied");
    ok(root.path(), &["check"]);
}

#[test]
fn case_alias_conflict_is_detected_on_a_case_insensitive_volume() {
    let probe = TempDir::new().unwrap();
    fs::write(probe.path().join("probe"), b"").unwrap();
    if !probe.path().join("PROBE").exists() {
        eprintln!("skipped: the test volume is case-sensitive");
        return;
    }
    let root = fixture(&[("keep.txt", b"keep\n")]);
    capture(
        root.path(),
        "source",
        &[("Readme.md", Edit::Text(b"a\n"))],
        "s",
    );
    capture(
        root.path(),
        "target",
        &[("readme.md", Edit::Text(b"b\n"))],
        "t",
    );
    let dir = TempDir::new().unwrap();
    let conflict = stack(root.path(), "stack", None, 3);
    assert!(
        conflict_kinds(&conflict)
            .iter()
            .all(|(kind, _)| kind == "Alias"),
        "{conflict}"
    );
    // Alias conflicts list the one present path; choosing its absence drops that name.
    let file = write_resolutions(dir.path(), "r.json", json!({"readme.md": "theirs"}));
    let applied = stack(root.path(), "stack", Some(&file), 0);
    assert_eq!(applied["update"]["outcome"], "applied");
    let (checkpoint, _) = head(root.path(), "target");
    assert!(entry(root.path(), &checkpoint, "readme.md").is_none());
    assert!(entry(root.path(), &checkpoint, "Readme.md").is_some());
    ok(root.path(), &["check"]);
}

#[test]
fn resolution_for_a_path_that_does_not_conflict_is_invalid_input() {
    let root = fixture(&[("file", b"a\nb\nc\n"), ("other.txt", b"x\n")]);
    capture(
        root.path(),
        "source",
        &[("file", Edit::Text(b"a\nSOURCE\nc\n"))],
        "s",
    );
    capture(
        root.path(),
        "target",
        &[("file", Edit::Text(b"a\nTARGET\nc\n"))],
        "t",
    );
    let dir = TempDir::new().unwrap();
    let file = write_resolutions(
        dir.path(),
        "r.json",
        json!({"file": "ours", "other.txt": "theirs"}),
    );
    let error = value(
        invoke(
            root.path(),
            &[
                "layer",
                "stack",
                "target",
                "--revision",
                "1",
                "--source",
                &head(root.path(), "source").0,
                "--operation",
                "stack",
                "--resolutions",
                file.to_str().unwrap(),
                "--json",
            ],
        ),
        2,
    );
    assert_eq!(error["error"]["code"], "invalid_input");
    // The refusal reserved nothing: the same operation ID now applies.
    let fixed = write_resolutions(dir.path(), "fixed.json", json!({"file": "ours"}));
    assert_eq!(
        stack(root.path(), "stack", Some(&fixed), 0)["update"]["outcome"],
        "applied"
    );
}

#[test]
fn resolved_stack_retries_replay_and_other_choices_are_refused() {
    let root = fixture(&[("file", b"a\nb\nc\n")]);
    capture(
        root.path(),
        "source",
        &[("file", Edit::Text(b"a\nSOURCE\nc\n"))],
        "s",
    );
    capture(
        root.path(),
        "target",
        &[("file", Edit::Text(b"a\nTARGET\nc\n"))],
        "t",
    );
    let dir = TempDir::new().unwrap();
    let ours = write_resolutions(dir.path(), "ours.json", json!({"file": "ours"}));
    let theirs = write_resolutions(dir.path(), "theirs.json", json!({"file": "theirs"}));

    // Retries name the original target revision: the target has since advanced.
    let first = stack_at(root.path(), 1, "stack", Some(&ours), 0);
    assert_eq!(first["update"]["outcome"], "applied");
    assert_eq!(stack_at(root.path(), 1, "stack", Some(&ours), 0), first);
    let reused = stack_at(root.path(), 1, "stack", Some(&theirs), 1);
    assert_eq!(reused["error"]["code"], "operation_id_reused");
    // Dropping the resolution is also a different input under the same ID.
    let plain = stack_at(root.path(), 1, "stack", None, 1);
    assert_eq!(plain["error"]["code"], "operation_id_reused");
    ok(root.path(), &["check"]);
}

/// World 1 holds `docs/readme.txt`. The Layer `work` has a directory whose uncaptured
/// edit conflicts with a stacked head that changed the same file.
fn refresh_fixture() -> (TempDir, PathBuf, TempDir) {
    let root = fixture(&[("docs/readme.txt", b"hello\n")]);
    let created = ok(
        root.path(),
        &["workspace", "create", "work", "--world", "1"],
    );
    let dir = PathBuf::from(created["workspace"]["root"].as_str().unwrap());
    capture(
        root.path(),
        "source",
        &[("docs/readme.txt", Edit::Text(b"theirs\n"))],
        "s",
    );
    // The stack reads the Layer's head, which the workspace edit does not touch yet.
    let stacked = ok(
        root.path(),
        &[
            "layer",
            "stack",
            "work",
            "--revision",
            "0",
            "--source",
            &head(root.path(), "source").0,
            "--operation",
            "stack-work",
        ],
    );
    assert_eq!(stacked["update"]["outcome"], "applied");
    fs::write(dir.join("docs/readme.txt"), b"mine\n").unwrap();
    (root, dir, TempDir::new().unwrap())
}

#[test]
fn refresh_conflict_blocks_publish_until_refresh_resolves_it() {
    let (root, dir, resolutions) = refresh_fixture();
    let refused = value(
        invoke(root.path(), &["workspace", "refresh", "work", "--json"]),
        1,
    );
    assert_eq!(refused["error"]["code"], "refresh_conflict");
    assert_eq!(fs::read(dir.join("docs/readme.txt")).unwrap(), b"mine\n");

    let blocked = value(
        invoke(
            root.path(),
            &[
                "workspace",
                "publish",
                "work",
                "--operation",
                "publish-work",
                "--json",
            ],
        ),
        1,
    );
    assert_eq!(blocked["error"]["code"], "refresh_conflict");
    assert_eq!(
        ok(root.path(), &["history"])["versions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let theirs = write_resolutions(
        resolutions.path(),
        "theirs.json",
        json!({"docs/readme.txt": "theirs"}),
    );
    ok(
        root.path(),
        &[
            "workspace",
            "refresh",
            "work",
            "--resolutions",
            theirs.to_str().unwrap(),
        ],
    );
    assert_eq!(fs::read(dir.join("docs/readme.txt")).unwrap(), b"theirs\n");
}

#[test]
fn refresh_resolved_to_ours_keeps_the_local_edit_and_publish_carries_it() {
    let (root, dir, resolutions) = refresh_fixture();
    let ours = write_resolutions(
        resolutions.path(),
        "ours.json",
        json!({"docs/readme.txt": "ours"}),
    );
    ok(
        root.path(),
        &[
            "workspace",
            "refresh",
            "work",
            "--resolutions",
            ours.to_str().unwrap(),
        ],
    );
    assert_eq!(fs::read(dir.join("docs/readme.txt")).unwrap(), b"mine\n");
    let published = ok(
        root.path(),
        &[
            "workspace",
            "publish",
            "work",
            "--operation",
            "publish-work",
        ],
    );
    assert_eq!(published["receipt"]["outcome"]["outcome"], "published");
    ok(root.path(), &["check"]);
}
