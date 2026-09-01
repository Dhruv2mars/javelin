use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_javelin"))
}

fn run(args: &[&str]) -> Output {
    let output = Command::new(binary()).args(args).output().unwrap();
    if !output.status.success() {
        panic!(
            "command failed ({:?}):\nstdout: {}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    output
}

fn output_text(output: Output) -> String {
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn init() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let world = temp.path().join("world");
    run(&["init", world.to_str().unwrap()]);
    (temp, world)
}

fn in_world(world: &Path, args: &[&str]) -> Output {
    let mut complete = vec!["--project", world.to_str().unwrap()];
    complete.extend_from_slice(args);
    run(&complete)
}

#[test]
fn init_is_idempotent_and_existing_content_is_world_v1() {
    let temp = tempfile::tempdir().unwrap();
    let world = temp.path().join("existing");
    fs::create_dir_all(world.join("empty")).unwrap();
    fs::write(world.join("hello.txt"), b"hello\n").unwrap();
    run(&["init", world.to_str().unwrap()]);
    run(&["init", world.to_str().unwrap()]);
    assert_eq!(
        output_text(in_world(&world, &["show", "v1:hello.txt"])),
        "hello"
    );
    let current: Value =
        serde_json::from_slice(&in_world(&world, &["world", "current", "--json"]).stdout).unwrap();
    assert_eq!(current["result"]["id"], "v1");
    in_world(&world, &["fsck"]);
}

#[test]
fn unchanged_status_uses_observation_and_dropped_events_still_reconcile() {
    let temp = tempfile::tempdir().unwrap();
    let world = temp.path().join("world");
    fs::create_dir_all(&world).unwrap();
    fs::write(world.join("tracked.txt"), b"before\n").unwrap();

    let init = Command::new(binary())
        .args(["init", world.to_str().unwrap()])
        .env("JAVELIN_MONITOR_CHILD", "1")
        .output()
        .unwrap();
    assert!(init.status.success());
    let baseline = Command::new(binary())
        .args(["--project", world.to_str().unwrap(), "status"])
        .env("JAVELIN_MONITOR_CHILD", "1")
        .output()
        .unwrap();
    assert!(baseline.status.success());

    let unchanged = Command::new(binary())
        .args(["--project", world.to_str().unwrap(), "status"])
        .env("JAVELIN_MONITOR_CHILD", "1")
        .env("JAVELIN_FAULT_POINT", "before_object_temp_write")
        .output()
        .unwrap();
    assert!(unchanged.status.success());

    fs::write(world.join("tracked.txt"), b"after\n").unwrap();
    let changed = Command::new(binary())
        .args(["--project", world.to_str().unwrap(), "status"])
        .env("JAVELIN_MONITOR_CHILD", "1")
        .env("JAVELIN_FAULT_POINT", "before_object_temp_write")
        .output()
        .unwrap();
    assert_eq!(changed.status.code(), Some(86));

    let reconciled = Command::new(binary())
        .args(["--project", world.to_str().unwrap(), "status"])
        .env("JAVELIN_MONITOR_CHILD", "1")
        .output()
        .unwrap();
    assert!(reconciled.status.success());
    assert!(
        String::from_utf8(reconciled.stdout)
            .unwrap()
            .contains("tracked.txt")
    );
}

#[test]
fn independent_layers_publish_without_lost_updates() {
    let (_temp, world) = init();
    fs::write(world.join("base.txt"), b"base\n").unwrap();
    in_world(&world, &["publish", "--idempotency-key", "base"]);
    let alpha = output_text(in_world(
        &world,
        &["layer", "create", "alpha", "--from", "world"],
    ));
    let beta = output_text(in_world(
        &world,
        &["layer", "create", "beta", "--from", "world"],
    ));
    fs::write(Path::new(&alpha).join("alpha.txt"), b"alpha\n").unwrap();
    fs::write(Path::new(&beta).join("beta.txt"), b"beta\n").unwrap();
    assert!(!Path::new(&alpha).join("beta.txt").exists());
    assert!(!Path::new(&beta).join("alpha.txt").exists());
    in_world(&world, &["publish", "alpha", "--idempotency-key", "alpha"]);
    in_world(&world, &["publish", "beta", "--idempotency-key", "beta"]);
    assert_eq!(
        output_text(in_world(&world, &["show", "world:alpha.txt"])),
        "alpha"
    );
    assert_eq!(
        output_text(in_world(&world, &["show", "world:beta.txt"])),
        "beta"
    );
    let history: Value =
        serde_json::from_slice(&in_world(&world, &["world", "history", "--json"]).stdout).unwrap();
    assert_eq!(history["result"]["versions"].as_array().unwrap().len(), 4);
}

#[test]
fn conflict_preserves_base_target_private_and_exit_code() {
    let (_temp, world) = init();
    fs::write(world.join("shared.txt"), b"base\n").unwrap();
    in_world(&world, &["publish", "--idempotency-key", "base"]);
    let left = output_text(in_world(
        &world,
        &["layer", "create", "left", "--from", "world"],
    ));
    let right = output_text(in_world(
        &world,
        &["layer", "create", "right", "--from", "world"],
    ));
    fs::write(Path::new(&left).join("shared.txt"), b"left\n").unwrap();
    fs::write(Path::new(&right).join("shared.txt"), b"right\n").unwrap();
    in_world(&world, &["publish", "left", "--idempotency-key", "left"]);
    let failed = Command::new(binary())
        .args([
            "--project",
            world.to_str().unwrap(),
            "publish",
            "right",
            "--idempotency-key",
            "right",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(failed.status.code(), Some(4));
    let error: Value = serde_json::from_slice(&failed.stderr).unwrap();
    assert_eq!(error["error"]["code"], "CONFLICT");
    let conflicts: Value =
        serde_json::from_slice(&in_world(&world, &["conflict", "list", "right", "--json"]).stdout)
            .unwrap();
    let conflict = &conflicts["result"]["conflicts"][0];
    assert!(conflict["base"]["object_id"].is_string());
    assert!(conflict["target"]["object_id"].is_string());
    assert!(conflict["private"]["object_id"].is_string());
}

#[test]
fn resolving_a_conflict_twice_does_not_append_another_checkpoint() {
    let (_temp, world) = init();
    fs::write(world.join("shared.txt"), b"base\n").unwrap();
    in_world(&world, &["publish", "--idempotency-key", "base"]);
    let left = output_text(in_world(
        &world,
        &["layer", "create", "left", "--from", "world"],
    ));
    let right = output_text(in_world(
        &world,
        &["layer", "create", "right", "--from", "world"],
    ));
    fs::write(Path::new(&left).join("shared.txt"), b"left\n").unwrap();
    fs::write(Path::new(&right).join("shared.txt"), b"right\n").unwrap();
    in_world(&world, &["publish", "left", "--idempotency-key", "left"]);
    let failed = Command::new(binary())
        .args([
            "--project",
            world.to_str().unwrap(),
            "publish",
            "right",
            "--idempotency-key",
            "right",
        ])
        .output()
        .unwrap();
    assert_eq!(failed.status.code(), Some(4));
    let conflict_id = output_text(in_world(&world, &["conflict", "list", "right"]))
        .split('\t')
        .next()
        .unwrap()
        .to_string();
    in_world(
        &world,
        &["conflict", "resolve", &conflict_id, "--use", "private"],
    );
    let before: Value = serde_json::from_slice(
        &in_world(&world, &["history", "--layer", "right", "--json"]).stdout,
    )
    .unwrap();
    let rejected = Command::new(binary())
        .args([
            "--project",
            world.to_str().unwrap(),
            "conflict",
            "resolve",
            &conflict_id,
            "--use",
            "private",
        ])
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(2));
    let after: Value = serde_json::from_slice(
        &in_world(&world, &["history", "--layer", "right", "--json"]).stdout,
    )
    .unwrap();
    assert_eq!(
        before["result"]["checkpoints"],
        after["result"]["checkpoints"]
    );
}

#[test]
fn diff_honors_every_path_filter() {
    let (_temp, world) = init();
    fs::write(world.join("first.txt"), b"first\n").unwrap();
    fs::write(world.join("second.txt"), b"second\n").unwrap();
    fs::write(world.join("third.txt"), b"third\n").unwrap();

    let result: Value = serde_json::from_slice(
        &in_world(&world, &["--json", "diff", "--", "first.txt", "second.txt"]).stdout,
    )
    .unwrap();
    let paths = result["result"]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| change["path"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(paths, ["first.txt", "second.txt"]);
}

#[test]
fn claims_use_restricted_path_grammar_and_plain_human_output() {
    let (_temp, world) = init();
    in_world(
        &world,
        &[
            "layer", "create", "valid", "--from", "world", "--claim", "src/**",
        ],
    );
    let listed = output_text(in_world(&world, &["claim", "list"]));
    assert!(listed.contains("\tvalid\tsrc/**"));
    assert!(!listed.contains('"'));

    let rejected = Command::new(binary())
        .args([
            "--project",
            world.to_str().unwrap(),
            "layer",
            "create",
            "invalid",
            "--from",
            "world",
            "--claim",
            "src/*.rs",
        ])
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(2));
}

#[test]
fn refresh_reports_case_fold_collision_as_conflict() {
    let (_temp, world) = init();
    let upper = output_text(in_world(
        &world,
        &["layer", "create", "upper", "--from", "world"],
    ));
    let lower = output_text(in_world(
        &world,
        &["layer", "create", "lower", "--from", "world"],
    ));
    fs::write(Path::new(&upper).join("Name.txt"), b"upper\n").unwrap();
    fs::write(Path::new(&lower).join("name.txt"), b"lower\n").unwrap();
    in_world(&world, &["publish", "upper", "--idempotency-key", "upper"]);
    let failed = Command::new(binary())
        .args([
            "--project",
            world.to_str().unwrap(),
            "publish",
            "lower",
            "--idempotency-key",
            "lower",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(failed.status.code(), Some(4));
    let conflicts: Value =
        serde_json::from_slice(&in_world(&world, &["conflict", "list", "lower", "--json"]).stdout)
            .unwrap();
    assert!(
        conflicts["result"]["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|conflict| conflict["type"] == "case")
    );
}

#[test]
fn monitor_records_stable_writes_without_explicit_checkpoint() {
    let (_temp, world) = init();
    fs::write(world.join("automatic.txt"), b"captured\n").unwrap();
    let started = Instant::now();
    loop {
        let history: Value = serde_json::from_slice(
            &in_world(&world, &["history", "--layer", "local", "--json"]).stdout,
        )
        .unwrap();
        if history["result"]["checkpoints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|checkpoint| checkpoint["reason"] == "automatic")
        {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn ignore_policy_change_cannot_hide_or_include_files_in_same_contribution() {
    let (_temp, world) = init();
    let policy_path = world.join(".javelinignore");
    let policy = fs::read_to_string(&policy_path)
        .unwrap()
        .lines()
        .filter(|line| *line != ".env")
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&policy_path, format!("{policy}\n")).unwrap();
    fs::write(world.join(".env"), b"TRACK_AFTER_POLICY_ACCEPTS=1\n").unwrap();
    in_world(&world, &["publish", "--idempotency-key", "policy-first"]);
    let absent = Command::new(binary())
        .args(["--project", world.to_str().unwrap(), "show", "v2:.env"])
        .output()
        .unwrap();
    assert_eq!(absent.status.code(), Some(2));
    in_world(&world, &["publish", "--idempotency-key", "file-second"]);
    assert_eq!(
        output_text(in_world(&world, &["show", "v3:.env"])),
        "TRACK_AFTER_POLICY_ACCEPTS=1"
    );
}

#[test]
fn parent_discard_requires_explicit_reparent_and_preserves_child() {
    let (_temp, world) = init();
    in_world(&world, &["layer", "create", "parent", "--from", "world"]);
    in_world(
        &world,
        &[
            "layer",
            "create",
            "child",
            "--from",
            "layer:parent",
            "--target",
            "layer:parent",
        ],
    );
    let rejected = Command::new(binary())
        .args(["--project", world.to_str().unwrap(), "discard", "parent"])
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(10));
    in_world(&world, &["discard", "parent", "--reparent", "world"]);
    let child: Value =
        serde_json::from_slice(&in_world(&world, &["layer", "show", "child", "--json"]).stdout)
            .unwrap();
    assert_eq!(child["result"]["layer"]["target_kind"], "world");
    assert_eq!(child["result"]["layer"]["status"], "active");
    in_world(&world, &["discarded", "recover", "parent"]);
}

#[test]
#[allow(
    clippy::permissions_set_readonly_false,
    reason = "the Windows test must clear the readonly file attribute before corrupting the cache"
)]
fn repair_rebuilds_corrupted_view_and_root_cache_from_objects() {
    let (_temp, world) = init();
    let layer_path = output_text(in_world(
        &world,
        &["layer", "create", "repairable", "--from", "world"],
    ));
    fs::write(Path::new(&layer_path).join("safe.txt"), b"canonical\n").unwrap();
    run(&["--project", &layer_path, "checkpoint", "--reason", "safe"]);
    let shown: Value = serde_json::from_slice(
        &in_world(&world, &["layer", "show", "repairable", "--json"]).stdout,
    )
    .unwrap();
    let root = shown["result"]["head"]["root_tree"].as_str().unwrap();
    in_world(&world, &["repair", "--view", "repairable"]);
    let cached_file = world
        .join(".javelin/materialized")
        .join(root)
        .join("safe.txt");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&cached_file, fs::Permissions::from_mode(0o644)).unwrap();
    }
    #[cfg(not(unix))]
    {
        let mut cache_permissions = fs::metadata(&cached_file).unwrap().permissions();
        cache_permissions.set_readonly(false);
        fs::set_permissions(&cached_file, cache_permissions).unwrap();
    }
    fs::write(&cached_file, b"corrupt cache\n").unwrap();
    fs::write(Path::new(&layer_path).join("safe.txt"), b"corrupt view\n").unwrap();
    in_world(&world, &["repair", "--view", "repairable"]);
    assert_eq!(
        fs::read(Path::new(&layer_path).join("safe.txt")).unwrap(),
        b"canonical\n"
    );
}

#[cfg(unix)]
#[test]
fn checkpoints_capture_symlink_executable_and_empty_directory() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (_temp, world) = init();
    fs::create_dir(world.join("empty")).unwrap();
    fs::write(world.join("run.sh"), b"#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(world.join("run.sh")).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(world.join("run.sh"), permissions).unwrap();
    symlink("run.sh", world.join("run-link")).unwrap();
    in_world(&world, &["checkpoint", "--reason", "portable-metadata"]);
    let shown: Value =
        serde_json::from_slice(&in_world(&world, &["show", "local", "--json"]).stdout).unwrap();
    let entries = shown["result"]["tree"]["entries"].as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry["path"] == "empty" && entry["kind"] == "directory")
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry["path"] == "run.sh" && entry["executable"] == true)
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry["path"] == "run-link" && entry["kind"] == "symlink")
    );
}

