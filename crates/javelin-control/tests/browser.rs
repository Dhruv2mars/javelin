//! The approval page's HTTP calls over loopback, driven by a software authenticator. The
//! client runs on a thread; this thread answers each connection with the control plane, which
//! is not `Send`.

use std::{
    io::{ErrorKind, Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::Duration,
};

use javelin_control::{ControlPlane, Poll, RelyingParty, answer};
use serde_json::{Value, json};
use webauthn_authenticator_rs::{WebauthnAuthenticator, softpasskey::SoftPasskey};
use webauthn_rs::prelude::{
    CreationChallengeResponse, RegisterPublicKeyCredential, RequestChallengeResponse, Url,
};

const ORIGIN: &str = "http://localhost:8080";

struct Response {
    status: u16,
    /// The head as sent, for values such as the cookie token that must keep their case.
    head: String,
    /// The head in lowercase, for matching header names and flags.
    headers: String,
    body: String,
}

/// One HTTP/1.1 request with `Connection: close`, read to the end.
fn call(
    addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&Value>,
) -> Response {
    let body = body.map(Value::to_string).unwrap_or_default();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(&body);
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream.write_all(request.as_bytes()).expect("write");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read");
    let (head, body) = raw.split_once("\r\n\r\n").expect("a complete response");
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("status code");
    Response {
        status,
        head: head.to_string(),
        headers: head.to_ascii_lowercase(),
        body: body.to_string(),
    }
}

fn json_of(response: &Response) -> Value {
    serde_json::from_str(&response.body)
        .unwrap_or_else(|error| panic!("body is not JSON ({error}): {}", response.body))
}

/// The `javelin_web` cookie from a `Set-Cookie` header, as `name=value`.
fn cookie_of(response: &Response) -> String {
    let line = response
        .head
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("set-cookie:"))
        .expect("a sign-in sets the cookie");
    let value = line["set-cookie:".len()..].trim();
    value.split(';').next().expect("cookie pair").to_string()
}

/// Runs the page's whole flow as a client: enroll with the admin token, sign in with a
/// passkey, approve the device code, then check that the page refuses what it should.
fn client_flow(addr: String, admin: String, user_code: String) -> Vec<String> {
    let mut notes = Vec::new();
    let mut key = SoftPasskey::new(true);
    let origin = Url::parse(ORIGIN).expect("origin");

    let page = call(&addr, "GET", "/", &[], None);
    assert_eq!(page.status, 200);
    assert!(page.headers.contains("content-type: text/html"));
    assert!(page.body.contains("Approve a Javelin sign-in"));
    notes.push("GET / serves the page".into());

    // Enrollment: the admin token in the header.
    let auth = format!("Bearer {admin}");
    let start = call(
        &addr,
        "POST",
        "/passkey/enroll/start",
        &[("Authorization", &auth), ("Origin", ORIGIN)],
        None,
    );
    assert_eq!(start.status, 200, "{}", start.body);
    let body = json_of(&start);
    let options: CreationChallengeResponse =
        serde_json::from_value(body["options"].clone()).expect("creation options");
    let credential: RegisterPublicKeyCredential = key
        .do_registration(origin.clone(), options)
        .expect("authenticator registers");
    let finish = call(
        &addr,
        "POST",
        "/passkey/enroll/finish",
        &[("Origin", ORIGIN)],
        Some(&json!({
            "ceremony": body["ceremony"],
            "credential": serde_json::to_value(&credential).expect("credential JSON"),
        })),
    );
    assert_eq!(finish.status, 200, "{}", finish.body);
    notes.push("enrolls a passkey with the admin token".into());

    // Sign-in: a cookie that approves codes.
    let start = call(
        &addr,
        "POST",
        "/passkey/signin/start",
        &[("Origin", ORIGIN)],
        None,
    );
    assert_eq!(start.status, 200, "{}", start.body);
    let body = json_of(&start);
    let options: RequestChallengeResponse =
        serde_json::from_value(body["options"].clone()).expect("request options");
    let assertion = key
        .do_authentication(origin, options)
        .expect("authenticator signs");
    let finish = call(
        &addr,
        "POST",
        "/passkey/signin/finish",
        &[("Origin", ORIGIN)],
        Some(&json!({
            "ceremony": body["ceremony"],
            "credential": serde_json::to_value(&assertion).expect("assertion JSON"),
        })),
    );
    assert_eq!(finish.status, 200, "{}", finish.body);
    let cookie = cookie_of(&finish);
    assert!(cookie.starts_with("javelin_web=jav_web_"));
    assert!(finish.headers.contains("httponly"));
    assert!(finish.headers.contains("samesite=strict"));
    notes.push("signs in with the passkey and sets an HttpOnly cookie".into());

    // Approval with the cookie.
    let approve = call(
        &addr,
        "POST",
        "/device/approve",
        &[("Cookie", &cookie), ("Origin", ORIGIN)],
        Some(&json!({"user_code": user_code})),
    );
    assert_eq!(approve.status, 200, "{}", approve.body);
    notes.push("approves the device code with the cookie".into());

    // Refusals: no credential, another origin, a malformed body, an unknown path.
    let anonymous = call(
        &addr,
        "POST",
        "/device/approve",
        &[("Origin", ORIGIN)],
        Some(&json!({"user_code": user_code})),
    );
    assert_eq!(anonymous.status, 401);
    let foreign = call(
        &addr,
        "POST",
        "/device/approve",
        &[("Cookie", &cookie), ("Origin", "http://evil.example")],
        Some(&json!({"user_code": user_code})),
    );
    assert_eq!(foreign.status, 403);
    let malformed = call(
        &addr,
        "POST",
        "/passkey/signin/finish",
        &[("Origin", ORIGIN)],
        Some(&json!({"ceremony": 7})),
    );
    assert_eq!(malformed.status, 400);
    let missing = call(&addr, "GET", "/nope", &[], None);
    assert_eq!(missing.status, 404);
    notes.push("refuses no credential (401), another origin (403), malformed JSON (400)".into());
    notes
}

#[test]
fn approval_page_flow_over_loopback() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plane = ControlPlane::open(dir.path()).expect("open");
    let admin = plane.bootstrap().expect("bootstrap");
    let rp = RelyingParty::new(ORIGIN).expect("relying party");
    let start = plane.device_start(1_800_000_000).expect("device start");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("non-blocking");
    let addr = listener.local_addr().expect("address").to_string();
    let user_code = start.user_code.clone();
    let client = thread::spawn(move || client_flow(addr, admin, user_code));

    // Answer connections until the client is done. A client panic shows up at `join`.
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).expect("blocking");
                answer(&plane, &rp, stream).expect("answer");
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if client.is_finished() {
                    break;
                }
                thread::sleep(Duration::from_millis(2));
            }
            Err(error) => panic!("accept: {error}"),
        }
    }
    let notes = client.join().expect("client flow passed");
    assert!(!notes.is_empty());

    match plane
        .device_poll(&start.device_code, 1_800_000_000)
        .expect("poll")
    {
        Poll::Approved(session) => assert_eq!(session.principal, "admin"),
        Poll::Pending => panic!("the page's approval left the code pending"),
    }
}
