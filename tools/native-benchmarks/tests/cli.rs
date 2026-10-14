//! Runs the real `native-bench` executable and checks both its behavior and that its
//! verifier detects tampered evidence (each check has a test that fails without it).
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::OnceLock,
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const BIN: &str = env!("CARGO_BIN_EXE_native-bench");

fn bench(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("run native-bench")
}

fn run_smoke(out: &Path, seed: &str) -> Output {
    bench(&[
        "run",
        "--profile",
        "smoke",
        "--seed",
        seed,
        "--out",
        out.to_str().unwrap(),
        "--allow-debug-build",
        "--source-commit",
        "test-source",
    ])
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// One shared smoke run; every tamper test copies it.
fn baseline() -> &'static PathBuf {
    static RUN: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    &RUN.get_or_init(|| {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("run");
        let result = run_smoke(&out, "1");
        assert!(result.status.success(), "{}", text(&result));
        (tmp, out)
    })
    .1
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn read_lines(dir: &Path) -> Vec<Value> {
    fs::read_to_string(dir.join("samples.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn write_lines(dir: &Path, lines: &[Value]) {
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(dir.join("samples.jsonl"), text).unwrap();
}

/// Copies the baseline, applies a tamper to its raw files, and returns verify output.
fn tampered(edit: impl FnOnce(&Path)) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("copy");
    copy_dir(baseline(), &dir);
    edit(&dir);
    let result = bench(&["verify", dir.to_str().unwrap()]);
    assert!(
        !result.status.success(),
        "tampered evidence must fail verification:\n{}",
        text(&result)
    );
    text(&result)
}

fn assert_fails(output: &str, check: &str) {
    assert!(
        output.contains(&format!("FAIL {check}:")),
        "expected FAIL {check} in:\n{output}"
    );
}

fn contention_ops(lines: &[Value], op: &str) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l["kind"] == "sample" && l["scenario"] == "contention" && l["op"] == op)
        .map(|(i, _)| i)
        .collect()
}

#[test]
fn smoke_run_passes_every_check_with_real_processes() {
    let verified = bench(&["verify", baseline().to_str().unwrap()]);
    let out = text(&verified);
    assert!(verified.status.success(), "{out}");
    assert!(
        out.contains("PASS contention_used_distinct_processes: 3 workers, 3 distinct pids"),
        "{out}"
    );
    assert!(!out.contains("FAIL"), "{out}");
    let summary = read_json(&baseline().join("summary.json"));
    assert_eq!(summary["barrier"]["workers"], 3);
    let outcomes: Vec<_> = summary["groups"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|g| g["scenario"] == "contention" && g["op"] == "compare_and_advance")
        .collect();
    assert!(outcomes.iter().any(|g| g["outcome"] == "accepted"));
    if cfg!(unix) {
        let memory = summary["memory"].as_array().unwrap();
        assert!(
            memory.len() >= 4 + 3
                && memory
                    .iter()
                    .all(|m| m["peak_rss_bytes"].as_u64().unwrap() > 0)
        );
        assert!(
            summary["disk"]
                .as_array()
                .unwrap()
                .iter()
                .all(|d| d["physical_bytes"].is_u64() && d["logical_bytes"].is_u64())
        );
    }
}

#[test]
fn provenance_names_the_exact_executable_sources_and_host() {
    let run = read_json(&baseline().join("run.json"));
    let exe = fs::read(run["command"][0].as_str().unwrap()).unwrap();
    let expected: String = Sha256::digest(&exe)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(run["provenance"]["executable_sha256"], expected);
    assert_eq!(
        run["provenance"]["source_commit_supplied_by_caller"],
        "test-source"
    );
    assert_eq!(run["provenance"]["debug_build_allowed"], true);
    assert_eq!(run["documented_settings"]["sqlite_busy_timeout_secs"], 5);
    assert!(run["host"]["rustc"].as_str().unwrap().contains("rustc"));
    assert!(run["host"]["logical_cpus"].as_u64().unwrap() >= 1);
    assert!(run["host"]["kernel"].is_string() || cfg!(windows));
}