#[test]
fn injected_publish_crashes_leave_one_repairable_world() {
    let fault_points = [
        "before_object_temp_write",
        "after_object_fsync",
        "before_object_rename",
        "after_object_rename",
        "before_publish_lease",
        "after_candidate_construction",
        "during_verification",
        "before_db_transaction",
        "inside_transaction_before_current_pointer_update",
        "after_current_pointer_update_before_commit",
        "before_event_delivery",
        "after_db_commit_before_view_update",
        "during_view_update",
    ];
    for point in fault_points {
        let (_temp, world) = init();
        let mut policy = fs::read_to_string(world.join("javelin.toml")).unwrap();
        policy.push_str(
            "\n[[verification.rule]]\nname = \"truth\"\ncommand = [\"test\", \"-f\", \"pass.txt\"]\nrequired = true\ntimeout_seconds = 5\n",
        );
        fs::write(world.join("javelin.toml"), policy).unwrap();
        fs::write(world.join("pass.txt"), b"pass\n").unwrap();
        in_world(&world, &["publish", "--idempotency-key", "policy"]);
        let layer = output_text(in_world(
            &world,
            &["layer", "create", "fault", "--from", "world"],
        ));
        fs::write(Path::new(&layer).join("change.txt"), point.as_bytes()).unwrap();
        let crashed = Command::new(binary())
            .args([
                "--project",
                world.to_str().unwrap(),
                "publish",
                "fault",
                "--idempotency-key",
                point,
            ])
            .env("JAVELIN_FAULT_POINT", point)
            .output()
            .unwrap();
        assert_eq!(
            crashed.status.code(),
            Some(86),
            "fault {point} did not terminate at its boundary: {}",
            String::from_utf8_lossy(&crashed.stderr)
        );
        in_world(&world, &["fsck"]);
        in_world(&world, &["repair"]);
        in_world(&world, &["publish", "fault", "--idempotency-key", point]);
        let current: Value =
            serde_json::from_slice(&in_world(&world, &["world", "current", "--json"]).stdout)
                .unwrap();
        assert_eq!(current["result"]["id"], "v3", "fault {point}");
    }
}

