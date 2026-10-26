//! Product gaps found by this suite.

use super::support::*;

// ---------------------------------------------------------------------------------------------
// Product gaps found by this suite. Each test states the designed outcome and fails today; they
// are ignored until the fix lands (TASKS.md, U14).

/// The `javelin` binary: `JAVELIN_ACCEPTANCE_CLI`, or the workspace's debug build.
fn cli() -> PathBuf {
    if let Some(path) = env::var_os("JAVELIN_ACCEPTANCE_CLI") {
        return path.into();
    }
    let target = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target"));
    let path = target
        .join("debug")
        .join(format!("javelin{}", env::consts::EXE_SUFFIX));
    assert!(
        path.exists(),
        "build the CLI first (cargo build -p javelin-cli) or set JAVELIN_ACCEPTANCE_CLI"
    );
    path
}

fn error_code(output: &std::process::Output) -> String {
    let text = String::from_utf8_lossy(&output.stderr);
    let value: Value =
        serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("stderr is not JSON: {text}"));
    value["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("no error code: {value}"))
        .to_string()
}

#[test]
#[ignore = "product gap (TASKS.md U14): an unreachable control plane is storage_error, not \
            control_plane_unavailable"]
fn gap_repo_create_without_a_control_plane_is_control_plane_unavailable() {
    let scratch = TempDir::new().unwrap();
    // The control plane's state cannot be reached: its path is a plain file.
    let unreachable = scratch.path().join("control");
    fs::write(&unreachable, b"").unwrap();
    let output = std::process::Command::new(cli())
        .args([
            "--json",
            "repo",
            "create",
            "acme/app",
            "--operation",
            "op-1",
        ])
        .env("JAVELIN_HOME", scratch.path().join("home"))
        .env("JAVELIN_CONTROL_DIR", &unreachable)
        .env("JAVELIN_ADMIN_TOKEN", "jav_admin_unused")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(error_code(&output), CONTROL_PLANE_UNAVAILABLE);
}

#[test]
#[ignore = "product gap (TASKS.md U14): a node answering 5xx is node_error, not node_unavailable"]
fn gap_attach_to_a_failing_node_is_node_unavailable() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_request(&mut stream);
        stream
            .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
    });
    let project = TempDir::new().unwrap();
    let output = std::process::Command::new(cli())
        .args(["--json", "attach", &format!("http://{addr}/app")])
        .arg(project.path())
        .env("JAVELIN_HOME", project.path().join(".home"))
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(!output.status.success());
    assert_eq!(error_code(&output), NODE_UNAVAILABLE);
}
