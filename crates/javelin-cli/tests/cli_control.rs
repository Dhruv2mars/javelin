//! `javelin control`, `javelin login` and `javelin repo create` end to end, as separate
//! processes sharing one state directory.

use std::{
    io::{BufRead, BufReader},
    path::Path,
    process::{Command, Output, Stdio},
};

fn javelin(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_javelin"))
        .args(args)
        .env("JAVELIN_HOME", home)
        .env_remove("JAVELIN_ADMIN_TOKEN")
        .output()
        .expect("run javelin")
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn error(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn login_then_repo_create_is_idempotent_and_refuses_collisions() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");

    let init = javelin(&home, &["control", "init"]);
    assert!(init.status.success(), "{}", error(&init));
    let token = text(&init)
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("jav_admin_"))
        .expect("admin token printed")
        .to_string();
    let again = javelin(&home, &["control", "init"]);
    assert!(!again.status.success());
    assert!(
        error(&again).contains("already initialized"),
        "{}",
        error(&again)
    );

    // `login` prints its code, then polls. Approve it from a second process with the token.
    let mut login = Command::new(env!("CARGO_BIN_EXE_javelin"))
        .arg("login")
        .env("JAVELIN_HOME", &home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn login");
    let mut stderr = BufReader::new(login.stderr.take().expect("stderr"));
    let mut code = None;
    let mut line = String::new();
    while stderr.read_line(&mut line).expect("read") > 0 {
        if let Some((_, rest)) = line.trim().rsplit_once("javelin control approve ") {
            code = Some(rest.to_string());
            break;
        }
        line.clear();
    }
    let code = code.expect("login printed a user code");
    let approve = Command::new(env!("CARGO_BIN_EXE_javelin"))
        .args(["control", "approve", &code])
        .env("JAVELIN_HOME", &home)
        .env("JAVELIN_ADMIN_TOKEN", &token)
        .output()
        .expect("approve");
    assert!(approve.status.success(), "{}", error(&approve));
    let signed_in = login.wait_with_output().expect("login exits");
    assert!(signed_in.status.success(), "{}", error(&signed_in));
    assert!(home.join("credentials").is_file(), "credentials are saved");

    let create = javelin(
        &home,
        &[
            "repo",
            "create",
            "acme/app",
            "--operation",
            "op-1",
            "--json",
        ],
    );
    assert!(create.status.success(), "{}", error(&create));
    let first: serde_json::Value = serde_json::from_str(text(&create).trim()).expect("json");
    assert_eq!(first["created"], true);
    assert_eq!(first["epoch"], 1);
    let repository = first["repository"]
        .as_str()
        .expect("repository")
        .to_string();

    let retry = javelin(
        &home,
        &[
            "repo",
            "create",
            "acme/app",
            "--operation",
            "op-1",
            "--json",
        ],
    );
    assert!(retry.status.success(), "{}", error(&retry));
    let second: serde_json::Value = serde_json::from_str(text(&retry).trim()).expect("json");
    assert_eq!(second["created"], false);
    assert_eq!(second["repository"], repository.as_str());

    let collision = javelin(
        &home,
        &["repo", "create", "acme/app", "--operation", "op-2"],
    );
    assert!(!collision.status.success());
    assert!(
        error(&collision).contains("already exists"),
        "{}",
        error(&collision)
    );

    let cloud = javelin(
        &home,
        &[
            "repo",
            "create",
            "acme/cloud",
            "--operation",
            "op-3",
            "--provider",
            "cloudflare",
        ],
    );
    assert!(!cloud.status.success());
    assert!(
        error(&cloud).contains("not available yet"),
        "{}",
        error(&cloud)
    );

    assert!(
        home.join("control")
            .join("nodes")
            .join(&repository)
            .is_dir()
    );
}

#[test]
fn repo_create_needs_a_sign_in_or_an_admin_token() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let create = javelin(
        &home,
        &["repo", "create", "acme/app", "--operation", "op-1"],
    );
    assert!(!create.status.success());
    assert!(error(&create).contains("not signed in") || error(&create).contains("not initialized"));
}