#[test]
fn nested_child_publish_parent_publish_and_idempotent_retry_are_linear() {
    let (_temp, world) = init();
    let parent = output_text(in_world(
        &world,
        &["layer", "create", "feature", "--from", "world"],
    ));
    let child = output_text(in_world(
        &world,
        &[
            "layer",
            "create",
            "api",
            "--from",
            "layer:feature",
            "--target",
            "layer:feature",
        ],
    ));
    fs::write(Path::new(&child).join("api.ts"), b"export const api = 1;\n").unwrap();
    in_world(
        &world,
        &["publish", "api", "--idempotency-key", "child-api"],
    );
    assert_eq!(
        fs::read(Path::new(&parent).join("api.ts")).unwrap(),
        b"export const api = 1;\n"
    );
    in_world(
        &world,
        &["publish", "feature", "--idempotency-key", "feature-world"],
    );
    in_world(
        &world,
        &["publish", "feature", "--idempotency-key", "feature-world"],
    );
    let history: Value =
        serde_json::from_slice(&in_world(&world, &["world", "history", "--json"]).stdout).unwrap();
    assert_eq!(history["result"]["versions"].as_array().unwrap().len(), 2);
    assert_eq!(
        output_text(in_world(&world, &["show", "world:api.ts"])),
        "export const api = 1;"
    );
}