#[test]
fn same_seed_reproduces_fixture_and_different_seed_changes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let digest = |name: &str, seed: &str| {
        let out = tmp.path().join(name);
        let result = run_smoke(&out, seed);
        assert!(result.status.success(), "{}", text(&result));
        let inv = read_json(&out.join("fixture-inventory.json"));
        (
            inv["ingest"]["api_object_digest_sha256"].clone(),
            inv["ingest"]["content_digest_sha256"].clone(),
        )
    };
    let first = digest("a", "1");
    assert_eq!(first, digest("b", "1"));
    assert_ne!(first, digest("c", "2"));
    let baseline = read_json(&baseline().join("fixture-inventory.json"));
    assert_eq!(baseline["ingest"]["api_object_digest_sha256"], first.0);
}

#[test]
fn hard_caps_and_existing_evidence_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    for bad in [
        ["--procs", "99"],
        ["--files", "100000"],
        ["--clones", "1000"],
        ["--history", "9999"],
        ["--budget-secs", "999999"],
    ] {
        let out = tmp.path().join(bad[0].trim_start_matches('-'));
        let result = bench(&[
            "run",
            "--profile",
            "smoke",
            "--out",
            out.to_str().unwrap(),
            "--allow-debug-build",
            bad[0],
            bad[1],
        ]);
        assert_eq!(result.status.code(), Some(2), "{bad:?}: {}", text(&result));
        assert!(
            text(&result).contains("outside hard cap") || text(&result).contains("exceeds"),
            "{}",
            text(&result)
        );
    }
    let nonempty = tmp.path().join("occupied");
    fs::create_dir_all(&nonempty).unwrap();
    fs::write(nonempty.join("old.json"), "{}").unwrap();
    let result = bench(&[
        "run",
        "--profile",
        "smoke",
        "--out",
        nonempty.to_str().unwrap(),
        "--allow-debug-build",
    ]);
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(fs::read_to_string(nonempty.join("old.json")).unwrap(), "{}");
    let refused = bench(&[
        "run",
        "--profile",
        "smoke",
        "--out",
        tmp.path().join("nodebug").to_str().unwrap(),
    ]);
    assert!(cfg!(not(debug_assertions)) || refused.status.code() == Some(2));
}

#[cfg(unix)]
#[test]
fn tool_never_invokes_git() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let shim = tmp.path().join("bin");
    fs::create_dir_all(&shim).unwrap();
    let marker = tmp.path().join("git-was-invoked");
    let script = shim.join("git");
    fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", shim.display(), std::env::var("PATH").unwrap());
    let out = tmp.path().join("run");
    let result = Command::new(BIN)
        .env("PATH", path)
        .args([
            "run",
            "--profile",
            "smoke",
            "--out",
            out.to_str().unwrap(),
            "--allow-debug-build",
        ])
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", text(&result));
    assert!(!marker.exists(), "native-bench invoked git");
}

#[test]
fn verifier_detects_dropped_or_rewritten_world_history() {
    let dropped = tampered(|d| {
        let mut h = read_json(&d.join("history.json"));
        h.as_array_mut().unwrap().pop();
        fs::write(d.join("history.json"), h.to_string()).unwrap();
    });
    assert_fails(&dropped, "one_ordered_winner_per_version");
    assert_fails(&dropped, "history_matches_acknowledged_accepts");
    let rewritten = tampered(|d| {
        let mut h = read_json(&d.join("history.json"));
        h[1]["manifest"] = json!("0".repeat(64));
        fs::write(d.join("history.json"), h.to_string()).unwrap();
    });
    assert_fails(&rewritten, "history_matches_acknowledged_accepts");
    let unexplained = tampered(|d| {
        let mut h = read_json(&d.join("history.json"));
        h[1]["operation"] = json!("ghost-operation");
        fs::write(d.join("history.json"), h.to_string()).unwrap();
    });
    assert_fails(&unexplained, "history_matches_acknowledged_accepts");
}

#[test]
fn verifier_detects_two_winners_for_one_version() {
    let out = tampered(|d| {
        let mut lines = read_lines(d);
        let accepted: Vec<_> = contention_ops(&lines, "compare_and_advance")
            .into_iter()
            .filter(|&i| lines[i]["outcome"] == "accepted")
            .collect();
        let (a, b) = (accepted[0], accepted[1]);
        lines[b]["result"] = lines[a]["result"].clone();
        write_lines(d, &lines);
    });
    assert_fails(&out, "one_ordered_winner_per_version");
}

#[test]
fn verifier_detects_stale_that_could_not_be_stale() {
    let out = tampered(|d| {
        let mut lines = read_lines(d);
        let stale = contention_ops(&lines, "compare_and_advance")
            .into_iter()
            .find(|&i| lines[i]["outcome"] == "stale");
        let Some(i) = stale else {
            panic!("smoke run produced no stale outcome to tamper")
        };
        lines[i]["result"] = lines[i]["expected"].clone();
        write_lines(d, &lines);
    });
    assert_fails(&out, "stale_outcomes_never_create_versions");
}

