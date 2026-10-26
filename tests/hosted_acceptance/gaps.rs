//! Product gaps this suite found, now closed. Each test states the designed outcome
//! (TASKS.md, U14).

use super::support::*;

#[test]
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
        .env_remove("JAVELIN_CONTROL_URL")
        .env("JAVELIN_ADMIN_TOKEN", "jav_admin_unused")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(error_code(&output), CONTROL_PLANE_UNAVAILABLE);
    report(
        "gaps",
        "gap.repo-create-control-plane-unavailable",
        "VERIFIED",
        "repo create with the control plane's state unreachable is control_plane_unavailable",
    );
}

#[test]
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
    report(
        "gaps",
        "gap.attach-node-5xx",
        "VERIFIED",
        "attach to a node answering HTTP 503 is node_unavailable",
    );
}