#[test]
fn idempotent_child_publish_repairs_parent_view_after_commit_crash() {
    let (_temp, world) = init();
    let parent = output_text(in_world(
        &world,
        &["layer", "create", "parent", "--from", "world"],
    ));
    let child = output_text(in_world(
        &world,
        &[
            "layer",
            "create",
            "child",
            "--from",
            "layer:parent",
            "--target",
            "layer:parent",
        ],
    ));
    fs::write(Path::new(&child).join("child.txt"), b"accepted\n").unwrap();

    let crashed = Command::new(binary())
        .args([
            "--project",
            world.to_str().unwrap(),
            "publish",
            "child",
            "--idempotency-key",
            "child-crash-repair",
        ])
        .env("JAVELIN_FAULT_POINT", "after_db_commit_before_view_update")
        .output()
        .unwrap();
    assert_eq!(crashed.status.code(), Some(86));
    assert!(!Path::new(&parent).join("child.txt").exists());

    in_world(
        &world,
        &[
            "publish",
            "child",
            "--idempotency-key",
            "child-crash-repair",
        ],
    );
    assert_eq!(
        fs::read(Path::new(&parent).join("child.txt")).unwrap(),
        b"accepted\n"
    );
}

#[test]
fn retrying_an_older_publish_key_does_not_roll_back_an_advanced_layer() {
    let (_temp, world) = init();
    let layer = output_text(in_world(
        &world,
        &["layer", "create", "advancing", "--from", "world"],
    ));
    fs::write(Path::new(&layer).join("state.txt"), b"one\n").unwrap();
    in_world(
        &world,
        &["publish", "advancing", "--idempotency-key", "key-a"],
    );
    fs::write(Path::new(&layer).join("state.txt"), b"two\n").unwrap();
    in_world(
        &world,
        &["publish", "advancing", "--idempotency-key", "key-b"],
    );
    let before: Value =
        serde_json::from_slice(&in_world(&world, &["layer", "show", "advancing", "--json"]).stdout)
            .unwrap();

    in_world(
        &world,
        &["publish", "advancing", "--idempotency-key", "key-a"],
    );

    let after: Value =
        serde_json::from_slice(&in_world(&world, &["layer", "show", "advancing", "--json"]).stdout)
            .unwrap();
    assert_eq!(before["result"]["head"], after["result"]["head"]);
    assert_eq!(
        fs::read(Path::new(&layer).join("state.txt")).unwrap(),
        b"two\n"
    );
}