#[test]
fn verifier_rejects_unresolved_and_non_uncertain_retries() {
    let unresolved = tampered(|d| {
        let mut lines = read_lines(d);
        let i = contention_ops(&lines, "compare_and_advance")[0];
        lines[i]["outcome"] = json!("error");
        lines[i]["error_code"] = json!("storage_error");
        write_lines(d, &lines);
    });
    assert_fails(&unresolved, "every_operation_has_durable_outcome");
    let retry_after_success = tampered(|d| {
        let mut lines = read_lines(d);
        let i = contention_ops(&lines, "compare_and_advance")[0];
        let mut again = lines[i].clone();
        again["attempt"] = json!(1);
        lines.insert(i + 1, again);
        write_lines(d, &lines);
    });
    assert_fails(
        &retry_after_success,
        "retries_repeat_exact_input_after_uncertain_errors",
    );
    let changed_input = tampered(|d| {
        let mut lines = read_lines(d);
        let i = contention_ops(&lines, "compare_and_advance")[0];
        let mut first = lines[i].clone();
        first["outcome"] = json!("error");
        first["error_code"] = json!("storage_error");
        first["expected"] = json!(999);
        lines[i]["attempt"] = json!(1);
        lines.insert(i, first);
        write_lines(d, &lines);
    });
    assert_fails(
        &changed_input,
        "retries_repeat_exact_input_after_uncertain_errors",
    );
}

#[test]
fn verifier_accepts_a_genuine_busy_retry_of_the_exact_same_input() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("copy");
    copy_dir(baseline(), &dir);
    let mut lines = read_lines(&dir);
    let i = contention_ops(&lines, "compare_and_advance")[0];
    let mut busy = lines[i].clone();
    busy["outcome"] = json!("error");
    busy["error_code"] = json!("storage_error");
    busy["error"] = json!("database is locked");
    busy["result"] = Value::Null;
    lines[i]["attempt"] = json!(1);
    lines.insert(i, busy);
    write_lines(&dir, &lines);
    let out = text(&bench(&["verify", dir.to_str().unwrap()]));
    assert!(
        out.contains("PASS retries_repeat_exact_input_after_uncertain_errors"),
        "{out}"
    );
    assert!(
        out.contains("FAIL summary_matches_raw"),
        "injected raw record must desynchronize the summary:\n{out}"
    );
}

#[test]
fn verifier_detects_failed_retry_reuse_blob_and_integrity_evidence() {
    let retry = tampered(|d| {
        let mut lines = read_lines(d);
        let i = contention_ops(&lines, "exact_retry_check")[0];
        lines[i]["outcome"] = json!("mismatch");
        write_lines(d, &lines);
    });
    assert_fails(&retry, "same_operation_retry_returns_identical_outcome");
    let reuse = tampered(|d| {
        let mut lines = read_lines(d);
        let i = contention_ops(&lines, "operation_id_reuse_check")[0];
        lines[i]["outcome"] = json!("mismatch");
        write_lines(d, &lines);
    });
    assert_fails(&reuse, "operation_id_reuse_rejected");
    let blob = tampered(|d| {
        let mut lines = read_lines(d);
        let i = contention_ops(&lines, "blob_readback")[0];
        lines[i]["outcome"] = json!("mismatch");
        write_lines(d, &lines);
    });
    assert_fails(&blob, "acknowledged_blobs_read_back_exactly");
    let shared = tampered(|d| {
        let mut lines = read_lines(d);
        let puts: Vec<_> = contention_ops(&lines, "put_blob")
            .into_iter()
            .filter(|&i| {
                lines[i]["content"]
                    .as_str()
                    .is_some_and(|c| c.starts_with("shared:"))
            })
            .collect();
        lines[puts[0]]["object"] = json!("1".repeat(64));
        write_lines(d, &lines);
    });
    assert_fails(&shared, "acknowledged_blobs_read_back_exactly");
    let integrity = tampered(|d| {
        let mut v = read_json(&d.join("integrity.json"));
        v["attempts"] = json!(v["attempts"].as_u64().unwrap() + 1);
        fs::write(d.join("integrity.json"), v.to_string()).unwrap();
    });
    assert_fails(&integrity, "integrity_matches_history_and_attempts");
}

