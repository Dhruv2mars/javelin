//! `javelin control`, `javelin login` and `javelin repo create` end to end, as separate
//! processes sharing one state directory.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Output, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
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
        error(&cloud).contains("--provider cloudflare needs --node-url"),
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

/// One admin call a stand-in for the Cloudflare node received: method, path, `authorization`
/// and body.
type Call = (String, String, String, serde_json::Value);

/// A stand-in for the Worker's admin routes on loopback: it answers every call `200 {}` and
/// records it.
fn stand_in_node() -> (String, Arc<Mutex<Vec<Call>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            // Other processes on the machine may probe the port; only admin calls count.
            if let Some(call) = admin_call(stream) {
                seen.lock().unwrap().push(call);
            }
        }
    });
    (url, calls)
}

/// Reads one HTTP request and answers `200 {}` when it is an admin call.
fn admin_call(stream: TcpStream) -> Option<Call> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut words = line.split_whitespace();
    let (method, path) = (words.next()?.to_string(), words.next()?.to_string());
    let (mut length, mut auth) = (0, String::new());
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':')?;
        match name.to_ascii_lowercase().as_str() {
            "content-length" => length = value.trim().parse().ok()?,
            "authorization" => auth = value.trim().to_string(),
            _ => {}
        }
    }
    if !path.starts_with("/admin/r/") {
        return None;
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    let body = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    reader
        .get_mut()
        .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
        .ok()?;
    Some((method, path, auth, body))
}

struct Served(Child);

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `javelin control serve --provider cloudflare --node-url URL` reaches that node with
/// `JAVELIN_NODE_ADMIN_TOKEN`, so a signed-in client at `JAVELIN_CONTROL_URL` creates a
/// repository there and changes its settings, and is told where to attach.
#[test]
fn a_served_control_plane_creates_and_manages_repositories_on_a_cloudflare_node() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let control = temp.path().join("control");
    let (node, calls) = stand_in_node();
    let init = Command::new(env!("CARGO_BIN_EXE_javelin"))
        .args(["--json", "control", "init"])
        .env("JAVELIN_CONTROL_DIR", &control)
        .env_remove("JAVELIN_CONTROL_URL")
        .output()
        .expect("init");
    assert!(init.status.success(), "{}", error(&init));
    let init: serde_json::Value = serde_json::from_slice(&init.stdout).expect("json");
    let admin = init["admin_token"]
        .as_str()
        .expect("admin token")
        .to_string();

    let port = TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();
    let _served = Served(
        Command::new(env!("CARGO_BIN_EXE_javelin"))
            .args([
                "control",
                "serve",
                &format!("127.0.0.1:{port}"),
                "--provider",
                "cloudflare",
                "--node-url",
                &node,
            ])
            .env("JAVELIN_CONTROL_DIR", &control)
            .env("JAVELIN_NODE_ADMIN_TOKEN", "node-secret")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("serve"),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "control serve never listened");
        thread::sleep(Duration::from_millis(50));
    }
    let client = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_javelin"));
        command
            .args(args)
            .env("JAVELIN_HOME", &home)
            .env("JAVELIN_CONTROL_URL", format!("http://127.0.0.1:{port}"))
            .env_remove("JAVELIN_CONTROL_DIR")
            .env_remove("JAVELIN_NODE_ADMIN_TOKEN")
            .env_remove("JAVELIN_ADMIN_TOKEN");
        command
    };

    let mut login = client(&["login"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("login");
    let mut stderr = BufReader::new(login.stderr.take().expect("stderr"));
    let mut line = String::new();
    let code = loop {
        line.clear();
        assert!(
            stderr.read_line(&mut line).expect("read") > 0,
            "login printed no code"
        );
        if let Some((_, code)) = line.trim().rsplit_once("javelin control approve ") {
            break code.to_string();
        }
    };
    let approve = client(&["control", "approve", &code])
        .env("JAVELIN_ADMIN_TOKEN", &admin)
        .output()
        .expect("approve");
    assert!(approve.status.success(), "{}", error(&approve));
    assert!(login.wait().expect("login").success());

    let create = [
        "repo",
        "create",
        "acme/app",
        "--operation",
        "op-1",
        "--publish",
        "requests",
    ];
    let text_create = client(&create).output().expect("create");
    assert!(text_create.status.success(), "{}", error(&text_create));
    let created = client(&[&["--json"][..], &create].concat())
        .output()
        .expect("retry");
    let created: serde_json::Value = serde_json::from_slice(&created.stdout).expect("json");
    let repository = created["repository"].as_str().expect("repository");
    assert_eq!(created["node_url"], node.as_str(), "{created}");
    assert!(
        text(&text_create).contains(&format!("javelin attach {node}/{repository} DIR")),
        "{}",
        text(&text_create)
    );
    assert!(
        !text(&text_create).contains("javelin-node serve"),
        "{}",
        text(&text_create)
    );

    let settings = client(&["repo", "settings", "acme/app", "--required-approvals", "1"])
        .output()
        .expect("settings");
    assert!(settings.status.success(), "{}", error(&settings));

    let calls = calls.lock().unwrap();
    let routes: Vec<_> = calls
        .iter()
        .map(|(method, path, _, _)| format!("{method} {path}"))
        .collect();
    let admin_route = |action: &str| format!("PUT /admin/r/{repository}/{action}");
    assert_eq!(
        routes,
        [
            admin_route("provision"),
            admin_route("keys"),
            admin_route("permissions"),
            admin_route("permissions")
        ],
    );
    assert!(
        calls
            .iter()
            .all(|(_, _, auth, _)| auth == "Bearer node-secret")
    );
    let pushed = &calls[3].3;
    assert_eq!(
        (&pushed["epoch"], &pushed["required_approvals"]),
        (&2.into(), &1.into()),
        "{pushed}"
    );
}
