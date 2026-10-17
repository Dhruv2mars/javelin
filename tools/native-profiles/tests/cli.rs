use serde_json::Value;
use std::{fs, path::Path, process::Command};

fn call(args: &[&str], success: bool) -> Vec<Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_javelin-native-profiles"))
        .env("NP_TEST_DEBUG", "1")
        .args(args)
        .output()
        .unwrap();
    assert_eq!(
        output.status.success(),
        success,
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn prepare(parent: &Path, name: &str) -> String {
    let path = parent.join(name);
    let s = path.to_str().unwrap().to_string();
    let rows = call(&["prepare", &s, "depth4"], true);
    let fixture = rows.iter().find(|v| v["type"] == "fixture").unwrap();
    assert_eq!(fixture["payload_bytes"], 262144);
    assert_eq!(fixture["history"].as_array().unwrap().len(), 5);
    assert_eq!(fixture["check"]["attempts"], 4);
    assert_eq!(fixture["verified_blob_reads"], 32);
    assert_eq!(fixture["verified_manifest_reads"], 128);
    s
}

#[test]
fn real_cli_checks_accepted_stale_head_and_clone() {
    let temp = tempfile::tempdir().unwrap();
    for action in ["head", "accepted", "stale", "clone"] {
        let root = prepare(temp.path(), action);
        let rows = call(&["measure", &root, action], true);
        let measured = rows.iter().find(|v| v["type"] == "measurement").unwrap();
        assert_eq!(
            measured["check"]["versions"],
            if action == "accepted" { 6 } else { 5 }
        );
        assert_eq!(
            measured["check"]["attempts"],
            if ["accepted", "stale"].contains(&action) {
                5
            } else {
                4
            }
        );
        assert_eq!(
            measured["check"]["layers"],
            if action == "clone" { 2 } else { 1 }
        );
        if ["accepted", "stale"].contains(&action) {
            assert_eq!(measured["outcome"], measured["replay"]);
            assert_eq!(measured["changed_error"], "operation_id_reused");
        }
    }
}

#[test]
fn wrong_declared_fixture_cannot_silently_produce_timings() {
    let temp = tempfile::tempdir().unwrap();
    let root = prepare(temp.path(), "wrong");
    let path = Path::new(&root).join("fixture.json");
    let mut f: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    f["depth"] = 8.into();
    fs::write(path, serde_json::to_vec(&f).unwrap()).unwrap();
    let rows = call(&["measure", &root, "accepted"], false);
    assert!(rows.iter().any(|r| r["type"] == "failure"));
    assert!(!rows.iter().any(|r| r["type"] == "measurement"));
}

#[test]
fn normal_cli_refuses_debug_evidence() {
    if cfg!(debug_assertions) {
        let output = Command::new(env!("CARGO_BIN_EXE_javelin-native-profiles"))
            .arg("prepare")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("release build required")
        );
    }
}