#[test]
fn idempotency_key_cannot_alias_a_different_layer() {
    let (_temp, world) = init();
    let first = output_text(in_world(
        &world,
        &["layer", "create", "first", "--from", "world"],
    ));
    let second = output_text(in_world(
        &world,
        &["layer", "create", "second", "--from", "world"],
    ));
    fs::write(Path::new(&first).join("first.txt"), b"first\n").unwrap();
    fs::write(Path::new(&second).join("second.txt"), b"second\n").unwrap();
    in_world(
        &world,
        &["publish", "first", "--idempotency-key", "shared-key"],
    );

    let rejected = Command::new(binary())
        .args([
            "--project",
            world.to_str().unwrap(),
            "publish",
            "second",
            "--idempotency-key",
            "shared-key",
        ])
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(6));
    let current: Value =
        serde_json::from_slice(&in_world(&world, &["world", "current", "--json"]).stdout).unwrap();
    assert_eq!(current["result"]["id"], "v2");
}

#[test]
fn copied_view_marker_cannot_redirect_project_discovery() {
    let (temp, world) = init();
    let layer = output_text(in_world(
        &world,
        &["layer", "create", "real", "--from", "world"],
    ));
    let forged = temp.path().join("forged-view");
    fs::create_dir_all(&forged).unwrap();
    fs::copy(
        Path::new(&layer).join(".javelin-view"),
        forged.join(".javelin-view"),
    )
    .unwrap();

    let rejected = Command::new(binary())
        .args(["--project", forged.to_str().unwrap(), "status"])
        .env("JAVELIN_MONITOR_CHILD", "1")
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(7));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("view marker does not match"));
}

#[test]
fn published_discarded_layer_can_be_purged_without_losing_contribution() {
    let (_temp, world) = init();
    let layer = output_text(in_world(
        &world,
        &["layer", "create", "published", "--from", "world"],
    ));
    fs::write(Path::new(&layer).join("published.txt"), b"accepted\n").unwrap();
    in_world(
        &world,
        &[
            "publish",
            "published",
            "--idempotency-key",
            "published-layer",
        ],
    );
    in_world(&world, &["discard", "published"]);
    in_world(&world, &["discarded", "purge", "published"]);
    in_world(&world, &["fsck"]);
    assert_eq!(
        output_text(in_world(&world, &["show", "world:published.txt"])),
        "accepted"
    );

    let replacement = output_text(in_world(
        &world,
        &["layer", "create", "replacement", "--from", "world"],
    ));
    fs::write(
        Path::new(&replacement).join("replacement.txt"),
        b"must not publish\n",
    )
    .unwrap();
    let rejected = Command::new(binary())
        .args([
            "--project",
            world.to_str().unwrap(),
            "publish",
            "replacement",
            "--idempotency-key",
            "published-layer",
        ])
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(6));
    assert!(!world.join("replacement.txt").exists());
}

#[test]
fn claim_prefix_matches_path_segments_only() {
    let (_temp, world) = init();
    in_world(
        &world,
        &[
            "layer", "create", "prefix", "--from", "world", "--claim", "a/**",
        ],
    );
    in_world(
        &world,
        &[
            "layer", "create", "sibling", "--from", "world", "--claim", "a-b",
        ],
    );
    let claims: Value =
        serde_json::from_slice(&in_world(&world, &["claim", "list", "--json"]).stdout).unwrap();
    assert!(claims["result"]["overlaps"].as_array().unwrap().is_empty());
}

#[test]
fn provenance_search_treats_wildcards_as_literal_text() {
    let (_temp, world) = init();
    in_world(&world, &["provenance", "begin", "--actor", "percent%agent"]);
    in_world(
        &world,
        &["provenance", "begin", "--actor", "ordinary-agent"],
    );

    let result: Value =
        serde_json::from_slice(&in_world(&world, &["provenance", "search", "%", "--json"]).stdout)
            .unwrap();
    let sessions = result["result"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["actor"]["name"], "percent%agent");
}

