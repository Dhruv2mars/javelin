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