#[test]
fn verifier_detects_missing_process_evidence_and_summary_and_provenance_edits() {
    let procs = tampered(|d| {
        let mut lines = read_lines(d);
        let i = contention_ops(&lines, "barrier_release")[0];
        lines.remove(i);
        write_lines(d, &lines);
    });
    assert_fails(&procs, "contention_used_distinct_processes");
    let same_pid = tampered(|d| {
        let mut lines = read_lines(d);
        let ids = contention_ops(&lines, "barrier_release");
        let pid = lines[ids[0]]["pid"].clone();
        lines[ids[1]]["pid"] = pid;
        write_lines(d, &lines);
    });
    assert_fails(&same_pid, "contention_used_distinct_processes");
    let summary = tampered(|d| {
        let mut s = read_json(&d.join("summary.json"));
        s["groups"][0]["p50_ns"] = json!(1);
        fs::write(d.join("summary.json"), s.to_string()).unwrap();
    });
    assert_fails(&summary, "summary_matches_raw");
    let provenance = tampered(|d| {
        let mut r = read_json(&d.join("run.json"));
        r["provenance"]["executable_sha256"] = json!("short");
        fs::write(d.join("run.json"), r.to_string()).unwrap();
    });
    assert_fails(&provenance, "provenance_present");
}

#[test]
fn verifier_detects_single_process_scenario_regressions() {
    let ingest = tampered(|d| {
        let mut lines = read_lines(d);
        let i = lines
            .iter()
            .position(|l| l["op"] == "put_blob_duplicate")
            .unwrap();
        lines[i]["object"] = json!("2".repeat(64));
        write_lines(d, &lines);
    });
    assert_fails(&ingest, "ingest_ids_stable_and_inventoried");
    let history = tampered(|d| {
        let mut lines = read_lines(d);
        let i = lines
            .iter()
            .position(|l| l["scenario"] == "history" && l["op"] == "compare_and_advance")
            .unwrap();
        lines[i]["outcome"] = json!("stale");
        write_lines(d, &lines);
    });
    assert_fails(&history, "history_advances_sequential");
    let layers = tampered(|d| {
        let mut lines = read_lines(d);
        let i = lines
            .iter()
            .position(|l| l["scenario"] == "layers" && l["op"] == "capture_layer")
            .unwrap();
        lines[i]["outcome"] = json!("stale");
        write_lines(d, &lines);
    });
    assert_fails(&layers, "layer_captures_applied");
    let stray = tampered(|d| {
        let mut lines = read_lines(d);
        let i = lines
            .iter()
            .position(|l| l["scenario"] == "layers" && l["op"] == "layer_diff")
            .unwrap();
        lines[i]["outcome"] = json!("error");
        write_lines(d, &lines);
    });
    assert_fails(&stray, "no_errors_outside_contention");
    let exit = tampered(|d| {
        let mut lines = read_lines(d);
        let i = lines
            .iter()
            .position(|l| l["kind"] == "scenario" && l["name"] == "history")
            .unwrap();
        lines[i]["exit_code"] = json!(1);
        write_lines(d, &lines);
    });
    assert_fails(&exit, "scenarios_exited_cleanly");
}

#[test]
fn clone_storage_check_passes_for_references_and_fails_for_copied_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("run");
    let result = bench(&[
        "run",
        "--profile",
        "smoke",
        "--out",
        out.to_str().unwrap(),
        "--allow-debug-build",
        "--big-blob-bytes",
        "262144",
        "--clones",
        "4",
    ]);
    assert!(result.status.success(), "{}", text(&result));
    assert!(
        text(&result).contains(
            "PASS clone_storage_not_payload_proportional: 4 clones grew logical bytes by"
        ),
        "{}",
        text(&result)
    );
    let mut lines = read_lines(&out);
    let last = lines
        .iter()
        .rposition(|l| {
            l["kind"] == "disk"
                && l["label"]
                    .as_str()
                    .is_some_and(|x| x.starts_with("after_clone_"))
        })
        .unwrap();
    let grown = lines[last]["logical_bytes"].as_u64().unwrap() + 4 * 262_144;
    lines[last]["logical_bytes"] = json!(grown);
    write_lines(&out, &lines);
    let verified = text(&bench(&["verify", out.to_str().unwrap()]));
    assert_fails(&verified, "clone_storage_not_payload_proportional");
}