#[test]
fn provenance_rejects_attachments_after_end_or_purge_and_unknown_purge() {
    let (_temp, world) = init();
    let attachment = world.join("trace.jsonl");
    fs::write(&attachment, b"{}\n").unwrap();

    let ended = output_text(in_world(
        &world,
        &["provenance", "begin", "--actor", "ended-agent"],
    ));
    in_world(&world, &["provenance", "end", &ended]);
    let rejected_ended = Command::new(binary())
        .args([
            "--project",
            world.to_str().unwrap(),
            "provenance",
            "attach",
            "--session",
            &ended,
            attachment.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(rejected_ended.status.code(), Some(10));

    let purged = output_text(in_world(
        &world,
        &["provenance", "begin", "--actor", "purged-agent"],
    ));
    in_world(&world, &["provenance", "purge", &purged]);
    let rejected_purged = Command::new(binary())
        .args([
            "--project",
            world.to_str().unwrap(),
            "provenance",
            "attach",
            "--session",
            &purged,
            attachment.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(rejected_purged.status.code(), Some(10));

    let unknown = Command::new(binary())
        .args([
            "--project",
            world.to_str().unwrap(),
            "provenance",
            "purge",
            "unknown-session",
        ])
        .output()
        .unwrap();
    assert_eq!(unknown.status.code(), Some(2));
}

#[cfg(unix)]
#[test]
fn candidate_timeout_change_is_not_deduplicated() {
    let (_temp, world) = init();
    let mut config = fs::read_to_string(world.join("javelin.toml")).unwrap();
    config.push_str(
        "\n[[verification.rule]]\nname = \"timeout-policy\"\ncommand = [\"sh\", \"-c\", \"sleep 2\"]\nrequired = true\ntimeout_seconds = 3\n",
    );
    fs::write(world.join("javelin.toml"), &config).unwrap();
    in_world(
        &world,
        &["publish", "--idempotency-key", "timeout-policy-base"],
    );

    fs::write(
        world.join("javelin.toml"),
        config.replace("timeout_seconds = 3", "timeout_seconds = 1"),
    )
    .unwrap();
    let rejected = Command::new(binary())
        .args([
            "--project",
            world.to_str().unwrap(),
            "publish",
            "--idempotency-key",
            "timeout-policy-candidate",
        ])
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(5));
}

#[test]
fn discard_preserves_world_and_supports_recover_and_exact_purge() {
    let (_temp, world) = init();
    fs::write(world.join("accepted.txt"), b"accepted\n").unwrap();
    in_world(&world, &["publish", "--idempotency-key", "accepted"]);
    let before: Value =
        serde_json::from_slice(&in_world(&world, &["world", "current", "--json"]).stdout).unwrap();
    let experiment = output_text(in_world(
        &world,
        &["layer", "create", "experiment", "--from", "world"],
    ));
    fs::write(Path::new(&experiment).join("accepted.txt"), b"tentative\n").unwrap();
    in_world(&world, &["discard", "experiment"]);
    let after: Value =
        serde_json::from_slice(&in_world(&world, &["world", "current", "--json"]).stdout).unwrap();
    assert_eq!(before["result"]["root_tree"], after["result"]["root_tree"]);
    assert_eq!(
        output_text(in_world(&world, &["show", "world:accepted.txt"])),
        "accepted"
    );
    in_world(&world, &["discarded", "recover", "experiment"]);
    assert_eq!(
        fs::read(Path::new(&experiment).join("accepted.txt")).unwrap(),
        b"tentative\n"
    );
    in_world(&world, &["discard", "experiment"]);
    in_world(&world, &["discarded", "purge", "experiment"]);
    let missing = Command::new(binary())
        .args([
            "--project",
            world.to_str().unwrap(),
            "layer",
            "show",
            "experiment",
        ])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(2));
}

#[test]
fn local_discard_preserves_reserved_and_ignored_content() {
    let (_temp, world) = init();
    fs::write(world.join("tracked.txt"), b"accepted\n").unwrap();
    in_world(&world, &["publish", "--idempotency-key", "accepted"]);
    fs::create_dir_all(world.join(".git")).unwrap();
    fs::write(world.join(".git/config"), b"foreign metadata\n").unwrap();
    fs::create_dir_all(world.join("node_modules/pkg")).unwrap();
    fs::write(world.join("node_modules/pkg/cache"), b"ignored cache\n").unwrap();
    fs::write(world.join("tracked.txt"), b"tentative\n").unwrap();

    in_world(&world, &["discard"]);

    assert_eq!(fs::read(world.join("tracked.txt")).unwrap(), b"accepted\n");
    assert_eq!(
        fs::read(world.join(".git/config")).unwrap(),
        b"foreign metadata\n"
    );
    assert_eq!(
        fs::read(world.join("node_modules/pkg/cache")).unwrap(),
        b"ignored cache\n"
    );
    assert!(world.join(".javelin/store.sqlite3").is_file());
}

#[test]
fn required_failure_blocks_while_informational_failure_is_recorded() {
    let (_temp, world) = init();
    let executable = binary().to_string_lossy().replace('\\', "\\\\");
    let mut config = fs::read_to_string(world.join("javelin.toml")).unwrap();
    config.push_str(&format!(
        "\n[[verification.rule]]\nname = \"gate\"\ncommand = [\"{executable}\", \"not-a-command\"]\nrequired = true\ntimeout_seconds = 10\n"
    ));
    fs::write(world.join("javelin.toml"), config).unwrap();
    let rejected = Command::new(binary())
        .args([
            "--project",
            world.to_str().unwrap(),
            "publish",
            "--idempotency-key",
            "bad-policy",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(5));
    let current: Value =
        serde_json::from_slice(&in_world(&world, &["world", "current", "--json"]).stdout).unwrap();
    assert_eq!(current["result"]["id"], "v1");

    let base = fs::read_to_string(world.join("javelin.toml")).unwrap();
    let fixed = base
        .replace(
            "name = \"gate\"\ncommand = [",
            "name = \"information\"\ncommand = [",
        )
        .replace("required = true", "required = false");
    fs::write(world.join("javelin.toml"), fixed).unwrap();
    let accepted: Value = serde_json::from_slice(
        &in_world(
            &world,
            &["publish", "--idempotency-key", "informational", "--json"],
        )
        .stdout,
    )
    .unwrap();
    assert_eq!(accepted["result"]["resulting_target_ref"], "v2");
    assert_eq!(accepted["result"]["validations"][0]["exit_code"], 2);
    assert_eq!(accepted["result"]["validations"][0]["required"], false);
}

#[cfg(unix)]
#[test]
fn startup_recovery_removes_abandoned_temp_and_dead_queue_entry() {
    let (_temp, world) = init();
    let pid: i32 = fs::read_to_string(world.join(".javelin/monitor/pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    unsafe { libc::kill(pid, libc::SIGTERM) };
    thread::sleep(Duration::from_millis(100));
    let abandoned = world.join(".javelin/temp/abandoned.tmp");
    fs::write(&abandoned, b"partial").unwrap();
    let database = world.join(".javelin/store.sqlite3");
    let connection = rusqlite::Connection::open(database).unwrap();
    connection
        .execute(
            "INSERT INTO publish_queue(request_id, target, pid, created_at)
             VALUES ('dead-request', 'world', 2147483647, '2000-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    drop(connection);
    let recovered = Command::new(binary())
        .args(["--project", world.to_str().unwrap(), "doctor"])
        .env("JAVELIN_STARTUP_TEMP_GRACE_SECONDS", "0")
        .output()
        .unwrap();
    assert!(recovered.status.success());
    assert!(!abandoned.exists());
    let connection = rusqlite::Connection::open(world.join(".javelin/store.sqlite3")).unwrap();
    let queued: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM publish_queue WHERE request_id = 'dead-request'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(queued, 0);
}

#[cfg(unix)]
#[test]
fn fsck_detects_corrupt_copied_store_without_damaging_original() {
    fn copy_tree(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let source_path = entry.path();
            let destination_path = destination.join(entry.file_name());
            let metadata = fs::symlink_metadata(&source_path).unwrap();
            if metadata.file_type().is_dir() {
                copy_tree(&source_path, &destination_path);
            } else if metadata.file_type().is_symlink() {
                std::os::unix::fs::symlink(fs::read_link(&source_path).unwrap(), destination_path)
                    .unwrap();
            } else {
                fs::copy(source_path, destination_path).unwrap();
            }
        }
    }

    let (temp, world) = init();
    fs::write(world.join("payload.txt"), b"canonical bytes\n").unwrap();
    in_world(&world, &["publish", "--idempotency-key", "canonical"]);
    let pid: i32 = fs::read_to_string(world.join(".javelin/monitor/pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    unsafe { libc::kill(pid, libc::SIGTERM) };
    thread::sleep(Duration::from_millis(100));
    let connection = rusqlite::Connection::open(world.join(".javelin/store.sqlite3")).unwrap();
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    drop(connection);

    let copied = temp.path().join("copied-world");
    copy_tree(&world, &copied);
    let objects = copied.join(".javelin/objects");
    let object = fs::read_dir(&objects)
        .unwrap()
        .flat_map(|shard| fs::read_dir(shard.unwrap().path()).unwrap())
        .map(|entry| entry.unwrap().path())
        .next()
        .unwrap();
    let mut bytes = fs::read(&object).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(&object, bytes).unwrap();

    let corrupted = Command::new(binary())
        .args(["--project", copied.to_str().unwrap(), "fsck", "--json"])
        .env("JAVELIN_MONITOR_CHILD", "1")
        .output()
        .unwrap();
    assert_eq!(corrupted.status.code(), Some(7));
    let error: Value = serde_json::from_slice(&corrupted.stderr).unwrap();
    assert!(matches!(
        error["error"]["code"].as_str(),
        Some("STORAGE_CORRUPTION" | "CORRUPT_OBJECT")
    ));
    let original = Command::new(binary())
        .args(["--project", world.to_str().unwrap(), "fsck"])
        .env("JAVELIN_MONITOR_CHILD", "1")
        .output()
        .unwrap();
    assert!(original.status.success());
}

#[test]
fn fsck_reports_missing_object_metadata_without_repairing_it() {
    let (_temp, world) = init();
    fs::write(world.join("tracked.txt"), b"tracked\n").unwrap();
    in_world(&world, &["publish", "--idempotency-key", "tracked"]);
    let database = world.join(".javelin/store.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    let object_id: String = connection
        .query_row(
            "SELECT id FROM object_metadata WHERE kind = 'blob' ORDER BY id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute("DELETE FROM object_metadata WHERE id = ?1", [&object_id])
        .unwrap();
    drop(connection);

    let checked = Command::new(binary())
        .args(["--project", world.to_str().unwrap(), "fsck"])
        .env("JAVELIN_MONITOR_CHILD", "1")
        .output()
        .unwrap();
    assert_eq!(checked.status.code(), Some(7));
    let connection = rusqlite::Connection::open(database).unwrap();
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM object_metadata WHERE id = ?1",
            [&object_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn corrupt_conflict_entry_is_reported_instead_of_treated_as_deletion() {
    let (_temp, world) = init();
    fs::write(world.join("shared.txt"), b"base\n").unwrap();
    in_world(&world, &["publish", "--idempotency-key", "base"]);
    let left = output_text(in_world(
        &world,
        &["layer", "create", "left-corrupt", "--from", "world"],
    ));
    let right = output_text(in_world(
        &world,
        &["layer", "create", "right-corrupt", "--from", "world"],
    ));
    fs::write(Path::new(&left).join("shared.txt"), b"left\n").unwrap();
    fs::write(Path::new(&right).join("shared.txt"), b"right\n").unwrap();
    in_world(
        &world,
        &[
            "publish",
            "left-corrupt",
            "--idempotency-key",
            "left-corrupt",
        ],
    );
    let failed = Command::new(binary())
        .args([
            "--project",
            world.to_str().unwrap(),
            "publish",
            "right-corrupt",
            "--idempotency-key",
            "right-corrupt",
        ])
        .output()
        .unwrap();
    assert_eq!(failed.status.code(), Some(4));
    let database = world.join(".javelin/store.sqlite3");
    let connection = rusqlite::Connection::open(database).unwrap();
    connection
        .execute("UPDATE conflicts SET private_entry = '{'", [])
        .unwrap();
    drop(connection);

    let listed = Command::new(binary())
        .args([
            "--project",
            world.to_str().unwrap(),
            "conflict",
            "list",
            "right-corrupt",
        ])
        .output()
        .unwrap();
    assert_eq!(listed.status.code(), Some(7));
}

#[test]
fn failed_repair_marks_view_as_repair_required() {
    let (_temp, world) = init();
    let layer = output_text(in_world(
        &world,
        &["layer", "create", "broken-view", "--from", "world"],
    ));
    fs::write(Path::new(&layer).join("payload.txt"), b"payload\n").unwrap();
    in_world(Path::new(&layer), &["checkpoint"]);
    let database = world.join(".javelin/store.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    let object_id: String = connection
        .query_row(
            "SELECT id FROM object_metadata WHERE kind = 'blob' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or_default();
    drop(connection);
    if !object_id.is_empty() {
        let object_path = world
            .join(".javelin/objects")
            .join(&object_id[..2])
            .join(&object_id[2..]);
        fs::remove_file(object_path).unwrap();
    }
    let repaired = Command::new(binary())
        .args([
            "--project",
            world.to_str().unwrap(),
            "repair",
            "--view",
            "broken-view",
        ])
        .env("JAVELIN_MONITOR_CHILD", "1")
        .output()
        .unwrap();
    assert_eq!(repaired.status.code(), Some(7));
    let connection = rusqlite::Connection::open(database).unwrap();
    let state: (i64, String) = connection
        .query_row(
            "SELECT stale, backend FROM views JOIN layers ON layers.id = views.layer_id
             WHERE layers.name = 'broken-view'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, (1, "repair_required".to_string()));
}

#[test]
fn migration_from_schema_v1_adds_validation_environment() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("world");
    fs::create_dir_all(&root).unwrap();
    let store = javelin::store::Store::create(&root).unwrap();
    drop(store);
    let database = root.join(".javelin/store.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             DELETE FROM schema_migrations WHERE version = 2;
             ALTER TABLE validation_runs DROP COLUMN environment_json;",
        )
        .unwrap();
    drop(connection);
    let store = javelin::store::Store::open(&root).unwrap();
    let has_environment: bool = store
        .conn
        .prepare("PRAGMA table_info(validation_runs)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .iter()
        .any(|column| column == "environment_json");
    assert!(has_environment);
}
