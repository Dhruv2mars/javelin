//! U14, the hosted acceptance suite: contract gate 4 (`docs/build-contract.md`, "Verification
//! gates") and the outage matrix (`docs/online-design.md` section 5), against the self-hosted
//! target. Each test starts a real control plane in a temporary directory, signs in through the
//! approval page's HTTP endpoints with a software passkey, creates repositories, and drives the
//! self-hosted node the control plane provisioned: its gate, its review partition, and its
//! loopback HTTP server, behind a fault-injecting proxy where a test needs one.
//!
//! The control plane has no membership API yet, so reviewers other than the creator get roles by
//! a permissions push straight to the node, and their tokens come from the public test key the
//! node is told to trust. The creator's tokens are the control plane's own.
//!
//! `tools/acceptance/run.ts` runs this file with a failing `git` first on PATH. When
//! `JAVELIN_ACCEPTANCE_REPORT` names a file, each assertion appends one JSON line to it:
//! `VERIFIED` once its checks pass, or `NOT VERIFIED` with the reason it cannot be proved here.
//! A test that fails writes nothing for its assertions, which the script reports as FAILED.

use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, ErrorKind, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use javelin::{
    CheckpointId, FileEntry, LayerId, ObjectId, OperationId, PublishAttribution, RepoPath,
    Repository, SparseChanges, VersionNumber,
    outbox::{Entry, Fault, Kind as Record, Outbox, Transport},
};
use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_control::{
    ControlError, ControlPlane, CreateRequest, Created, ForkRequest, LocalNode, NodeLink,
    NodeTarget, Poll, Publish, RelyingParty, Session, answer,
};
use javelin_identity::{KeySet, Need, Scope, test_keys};
use javelin_node::{FileWorldLog, TraceVisibility, auth::Gate, bind_loopback, fork, review, serve};
use javelin_node_worker::CloudflareNode;
use javelin_protocol::{
    Message, ProtocolError,
    codec::{
        Kind, LayerOutcome, PublishOutcome, RequestId, RequestStatus, ReviewOutcome, decode, encode,
    },
    receiver::WorldLog,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use webauthn_authenticator_rs::{WebauthnAuthenticator, softpasskey::SoftPasskey};
use webauthn_rs::prelude::{
    CreationChallengeResponse, RegisterPublicKeyCredential, RequestChallengeResponse, Url,
};

const ORIGIN: &str = "http://localhost:8080";
const NODE_UNAVAILABLE: &str = "node_unavailable";
const CONTROL_PLANE_UNAVAILABLE: &str = "control_plane_unavailable";
const REVIEWER: &[Scope] = &[
    Scope::Read,
    Scope::LayerWrite,
    Scope::ReviewWrite,
    Scope::ReviewApprove,
];

// ---------------------------------------------------------------------------------------------
// The report.

fn report(target: &str, id: &str, status: &str, detail: &str) {
    let Some(path) = env::var_os("JAVELIN_ACCEPTANCE_REPORT") else {
        return;
    };
    let line = json!({"target": target, "id": id, "status": status, "detail": detail});
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open the acceptance report");
    file.write_all(format!("{line}\n").as_bytes())
        .expect("write the acceptance report");
}

fn verified(id: &str, detail: &str) {
    report("self-hosted", id, "VERIFIED", detail);
}

fn not_verified(id: &str, reason: &str) {
    report("self-hosted", id, "NOT VERIFIED", reason);
}

// ---------------------------------------------------------------------------------------------
// Small values.

/// The real clock: the approval page reads it, so every token and code here uses it too.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after 1970")
        .as_secs()
}

fn op(value: &str) -> OperationId {
    OperationId::parse(value).unwrap()
}

fn layer(name: &str) -> LayerId {
    LayerId::parse(name).unwrap()
}

fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}

fn request(id: &str) -> RequestId {
    RequestId::parse(id).unwrap()
}

fn create_request(name: &str, public: bool, publish: Publish) -> CreateRequest {
    CreateRequest {
        owner: "acme".into(),
        name: name.into(),
        public,
        publish,
        operation: format!("create-{name}"),
    }
}

fn repository_id(created: &Created) -> RepositoryId {
    RepositoryId::parse(&created.repository).unwrap()
}

/// A token for a member the control plane cannot name yet, signed by the public test key.
fn member(repository: &str, sub: &str, scopes: &[Scope]) -> String {
    let now = now();
    let claims = test_keys::claims(
        sub,
        repository,
        scopes,
        now,
        now + 600,
        &format!("{sub}-{repository}"),
    );
    test_keys::issuer().issue(&claims).unwrap()
}

// ---------------------------------------------------------------------------------------------
// The browser: the approval page's HTTP endpoints, driven by a software passkey.

struct Response {
    status: u16,
    head: String,
    body: String,
}

/// One HTTP/1.1 request with `Connection: close`, read to the end.
fn http(
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
    let mut stream = TcpStream::connect(addr).expect("connect to the approval page");
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
        body: body.to_string(),
    }
}

fn json_of(response: &Response) -> Value {
    serde_json::from_str(&response.body)
        .unwrap_or_else(|error| panic!("body is not JSON ({error}): {}", response.body))
}

/// What a browser does to approve a device code: enroll a passkey with the admin token, sign in
/// with it, and approve the code with the sign-in cookie.
fn browser(addr: &str, admin: &str, user_code: &str) {
    let mut key = SoftPasskey::new(true);
    let origin = Url::parse(ORIGIN).unwrap();
    let page = http(addr, "GET", &format!("/?code={user_code}"), &[], None);
    assert_eq!(page.status, 200, "the approval page loads");

    let bearer = format!("Bearer {admin}");
    let start = http(
        addr,
        "POST",
        "/passkey/enroll/start",
        &[("Authorization", &bearer), ("Origin", ORIGIN)],
        None,
    );
    assert_eq!(start.status, 200, "{}", start.body);
    let start = json_of(&start);
    let options: CreationChallengeResponse =
        serde_json::from_value(start["options"].clone()).unwrap();
    let credential: RegisterPublicKeyCredential =
        key.do_registration(origin.clone(), options).unwrap();
    let finish = http(
        addr,
        "POST",
        "/passkey/enroll/finish",
        &[("Origin", ORIGIN)],
        Some(&json!({"ceremony": start["ceremony"], "credential": credential})),
    );
    assert_eq!(finish.status, 200, "{}", finish.body);

    let start = http(
        addr,
        "POST",
        "/passkey/signin/start",
        &[("Origin", ORIGIN)],
        None,
    );
    assert_eq!(start.status, 200, "{}", start.body);
    let start = json_of(&start);
    let options: RequestChallengeResponse =
        serde_json::from_value(start["options"].clone()).unwrap();
    let assertion = key.do_authentication(origin, options).unwrap();
    let finish = http(
        addr,
        "POST",
        "/passkey/signin/finish",
        &[("Origin", ORIGIN)],
        Some(&json!({"ceremony": start["ceremony"], "credential": assertion})),
    );
    assert_eq!(finish.status, 200, "{}", finish.body);
    let cookie = finish
        .head
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("set-cookie:"))
        .and_then(|line| line["set-cookie:".len()..].trim().split(';').next())
        .expect("the sign-in sets a cookie")
        .to_string();

    // Another origin is refused even with the cookie; the page's own origin approves.
    let foreign = http(
        addr,
        "POST",
        "/device/approve",
        &[("Cookie", &cookie), ("Origin", "http://evil.example")],
        Some(&json!({"user_code": user_code})),
    );
    assert_eq!(foreign.status, 403);
    let approve = http(
        addr,
        "POST",
        "/device/approve",
        &[("Cookie", &cookie), ("Origin", ORIGIN)],
        Some(&json!({"user_code": user_code})),
    );
    assert_eq!(approve.status, 200, "{}", approve.body);
}

/// Serves the approval page on loopback until the browser on another thread is done. The
/// control plane is not `Send`, so this thread answers every connection.
fn approve_in_browser(plane: &ControlPlane, admin: &str, user_code: &str) {
    let rp = RelyingParty::new(ORIGIN).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (admin, user_code) = (admin.to_string(), user_code.to_string());
    let client = thread::spawn(move || browser(&addr, &admin, &user_code));
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                answer(plane, &rp, stream).expect("answer");
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
    client.join().expect("the browser approval flow passed");
}

// ---------------------------------------------------------------------------------------------
// The hosted deployment: control plane, its self-hosted nodes, and a signed-in device.

struct Hosted {
    dir: TempDir,
    /// `None` while the control plane is down.
    plane: Option<ControlPlane>,
    session: Session,
    /// What the nodes trust: the control plane's key, and the public test key for members.
    keys: KeySet,
}

impl Hosted {
    /// Bootstraps the control plane and signs this device in: `device_start`, approval in the
    /// browser over HTTP, then `device_poll`.
    fn start() -> Self {
        let dir = TempDir::new().unwrap();
        let plane = ControlPlane::open(dir.path()).unwrap();
        Self::sign_in(dir, plane)
    }

    fn sign_in(dir: TempDir, plane: ControlPlane) -> Self {
        let admin = plane.bootstrap().unwrap();
        let start = plane.device_start(now()).unwrap();
        assert_eq!(
            plane.device_poll(&start.device_code, now()).unwrap(),
            Poll::Pending
        );
        approve_in_browser(&plane, &admin, &start.user_code);
        let session = match plane.device_poll(&start.device_code, now()).unwrap() {
            Poll::Approved(session) => session,
            Poll::Pending => panic!("the browser's approval left the code pending"),
        };
        let mut keys = plane.node_keys().unwrap();
        keys.trust(test_keys::KID, test_keys::signing_key(1).verifying_key())
            .unwrap();
        Self {
            dir,
            plane: Some(plane),
            session,
            keys,
        }
    }

    fn plane(&self) -> &ControlPlane {
        self.plane.as_ref().expect("the control plane is up")
    }

    fn nodes(&self) -> LocalNode {
        LocalNode::new(self.dir.path().join("nodes"))
    }

    fn create(&self, name: &str, public: bool, publish: Publish) -> Created {
        self.plane()
            .create_repository(
                &self.session.access_token,
                &create_request(name, public, publish),
                now(),
            )
            .unwrap()
    }

    /// The control plane's access token for `repository`, as its creator.
    fn token(&self, repository: &str) -> String {
        self.plane()
            .repository_token(&self.session.access_token, repository, now())
            .unwrap()
            .access_token
    }

    fn node_dir(&self, repository: &str) -> PathBuf {
        self.nodes()
            .repository_dir(&RepositoryId::parse(repository).unwrap())
    }

    /// The node's gate, with the permissions the node holds.
    fn gate(&self, repository: &str) -> Gate {
        self.nodes()
            .gate(&RepositoryId::parse(repository).unwrap(), self.keys.clone())
            .unwrap()
    }

    fn log(&self, repository: &str) -> FileWorldLog {
        FileWorldLog::open(self.node_dir(repository)).unwrap()
    }

    /// Pushes permissions at `epoch`: the creator (`admin`) keeps Maintain, plus `members`.
    fn push(&self, created: &Created, epoch: u64, members: &[(&str, Role)], required: u32) {
        self.nodes()
            .apply(&permissions(created, epoch, members, required))
            .unwrap();
    }
}

fn permissions(
    created: &Created,
    epoch: u64,
    members: &[(&str, Role)],
    required: u32,
) -> AppliedPermissions {
    let mut roles = BTreeMap::new();
    roles.insert(PrincipalId::parse("admin").unwrap(), Role::Maintain);
    for (name, role) in members {
        roles.insert(PrincipalId::parse(name).unwrap(), *role);
    }
    AppliedPermissions {
        repository: repository_id(created),
        epoch,
        visibility: if created.public {
            Visibility::Public
        } else {
            Visibility::Private
        },
        roles,
        required_approvals: required,
    }
}

// ---------------------------------------------------------------------------------------------
// The node: Layer writes, review messages, and the loopback wire.

/// Writes one file to Layer `name` on the node and returns the Layer's new checkpoint.
fn write(
    log: &mut FileWorldLog,
    name: &str,
    operation: &str,
    path: &str,
    bytes: &[u8],
) -> ObjectId {
    let content = log.repository().put_blob(bytes).unwrap();
    log.append(&op(operation), &layer(name), &file(path, content))
        .unwrap()
        .checkpoint()
        .object()
}

fn file(path: &str, content: ObjectId) -> SparseChanges {
    SparseChanges::new([(
        RepoPath::parse(path).unwrap(),
        Some(FileEntry::Regular {
            content,
            executable: false,
        }),
    )])
    .unwrap()
}

/// The tree a checkpoint composes to.
fn tree_of(log: &mut FileWorldLog, checkpoint: ObjectId) -> ObjectId {
    let repo = log.repository();
    let manifest = repo
        .checkpoint_manifest(CheckpointId::parse(&checkpoint.to_string()).unwrap())
        .unwrap();
    repo.put_snapshot(&manifest).unwrap()
}

fn send(log: &mut FileWorldLog, gate: &Gate, token: &str, message: Message) -> Message {
    review::handle(log, gate, Some(token), now(), &message).unwrap()
}

fn open_request(operation: &str, id: &str, name: &str, checkpoint: ObjectId, base: u64) -> Message {
    Message::OpenRequest {
        operation: op(operation),
        request: request(id),
        layer: layer(name),
        checkpoint,
        base: v(base),
    }
}

fn revise(operation: &str, id: &str, expected: u32, checkpoint: ObjectId) -> Message {
    Message::Revise {
        operation: op(operation),
        request: request(id),
        expected_revision: expected,
        checkpoint,
        base: v(0),
    }
}

fn approve(operation: &str, id: &str, revision: u32) -> Message {
    Message::Approve {
        operation: op(operation),
        request: request(id),
        revision,
    }
}

fn publish_request(operation: &str, id: &str, revision: u32, base: u64) -> Message {
    Message::PublishRequest {
        operation: op(operation),
        request: request(id),
        revision,
        base: v(base),
    }
}

fn accepted(reply: Message) -> u32 {
    match reply {
        Message::ReviewOutcome(ReviewOutcome::Accepted { revision }) => revision,
        other => panic!("expected accepted, got {other:?}"),
    }
}

fn refused(reply: Message) -> String {
    match reply {
        Message::ReviewOutcome(ReviewOutcome::Refused { code })
        | Message::PublishOutcome(PublishOutcome::Refused { code }) => code,
        other => panic!("expected refused, got {other:?}"),
    }
}

fn published(reply: &Message) -> (VersionNumber, ObjectId) {
    match reply {
        Message::PublishOutcome(PublishOutcome::Published { number, tree, .. }) => (*number, *tree),
        other => panic!("expected published, got {other:?}"),
    }
}

/// Every stored row of a request, in order: the review partition's hash chain.
/// `(seq, operation, command, reply, chain)` of one stored review row.
type ReviewRow = (i64, String, Vec<u8>, Vec<u8>, String);

fn review_rows(node_dir: &Path, id: &str) -> Vec<ReviewRow> {
    let db = rusqlite::Connection::open(node_dir.join("node.db")).unwrap();
    let mut rows = db
        .prepare(
            "SELECT seq, operation, command, reply, chain FROM review_rows
             WHERE request = ?1 ORDER BY seq",
        )
        .unwrap();
    rows.query_map([id], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
        ))
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

/// Serves `log` on loopback until the process exits. The server holds the node's lock.
fn serve_forever(log: FileWorldLog, name: &str) -> SocketAddr {
    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let name = name.to_string();
    thread::spawn(move || {
        let mut log = log;
        let _ = serve(&listener, &name, &mut log, None);
    });
    addr
}

/// Serves exactly `connections` connections, then hands the log back.
fn serve_then_return(
    log: FileWorldLog,
    name: &str,
    connections: usize,
) -> (SocketAddr, thread::JoinHandle<FileWorldLog>) {
    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let name = name.to_string();
    let server = thread::spawn(move || {
        let mut log = log;
        serve(&listener, &name, &mut log, Some(connections)).unwrap();
        log
    });
    (addr, server)
}

/// One request body over loopback HTTP, as a client sends it. No reply, a reset, or a status
/// other than 200 is `node_unavailable`: the node did not decide, as far as the client knows.
fn post(addr: SocketAddr, name: &str, body: &[u8]) -> Result<Vec<u8>, &'static str> {
    let exchange = || -> io::Result<Vec<u8>> {
        let mut stream = TcpStream::connect(addr)?;
        stream.set_read_timeout(Some(Duration::from_secs(20)))?;
        write!(
            stream,
            "POST /v2/r/{name} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )?;
        stream.write_all(body)?;
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply)?;
        Ok(reply)
    };
    let reply = exchange().map_err(|_| NODE_UNAVAILABLE)?;
    let split = reply
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or(NODE_UNAVAILABLE)?;
    if !reply.starts_with(b"HTTP/1.1 200 ") {
        return Err(NODE_UNAVAILABLE);
    }
    Ok(reply[split + 4..].to_vec())
}

fn call(addr: SocketAddr, name: &str, message: &Message) -> Result<Message, &'static str> {
    let reply = post(addr, name, &encode(message).unwrap())?;
    Ok(decode(&reply).unwrap())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    /// Forward every request.
    Pass,
    /// Close every connection before forwarding: the node is down.
    Reset,
    /// Forward the next request, then close without relaying the reply: the node decided and
    /// the client never heard.
    DropReplyOnce,
}

#[derive(Default)]
struct ProxyState {
    reset: bool,
    forwarded: usize,
    /// The `forwarded` count whose reply is dropped.
    drop_at: Option<usize>,
}

/// A proxy in front of a node, one request per connection: it forces `Connection: close` on
/// both sides so a keep-alive client cannot carry two requests past one decision.
struct Proxy {
    addr: SocketAddr,
    state: Arc<Mutex<ProxyState>>,
}

impl Proxy {
    fn start(upstream: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(ProxyState::default()));
        let shared = Arc::clone(&state);
        thread::spawn(move || {
            for client in listener.incoming() {
                let Ok(mut client) = client else { continue };
                if shared.lock().unwrap().reset {
                    continue;
                }
                let Ok(request) = read_request(&mut client) else {
                    continue;
                };
                let drop_reply = {
                    let mut state = shared.lock().unwrap();
                    state.forwarded += 1;
                    state.drop_at == Some(state.forwarded)
                };
                let reply = forward(upstream, &close(&request));
                if !drop_reply {
                    let _ = client.write_all(&close(&reply));
                }
            }
        });
        Self { addr, state }
    }

    fn set(&self, mode: Mode) {
        let mut state = self.state.lock().unwrap();
        state.reset = mode == Mode::Reset;
        state.drop_at = (mode == Mode::DropReplyOnce).then_some(state.forwarded + 1);
    }

    /// Forwards `passed` requests, then drops the reply to the one after.
    fn drop_reply_after(&self, passed: usize) {
        let mut state = self.state.lock().unwrap();
        state.reset = false;
        state.drop_at = Some(state.forwarded + passed + 1);
    }

    fn forwarded(&self) -> usize {
        self.state.lock().unwrap().forwarded
    }
}

/// `message` (a request or a reply) with its `Connection` header replaced by `close`.
fn close(message: &[u8]) -> Vec<u8> {
    let Some(end) = message.windows(4).position(|w| w == b"\r\n\r\n") else {
        return message.to_vec();
    };
    let head = String::from_utf8_lossy(&message[..end]);
    let mut lines = head.split("\r\n");
    let mut out = lines.next().unwrap_or_default().to_string();
    for line in lines.filter(|line| !line.to_ascii_lowercase().starts_with("connection:")) {
        out.push_str("\r\n");
        out.push_str(line);
    }
    out.push_str("\r\nConnection: close");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(&message[end..]);
    bytes
}

/// One whole request: headers, then `Content-Length` bytes of body.
fn read_request(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut data = Vec::new();
    let mut chunk = [0; 4096];
    let end = loop {
        if let Some(at) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(ErrorKind::UnexpectedEof.into());
        }
        data.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&data[..end]).to_ascii_lowercase();
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    while data.len() < end + length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(ErrorKind::UnexpectedEof.into());
        }
        data.extend_from_slice(&chunk[..read]);
    }
    Ok(data)
}

fn forward(upstream: SocketAddr, request: &[u8]) -> Vec<u8> {
    let Ok(mut node) = TcpStream::connect(upstream) else {
        return Vec::new();
    };
    let _ = node.set_read_timeout(Some(Duration::from_secs(20)));
    let mut reply = Vec::new();
    if node.write_all(request).is_ok() {
        let _ = node.read_to_end(&mut reply);
    }
    reply
}

/// The client's outbox transport: each record's payload is one request to the node. Before
/// sending, it checks its access token. An expired token must be refreshed at the control
/// plane; when the control plane is down, the record waits with `control_plane_unavailable`.
struct Wire<'a> {
    addr: SocketAddr,
    name: String,
    clock: u64,
    token_expires: u64,
    refresh_token: String,
    plane: Option<&'a ControlPlane>,
}

impl Transport for Wire<'_> {
    fn deliver(&mut self, entry: &Entry) -> Result<Vec<u8>, Fault> {
        if self.clock >= self.token_expires {
            let plane = self.plane.ok_or_else(|| {
                Fault::ControlPlaneUnavailable(
                    "the token has expired and the control plane is unreachable".into(),
                )
            })?;
            let fresh = plane
                .refresh(&self.refresh_token, self.clock)
                .map_err(|error| Fault::ControlPlaneUnavailable(error.to_string()))?;
            self.token_expires = self.clock + fresh.expires_in;
        }
        post(self.addr, &self.name, &entry.payload)
            .map_err(|code| Fault::NodeUnavailable(code.into()))
    }
}

fn publish_payload(operation: &str, name: &str, expected: ObjectId, base: u64) -> Vec<u8> {
    encode(&Message::PublishLayer {
        operation: op(operation),
        layer: layer(name),
        expected,
        base: v(base),
        attribution: PublishAttribution::Human,
    })
    .unwrap()
}

fn head(node_dir: &Path) -> VersionNumber {
    Repository::open(node_dir).unwrap().head().unwrap().number
}

fn code_of<T: std::fmt::Debug>(result: Result<T, ProtocolError>) -> String {
    result.unwrap_err().code
}

// ---------------------------------------------------------------------------------------------
// Public and private repository flows.

#[test]
fn public_and_private_repository_flows_permissions_and_retries() {
    let hosted = Hosted::start();
    verified(
        "flow.browser-device-approval",
        "device_start, passkey enroll and sign-in, then POST /device/approve with the cookie over \
         loopback HTTP; another Origin is refused (403); device_poll returns the session",
    );
    let plane = hosted.plane();
    let bearer = &hosted.session.access_token;

    let site = hosted.create("site", true, Publish::Requests);
    let vault = hosted.create("vault", false, Publish::Direct);
    assert!(site.created && site.public && site.publish == Publish::Requests);
    assert!(vault.created && !vault.public && vault.publish == Publish::Direct);
    assert_eq!((site.epoch, vault.epoch), (1, 1));
    assert_eq!(head(&hosted.node_dir(&site.repository)), v(0));
    assert_eq!(head(&hosted.node_dir(&vault.repository)), v(0));

    // Retries: the same operation is the same repository; anything else is refused.
    let again = plane
        .create_repository(
            bearer,
            &create_request("site", true, Publish::Requests),
            now(),
        )
        .unwrap();
    assert!(!again.created);
    assert_eq!(again.repository, site.repository);
    let mut collision = create_request("site", true, Publish::Requests);
    collision.operation = "create-site-again".into();
    assert_eq!(
        plane
            .create_repository(bearer, &collision, now())
            .unwrap_err()
            .code(),
        "conflict"
    );
    let mut reused = create_request("other", true, Publish::Requests);
    reused.operation = "create-site".into();
    assert_eq!(
        plane
            .create_repository(bearer, &reused, now())
            .unwrap_err()
            .code(),
        "operation_id_reused"
    );
    verified(
        "flow.retries",
        "repeating create with its operation ID returns the same repository (created=false); a \
         name collision is conflict; reusing an operation ID for another request is \
         operation_id_reused",
    );

    // A refreshed sign-in gets repository tokens; a repository token cannot create.
    let fresh = plane.refresh(&hosted.session.refresh_token, now()).unwrap();
    let site_token = plane
        .repository_token(&fresh.access_token, &site.repository, now())
        .unwrap()
        .access_token;
    let vault_token = hosted.token(&vault.repository);
    assert_eq!(
        plane
            .create_repository(
                &site_token,
                &create_request("x", false, Publish::Direct),
                now()
            )
            .unwrap_err()
            .code(),
        "forbidden"
    );

    // What the node holds after provisioning: visibility, and the creator as the only role.
    let site_gate = hosted.gate(&site.repository);
    let vault_gate = hosted.gate(&vault.repository);
    for (gate, visibility) in [
        (&site_gate, Visibility::Public),
        (&vault_gate, Visibility::Private),
    ] {
        let applied = gate.permissions().unwrap();
        assert_eq!(applied.epoch, 1);
        assert_eq!(applied.visibility, visibility);
        assert_eq!(
            applied
                .roles
                .keys()
                .map(PrincipalId::as_str)
                .collect::<Vec<_>>(),
            ["admin"]
        );
    }
    let need = |scope| Need { scope, layer: None };
    let t = now();

    // Anonymous: a public repository is readable, nothing more; a private one is not.
    site_gate.check(None, t, need(Scope::Read)).unwrap();
    assert_eq!(
        code_of(site_gate.check(None, t, need(Scope::LayerWrite))),
        "unauthorized"
    );
    assert_eq!(
        code_of(vault_gate.check(None, t, need(Scope::Read))),
        "unauthorized"
    );
    verified(
        "flow.public-private",
        "public+requests and private+direct repositories created through the control plane and \
         provisioned at World 0; anonymous Read of the public one is allowed and nothing else; \
         anonymous Read of the private one is unauthorized",
    );

    // Tokens: exact repository, role intersected with scope, epochs enforced.
    vault_gate
        .check(Some(&vault_token), t, need(Scope::Publish))
        .unwrap();
    assert_eq!(
        code_of(vault_gate.check(Some(&site_token), t, need(Scope::Read))),
        "forbidden",
        "a token for another repository"
    );
    assert_eq!(
        code_of(vault_gate.check(Some(bearer), t, need(Scope::Read))),
        "forbidden",
        "a sign-in (`*`) token at a node"
    );
    hosted.push(&vault, 2, &[("bob", Role::Read)], 0);
    let vault_gate = hosted.gate(&vault.repository);
    let bob = member(&vault.repository, "bob", &[Scope::Read, Scope::Publish]);
    vault_gate.check(Some(&bob), t, need(Scope::Read)).unwrap();
    assert_eq!(
        code_of(vault_gate.check(Some(&bob), t, need(Scope::Publish))),
        "forbidden",
        "a Read role with a publish-scoped token"
    );
    let stranger = member(&vault.repository, "eve", &[Scope::Read]);
    assert_eq!(
        code_of(vault_gate.check(Some(&stranger), t, need(Scope::Read))),
        "forbidden",
        "no role in the private repository"
    );
    // A stale push is refused; the same push again is a harmless retry.
    let stale = hosted
        .nodes()
        .apply(&permissions(&vault, 1, &[("eve", Role::Maintain)], 0))
        .unwrap_err();
    assert_eq!(stale.code(), "stale_permissions");
    hosted.push(&vault, 2, &[("bob", Role::Read)], 0);
    assert!(
        hosted
            .gate(&vault.repository)
            .check(Some(&stranger), t, need(Scope::Read))
            .is_err()
    );
    verified(
        "flow.permissions",
        "control-plane tokens are bound to one repository and refused elsewhere (forbidden); a \
         `*` sign-in token is refused at a node; role caps scope (Read role cannot publish); a \
         stale permissions push is stale_permissions and changes nothing",
    );

    // The node's wire: the public repository's World 0 over loopback HTTP.
    let (addr, server) = serve_then_return(hosted.log(&site.repository), &site.repository, 1);
    let resolved = call(addr, &site.repository, &Message::Resolve { version: None }).unwrap();
    server.join().unwrap();
    assert!(matches!(resolved, Message::Resolved { number, .. } if number == v(0)));
    not_verified(
        "flow.wire-authentication",
        "the self-hosted node's HTTP server carries no token and binds loopback only \
         (crates/javelin-node/src/server.rs); permissions are proved at the node's gate instead",
    );
    not_verified(
        "flow.membership",
        "the control plane has no membership API (repository_token serves the creator and admin \
         only); members here are pushed to the node directly with test-key tokens",
    );
}

// ---------------------------------------------------------------------------------------------
// Gate 4: exact revision authorization and immutable prior revisions.

#[test]
fn exact_revision_authorization_and_immutable_prior_revisions() {
    let hosted = Hosted::start();
    let site = hosted.create("site", true, Publish::Requests);
    let id = site.repository.as_str();
    let reviewers = [("alice", Role::Review), ("rita", Role::Review)];
    hosted.push(&site, 2, &reviewers, 1);
    let gate = hosted.gate(id);
    let dir = hosted.node_dir(id);
    let mut log = hosted.log(id);
    log.create_layer(&layer("work"), v(0)).unwrap();
    let alice = member(id, "alice", REVIEWER);
    let rita = member(id, "rita", REVIEWER);
    let maintainer = hosted.token(id);

    let one = write(&mut log, "work", "write-1", "README.md", b"one");
    assert_eq!(
        accepted(send(
            &mut log,
            &gate,
            &alice,
            open_request("open", "pr-1", "work", one, 0)
        )),
        1
    );
    assert_eq!(
        accepted(send(
            &mut log,
            &gate,
            &rita,
            approve("approve-1", "pr-1", 1)
        )),
        1
    );
    let first_rows = review_rows(&dir, "pr-1");
    assert_eq!(first_rows.len(), 2);

    let two = write(&mut log, "work", "write-2", "README.md", b"two");
    let approved_tree = tree_of(&mut log, two);
    assert_eq!(
        accepted(send(
            &mut log,
            &gate,
            &alice,
            revise("revise-1", "pr-1", 1, two)
        )),
        2
    );
    // Revision 1 and its approval are unchanged byte for byte.
    let second_rows = review_rows(&dir, "pr-1");
    assert_eq!(second_rows[..2], first_rows[..]);

    // Exact authorization.
    assert_eq!(
        refused(send(
            &mut log,
            &gate,
            &maintainer,
            publish_request("publish-a", "pr-1", 2, 0)
        )),
        "not_enough_approvals",
        "an approval of revision 1 does not count for revision 2"
    );
    assert_eq!(
        refused(send(
            &mut log,
            &gate,
            &maintainer,
            publish_request("publish-b", "pr-1", 1, 0)
        )),
        "stale_revision",
        "a superseded revision cannot be published"
    );
    assert_eq!(
        refused(send(
            &mut log,
            &gate,
            &alice,
            approve("approve-self", "pr-1", 2)
        )),
        "self_approval"
    );
    assert_eq!(
        refused(send(
            &mut log,
            &gate,
            &rita,
            revise("revise-rita", "pr-1", 2, two)
        )),
        "not_author"
    );
    assert_eq!(
        refused(send(
            &mut log,
            &gate,
            &rita,
            approve("approve-old", "pr-1", 1)
        )),
        "stale_revision",
        "an approval names the exact current revision"
    );

    // Immutable prior revisions: revision 2 cannot be submitted again.
    let three = write(&mut log, "work", "write-3", "README.md", b"three");
    assert_eq!(
        refused(send(
            &mut log,
            &gate,
            &alice,
            revise("revise-again", "pr-1", 1, three)
        )),
        "stale_revision"
    );

    // An approver whose role is removed at a newer epoch no longer counts.
    assert_eq!(
        accepted(send(
            &mut log,
            &gate,
            &rita,
            approve("approve-2", "pr-1", 2)
        )),
        2
    );
    // A permissions push takes the node's lock, which an open log holds.
    drop(log);
    hosted.push(&site, 3, &reviewers[..1], 1);
    let without_rita = hosted.gate(id);
    let mut log = hosted.log(id);
    assert_eq!(
        refused(send(
            &mut log,
            &without_rita,
            &maintainer,
            publish_request("publish-c", "pr-1", 2, 0)
        )),
        "not_enough_approvals"
    );
    drop(log);
    hosted.push(&site, 4, &reviewers, 1);
    let gate = hosted.gate(id);
    let mut log = hosted.log(id);

    // The Layer has moved on to "three"; revision 2 still publishes its own tree.
    let reply = send(
        &mut log,
        &gate,
        &maintainer,
        publish_request("publish-d", "pr-1", 2, 0),
    );
    let (number, tree) = published(&reply);
    assert_eq!(number, v(1));
    assert_eq!(tree, approved_tree);
    let final_rows = review_rows(&dir, "pr-1");
    assert_eq!(final_rows[..second_rows.len()], second_rows[..]);
    assert_eq!(
        send(
            &mut log,
            &gate,
            &alice,
            Message::ReadRequest {
                request: request("pr-1")
            }
        ),
        Message::RequestState {
            request: request("pr-1"),
            state: RequestStatus::Published,
            revision: 2,
        }
    );
    verified(
        "gate4.exact-revision-authorization",
        "an approval of revision 1 does not count for revision 2 (not_enough_approvals); a \
         superseded revision is stale_revision; self-approval and non-author revise are refused; \
         an approver removed at a newer epoch blocks Publish; Publish publishes the approved \
         revision's tree although the Layer moved on",
    );
    verified(
        "gate4.immutable-prior-revisions",
        "re-submitting a revision is stale_revision; the stored rows of every earlier revision \
         (command, reply and hash chain) are byte-identical after each later revise, approval \
         and Publish",
    );
}

// ---------------------------------------------------------------------------------------------
// Gate 4: one durable outcome after a lost reply.

#[test]
fn one_durable_outcome_after_a_lost_reply() {
    let hosted = Hosted::start();
    let vault = hosted.create("vault", false, Publish::Direct);
    let id = vault.repository.clone();
    let dir = hosted.node_dir(&id);
    let mut log = hosted.log(&id);
    let mut heads = BTreeMap::new();
    for name in ["a", "b"] {
        log.create_layer(&layer(name), v(0)).unwrap();
        let head = write(
            &mut log,
            name,
            &format!("write-{name}"),
            &format!("{name}.txt"),
            name.as_bytes(),
        );
        heads.insert(name, head);
    }
    let proxy = Proxy::start(serve_forever(log, &id));

    // PublishLayer: the node commits, the reply is lost, the client retries.
    let publish_a = Message::PublishLayer {
        operation: op("publish-a"),
        layer: layer("a"),
        expected: heads["a"],
        base: v(0),
        attribution: PublishAttribution::Human,
    };
    proxy.set(Mode::DropReplyOnce);
    assert_eq!(call(proxy.addr, &id, &publish_a), Err(NODE_UNAVAILABLE));
    assert_eq!(
        head(&dir),
        v(1),
        "the node decided before the reply was lost"
    );
    let retry = call(proxy.addr, &id, &publish_a).unwrap();
    let again = call(proxy.addr, &id, &publish_a).unwrap();
    assert_eq!(published(&retry).0, v(1));
    assert_eq!(retry, again);
    assert_eq!(head(&dir), v(1), "one World version for the operation");
    let other = Message::PublishLayer {
        operation: op("publish-a"),
        layer: layer("b"),
        expected: heads["b"],
        base: v(1),
        attribution: PublishAttribution::Human,
    };
    assert_eq!(
        call(proxy.addr, &id, &other).unwrap(),
        Message::PublishOutcome(PublishOutcome::Refused {
            code: "operation_id_reused".into()
        })
    );

    // The outbox: the reply is lost mid-replay; the record stays queued and replays once.
    let client = TempDir::new().unwrap();
    let mut outbox = Outbox::open(client.path(), "vault").unwrap();
    let seq = outbox
        .enqueue(
            Record::Publish,
            "b",
            &op("publish-b"),
            &publish_payload("publish-b", "b", heads["b"], 0),
        )
        .unwrap();
    let mut wire = Wire {
        addr: proxy.addr,
        name: id.clone(),
        clock: now(),
        token_expires: now() + hosted.session.expires_in,
        refresh_token: hosted.session.refresh_token.clone(),
        plane: hosted.plane.as_ref(),
    };
    proxy.set(Mode::DropReplyOnce);
    let lost = outbox.replay(&mut wire).unwrap();
    assert!(lost.delivered.is_empty());
    assert_eq!(lost.stalled.unwrap().fault.code(), NODE_UNAVAILABLE);
    assert_eq!(head(&dir), v(2));
    // The client restarts; the journal still holds the record.
    drop(outbox);
    let mut outbox = Outbox::open(client.path(), "vault").unwrap();
    assert_eq!(outbox.pending().count(), 1);
    let replayed = outbox.replay(&mut wire).unwrap();
    assert_eq!(replayed.delivered.len(), 1);
    assert_eq!(replayed.delivered[0].seq, seq);
    assert_eq!(
        published(&decode(&replayed.delivered[0].outcome).unwrap()).0,
        v(2)
    );
    assert_eq!(outbox.pending().count(), 0);
    assert!(outbox.replay(&mut wire).unwrap().delivered.is_empty());
    assert_eq!(
        head(&dir),
        v(2),
        "one World version for the queued operation"
    );

    // PublishRequest: the review partition records one outcome for the operation.
    let site = hosted.create("site", true, Publish::Requests);
    hosted.push(
        &site,
        2,
        &[("alice", Role::Review), ("rita", Role::Review)],
        1,
    );
    let site_id = site.repository.as_str();
    let gate = hosted.gate(site_id);
    let mut log = hosted.log(site_id);
    log.create_layer(&layer("work"), v(0)).unwrap();
    let (alice, rita) = (
        member(site_id, "alice", REVIEWER),
        member(site_id, "rita", REVIEWER),
    );
    let maintainer = hosted.token(site_id);
    let one = write(&mut log, "work", "write-1", "a.txt", b"one");
    send(
        &mut log,
        &gate,
        &alice,
        open_request("open", "pr-1", "work", one, 0),
    );
    send(&mut log, &gate, &rita, approve("approve-1", "pr-1", 1));
    let first = send(
        &mut log,
        &gate,
        &maintainer,
        publish_request("publish", "pr-1", 1, 0),
    );
    let retry = send(
        &mut log,
        &gate,
        &maintainer,
        publish_request("publish", "pr-1", 1, 0),
    );
    assert_eq!(first, retry);
    assert_eq!(published(&first).0, v(1));
    assert_eq!(
        refused(send(
            &mut log,
            &gate,
            &maintainer,
            publish_request("publish-2", "pr-1", 1, 0)
        )),
        "not_open"
    );
    assert_eq!(log.head().unwrap().number, v(1));
    verified(
        "gate4.lost-reply-one-outcome",
        "PublishLayer over loopback HTTP with the reply dropped after commit: the retry returns \
         the identical outcome and the World advanced once; the operation ID is bound to its \
         request (operation_id_reused); a queued outbox Publish whose reply was lost stays queued \
         across a client restart and replays to the same single version; a repeated \
         PublishRequest returns the recorded outcome",
    );
    assert!(proxy.forwarded() >= 5);
}

// ---------------------------------------------------------------------------------------------
// Gate 4: inherited fork data without inherited credentials.

#[test]
fn a_fork_inherits_data_but_not_credentials() {
    let hosted = Hosted::start();
    let site = hosted.create("site", true, Publish::Requests);
    let source = site.repository.clone();
    hosted.push(
        &site,
        2,
        &[("alice", Role::Review), ("rita", Role::Maintain)],
        1,
    );
    let (content, published_tree) = {
        let mut log = hosted.log(&source);
        log.create_layer(&layer("main"), v(0)).unwrap();
        let content = log.repository().put_blob(b"inherited content").unwrap();
        let head = log
            .append(&op("write-1"), &layer("main"), &file("README.md", content))
            .unwrap()
            .checkpoint()
            .object();
        let outcome = log
            .publish_layer(
                &op("publish-1"),
                &layer("main"),
                head,
                v(0),
                &PublishAttribution::Human,
            )
            .unwrap();
        let PublishOutcome::Published { tree, .. } = outcome else {
            panic!("publish: {outcome:?}");
        };
        // An open request in the source: review state is the source's alone.
        let gate = hosted.gate(&source);
        log.create_layer(&layer("draft"), v(1)).unwrap();
        let draft = write(&mut log, "draft", "write-2", "b.txt", b"draft");
        let alice = member(&source, "alice", REVIEWER);
        assert_eq!(
            accepted(send(
                &mut log,
                &gate,
                &alice,
                open_request("open", "pr-1", "draft", draft, 1)
            )),
            1
        );
        (content, tree)
    };

    let plane = hosted.plane();
    let fork_request = ForkRequest {
        source: source.clone(),
        at: 1,
        target: create_request("site-fork", true, Publish::Requests),
    };
    let forked = plane
        .fork_repository(&hosted.session.access_token, &fork_request, now())
        .unwrap();
    let retried = plane
        .fork_repository(&hosted.session.access_token, &fork_request, now())
        .unwrap();
    assert_eq!(retried.repository.repository, forked.repository.repository);
    assert!(!retried.repository.created);
    let target = forked.repository.repository.clone();
    assert_ne!(target, source);

    // Data: version 1 has the same ID and tree, and its content is in the fork's own store.
    let source_repo = Repository::open(hosted.node_dir(&source)).unwrap();
    let fork_repo = Repository::open(hosted.node_dir(&target)).unwrap();
    let (theirs, ours) = (
        source_repo.version(v(1)).unwrap(),
        fork_repo.version(v(1)).unwrap(),
    );
    assert_eq!((ours.id, ours.tree), (theirs.id, theirs.tree));
    assert_eq!(ours.tree, published_tree);
    assert_eq!(fork_repo.head().unwrap().number, v(1));
    assert_eq!(fork_repo.read_blob(content).unwrap(), b"inherited content");
    assert_eq!(forked.origin.source, source);
    assert_eq!(forked.origin.number, v(1));
    assert_eq!(forked.origin.version, theirs.id);

    // Credentials: the fork has its own permissions, with the forker as its only member.
    let fork_gate = hosted.gate(&target);
    let applied = fork_gate.permissions().unwrap();
    assert_eq!(applied.epoch, 1);
    assert_eq!(applied.required_approvals, 0);
    assert_eq!(
        applied
            .roles
            .keys()
            .map(PrincipalId::as_str)
            .collect::<Vec<_>>(),
        ["admin"]
    );
    let t = now();
    let need = |scope| Need { scope, layer: None };
    let source_member = member(&source, "rita", &[Scope::Read, Scope::Publish]);
    let as_fork_member = member(&target, "rita", &[Scope::Read, Scope::Publish]);
    let source_token = hosted.token(&source);
    assert_eq!(
        code_of(fork_gate.check(Some(&source_member), t, need(Scope::Publish))),
        "forbidden",
        "a source member's token"
    );
    assert_eq!(
        code_of(fork_gate.check(Some(&as_fork_member), t, need(Scope::Publish))),
        "forbidden",
        "a source member's role does not carry over"
    );
    assert_eq!(
        code_of(fork_gate.check(Some(&source_token), t, need(Scope::Read))),
        "forbidden",
        "the creator's token for the source"
    );
    let fork_token = hosted.token(&target);
    fork_gate
        .check(Some(&fork_token), t, need(Scope::Publish))
        .unwrap();

    // Review state stays with the source; the fork moves on its own.
    let mut fork_log = hosted.log(&target);
    let reply = review::handle(
        &mut fork_log,
        &fork_gate,
        Some(&fork_token),
        t,
        &Message::ReadRequest {
            request: request("pr-1"),
        },
    );
    assert!(
        reply.is_err(),
        "the source's request is not in the fork: {reply:?}"
    );
    fork_log.create_layer(&layer("next"), v(1)).unwrap();
    let next = write(&mut fork_log, "next", "write-f", "c.txt", b"fork only");
    let outcome = fork_log
        .publish_layer(
            &op("publish-f"),
            &layer("next"),
            next,
            v(1),
            &PublishAttribution::Human,
        )
        .unwrap();
    assert!(matches!(outcome, PublishOutcome::Published { number, .. } if number == v(2)));
    assert_eq!(head(&hosted.node_dir(&source)), v(1));
    verified(
        "gate4.fork-data-without-credentials",
        "fork through the control plane at version 1: same version ID and tree, content readable \
         from the fork's own store, fork_of recorded, retry returns the same fork; the fork's \
         permissions hold only the forker at epoch 1; source members' roles and every source \
         token are refused (forbidden); the source's Publish Requests are not in the fork; \
         publishing on the fork leaves the source unchanged",
    );
}

// ---------------------------------------------------------------------------------------------
// Gate 4: no access to restricted traces.

#[test]
fn restricted_traces_stay_with_maintainers() {
    const SECRET: &[u8] = b"agent trace: the staging key is in vault 7";
    let hosted = Hosted::start();
    let site = hosted.create("site", true, Publish::Requests);
    let id = site.repository.clone();
    hosted.push(&site, 2, &[("rita", Role::Review), ("bob", Role::Read)], 1);
    let gate = hosted.gate(&id);
    let maintainer = hosted.token(&id);
    let rita = member(
        &id,
        "rita",
        &[Scope::Read, Scope::Publish, Scope::ReviewApprove],
    );
    let bob = member(&id, "bob", &[Scope::Read]);

    let mut log = hosted.log(&id);
    log.create_layer(&layer("agent"), v(0)).unwrap();
    let content = log.repository().put_blob(b"public content").unwrap();
    let head = log
        .append(&op("write-1"), &layer("agent"), &file("out.txt", content))
        .unwrap()
        .checkpoint()
        .object();
    let trace = log.repository().put_blob(SECRET).unwrap();
    let outcome = log
        .publish_layer(
            &op("publish-1"),
            &layer("agent"),
            head,
            v(0),
            &PublishAttribution::Agent {
                context: Some(trace),
                allow_missing_trace: false,
            },
        )
        .unwrap();
    assert!(matches!(outcome, PublishOutcome::Published { .. }));

    let t = now();
    let traces = log.traces();
    assert_eq!(
        code_of(traces.restrict(&gate, log.repo(), Some(&rita), t, trace)),
        "forbidden",
        "only Maintain restricts"
    );
    traces
        .restrict(&gate, log.repo(), Some(&maintainer), t, trace)
        .unwrap();
    assert_eq!(
        traces.visibility(trace).unwrap(),
        TraceVisibility::Restricted
    );
    for (who, token) in [
        ("anonymous", None),
        ("Read", Some(&bob)),
        ("Review", Some(&rita)),
    ] {
        assert_eq!(
            code_of(traces.read(&gate, log.repo(), token.map(String::as_str), t, trace)),
            "forbidden",
            "{who} reads a restricted trace"
        );
    }
    assert_eq!(
        traces
            .read(&gate, log.repo(), Some(&maintainer), t, trace)
            .unwrap(),
        SECRET
    );

    // The wire never serves it, while ordinary content is served.
    let (addr, server) = serve_then_return(log, &id, 2);
    let refused_get = call(addr, &id, &Message::Get(vec![(Kind::Blob, trace)])).unwrap();
    let served = call(addr, &id, &Message::Get(vec![(Kind::Blob, content)])).unwrap();
    let log = server.join().unwrap();
    assert!(matches!(refused_get, Message::Error { ref code, .. } if code == "forbidden"));
    assert!(matches!(served, Message::Objects(ref objects) if objects.len() == 1));

    // A fork by a reader without Maintain leaves the trace out and marks it.
    let reader_fork = TempDir::new().unwrap();
    let mut target = FileWorldLog::open(reader_fork.path()).unwrap();
    let forked = fork::fork(&log, &id, &mut target, v(1), Role::Read).unwrap();
    assert_eq!(forked.omitted, [trace]);
    assert_eq!(fork::omitted_traces(&target).unwrap(), [trace]);
    assert!(!target.repo().has_blob(trace).unwrap());
    assert!(target.repo().has_blob(content).unwrap());
    assert!(!tree_contains(reader_fork.path(), SECRET));
    drop(log);

    // The forker's own fork (Maintain in the source) keeps the trace, still restricted.
    let maintained = hosted
        .plane()
        .fork_repository(
            &hosted.session.access_token,
            &ForkRequest {
                source: id.clone(),
                at: 1,
                target: create_request("site-fork", true, Publish::Requests),
            },
            now(),
        )
        .unwrap();
    assert!(maintained.omitted.is_empty());
    let fork_log = hosted.log(&maintained.repository.repository);
    assert_eq!(
        fork_log.traces().visibility(trace).unwrap(),
        TraceVisibility::Restricted
    );
    let fork_gate = hosted.gate(&maintained.repository.repository);
    assert_eq!(
        code_of(
            fork_log
                .traces()
                .read(&fork_gate, fork_log.repo(), None, now(), trace)
        ),
        "forbidden",
        "anonymous read of the restricted trace in the public fork"
    );
    verified(
        "gate4.restricted-traces",
        "only Maintain restricts; anonymous, Read and Review callers get forbidden; the loopback \
         wire refuses Get for the trace (forbidden) while serving other content; a Read-role fork \
         omits and marks it and its bytes are absent from the fork's directory; the maintainer's \
         fork keeps it restricted",
    );
}

/// Whether any file under `dir` holds `needle` verbatim.
fn tree_contains(dir: &Path, needle: &[u8]) -> bool {
    fs::read_dir(dir).unwrap().flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            tree_contains(&path, needle)
        } else {
            fs::read(&path).is_ok_and(|bytes| bytes.windows(needle.len()).any(|w| w == needle))
        }
    })
}

// ---------------------------------------------------------------------------------------------
// Gate 4 and the outage matrix: distinct control-plane and provider outcomes.

#[test]
fn control_plane_and_node_outages_have_distinct_outcomes() {
    let mut hosted = Hosted::start();
    let vault = hosted.create("vault", false, Publish::Direct);
    let id = vault.repository.clone();
    let dir = hosted.node_dir(&id);
    let token = hosted.token(&id);
    let gate = hosted.gate(&id);
    let mut log = hosted.log(&id);
    let mut heads = BTreeMap::new();
    for name in ["p", "q"] {
        log.create_layer(&layer(name), v(0)).unwrap();
        let path = format!("{name}.txt");
        heads.insert(
            name,
            write(
                &mut log,
                name,
                &format!("write-{name}"),
                &path,
                name.as_bytes(),
            ),
        );
    }
    let empty = log
        .create_layer(&layer("c"), v(0))
        .unwrap()
        .checkpoint()
        .object();
    let proxy = Proxy::start(serve_forever(log, &id));

    // Both down. Local work needs neither.
    proxy.set(Mode::Reset);
    let plane = hosted.plane.take().unwrap();
    drop(plane);
    let local = TempDir::new().unwrap();
    let mut repo = Repository::init(local.path()).unwrap();
    let base = repo.layer_base(v(0)).unwrap();
    let start = repo.create_layer(&layer("offline"), &base).unwrap();
    let content = repo.put_blob(b"written offline").unwrap();
    let captured = repo
        .capture_layer(
            &op("capture-offline"),
            &start,
            &file("offline.txt", content),
        )
        .unwrap();
    assert!(matches!(captured, javelin::LayerUpdate::Applied { .. }));
    verified(
        "outage.local-layer-work",
        "with the node and the control plane both down, a Layer is created and captured locally",
    );

    // Node down: uploads, appends and Publish queue; direct node actions refuse.
    let changes = file("c.txt", content);
    let bytes = changes.canonical_bytes();
    let checkpoint = Kind::Checkpoint.id(&bytes);
    let client = TempDir::new().unwrap();
    let mut outbox = Outbox::open(client.path(), "vault").unwrap();
    assert_eq!(Kind::Blob.id(b"written offline"), content);
    // Changes objects travel apart from content objects.
    let blob = encode(&Message::Objects(vec![(
        Kind::Blob,
        content,
        b"written offline".to_vec(),
    )]))
    .unwrap();
    let upload = encode(&Message::Objects(vec![(
        Kind::Checkpoint,
        checkpoint,
        bytes,
    )]))
    .unwrap();
    let append = encode(&Message::LayerAppend {
        operation: op("append-c"),
        layer: layer("c"),
        parent: Some(empty),
        checkpoint,
    })
    .unwrap();
    outbox
        .enqueue(Record::Upload, "c", &op("upload-blob"), &blob)
        .unwrap();
    outbox
        .enqueue(Record::Upload, "c", &op("upload-c"), &upload)
        .unwrap();
    outbox
        .enqueue(Record::Append, "c", &op("append-c"), &append)
        .unwrap();
    outbox
        .enqueue(
            Record::Publish,
            "p",
            &op("publish-p"),
            &publish_payload("publish-p", "p", heads["p"], 0),
        )
        .unwrap();
    let mut wire = Wire {
        addr: proxy.addr,
        name: id.clone(),
        clock: now(),
        token_expires: now() + hosted.session.expires_in,
        refresh_token: hosted.session.refresh_token.clone(),
        plane: None,
    };
    let down = outbox.replay(&mut wire).unwrap();
    assert!(down.delivered.is_empty());
    let node_fault = down.stalled.unwrap().fault;
    assert_eq!(node_fault.code(), NODE_UNAVAILABLE);
    assert_eq!(outbox.pending().count(), 4);
    assert_eq!(
        call(proxy.addr, &id, &Message::Resolve { version: None }),
        Err(NODE_UNAVAILABLE),
        "a direct read (what attach does) refuses"
    );
    assert_eq!(head(&dir), v(0));
    assert_eq!(proxy.forwarded(), 0, "nothing reached the node");

    // Control plane down, token still valid: node actions continue under applied permissions.
    proxy.set(Mode::Pass);
    gate.check(
        Some(&token),
        now(),
        Need {
            scope: Scope::Publish,
            layer: None,
        },
    )
    .unwrap();
    let up = outbox.replay(&mut wire).unwrap();
    assert!(up.stalled.is_none());
    assert_eq!(up.delivered.len(), 4);
    let outcomes: Vec<Message> = up
        .delivered
        .iter()
        .map(|delivered| decode(&delivered.outcome).unwrap())
        .collect();
    assert_eq!(outcomes[0], Message::Stored { count: 1 });
    assert_eq!(outcomes[1], Message::Stored { count: 1 });
    assert!(matches!(
        outcomes[2],
        Message::LayerOutcome(LayerOutcome::Accepted { seq: 1, .. })
    ));
    assert_eq!(published(&outcomes[3]).0, v(1));
    assert_eq!(head(&dir), v(1));

    // Control plane down, token expired: the record waits with control_plane_unavailable.
    outbox
        .enqueue(
            Record::Publish,
            "q",
            &op("publish-q"),
            &publish_payload("publish-q", "q", heads["q"], 0),
        )
        .unwrap();
    wire.clock = wire.token_expires;
    let expired = outbox.replay(&mut wire).unwrap();
    assert!(expired.delivered.is_empty());
    let plane_fault = expired.stalled.unwrap().fault;
    assert_eq!(plane_fault.code(), CONTROL_PLANE_UNAVAILABLE);
    assert_ne!(plane_fault.code(), node_fault.code());
    assert_eq!(outbox.pending().count(), 1);
    assert_eq!(head(&dir), v(1), "nothing was sent without a credential");

    // The control plane is back: refresh, then the record replays once.
    let plane = ControlPlane::open(hosted.dir.path()).unwrap();
    let mut wire = Wire {
        plane: Some(&plane),
        ..wire
    };
    let back = outbox.replay(&mut wire).unwrap();
    assert!(back.stalled.is_none());
    assert_eq!(
        published(&decode(&back.delivered[0].outcome).unwrap()).0,
        v(2)
    );
    assert_eq!(head(&dir), v(2));
    verified(
        "gate4.distinct-outage-outcomes",
        "node down (proxy resets): upload, LayerAppend and PublishLayer stay queued with \
         node_unavailable and a direct read refuses; control plane down with a valid token: the \
         node authorizes and the queue drains in order; token expired while the control plane is \
         down: the record waits with control_plane_unavailable, a distinct code, and replays once \
         after refresh",
    );
    not_verified(
        "outage.control-plane-refusals",
        "repo create, fork and refresh against an unreachable control plane: the product has no \
         network control-plane client yet, and an unreadable control directory surfaces as \
         storage_error rather than control_plane_unavailable (TASKS.md U14; the ignored gap_* tests)",
    );
    not_verified(
        "outage.stack-node-down",
        "Stack is not served by the node yet (LayerStack has no handler; U6 is open)",
    );
    not_verified(
        "outage.review-node-down",
        "Revise queueing and Approve/Comment refusal need a client path for review messages; \
         review is served in-process only (javelin_node::review), not on the node's wire",
    );
    not_verified(
        "outage.coordinator-transactions",
        "coordinator transaction behavior is accepted only against a real Cloudflare Durable \
         Object (docs/online-design.md section 6); a deployed account is out of scope",
    );
}

// ---------------------------------------------------------------------------------------------
// A selectable target: a node at a URL, such as the Cloudflare Worker on `wrangler dev` or a
// deployed Worker. `JAVELIN_ACCEPTANCE_NODE_URL` and `JAVELIN_ACCEPTANCE_NODE_ADMIN_TOKEN` name
// it, and `JAVELIN_ACCEPTANCE_TARGET` labels its report lines. Faults are injected by a proxy
// when the node is on loopback; a remote node cannot be put behind one, so those checks report
// NOT VERIFIED there.

struct Target {
    name: String,
    url: String,
    token: String,
}

impl Target {
    fn from_env() -> Self {
        let var = |name: &str| env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
        Self {
            name: env::var("JAVELIN_ACCEPTANCE_TARGET")
                .unwrap_or_else(|_| "cloudflare-workerd".into()),
            url: var("JAVELIN_ACCEPTANCE_NODE_URL"),
            token: var("JAVELIN_ACCEPTANCE_NODE_ADMIN_TOKEN"),
        }
    }

    /// What the control plane records as this repository's node: the target's own URL.
    fn label(&self) -> NodeTarget {
        NodeTarget::cloudflare(&self.url)
    }

    fn node(&self, url: &str) -> CloudflareNode {
        CloudflareNode::new(url, &self.token).unwrap()
    }

    /// The node's address when it is on loopback, so a proxy can stand in front of it.
    fn loopback(&self) -> Option<SocketAddr> {
        let rest = self.url.strip_prefix("http://")?;
        let authority = rest.split('/').next()?;
        let (host, port) = authority.rsplit_once(':')?;
        let host = match host {
            "localhost" | "127.0.0.1" => "127.0.0.1",
            _ => return None,
        };
        format!("{host}:{port}").parse().ok()
    }

    fn verified(&self, id: &str, detail: &str) {
        report(&self.name, id, "VERIFIED", detail);
    }

    fn not_verified(&self, id: &str, reason: &str) {
        report(&self.name, id, "NOT VERIFIED", reason);
    }
}

fn node_state(node: &CloudflareNode, created: &Created) -> (Option<(String, String)>, Value) {
    let snapshot = node.snapshot(&repository_id(created)).unwrap();
    (
        snapshot.genesis,
        snapshot.permissions.unwrap_or(Value::Null),
    )
}

#[test]
#[ignore = "needs a node URL; tools/acceptance/run.ts starts one on workerd"]
fn target_repository_flows_and_retries() {
    let target = Target::from_env();
    let dir = TempDir::new().unwrap();
    let plane = ControlPlane::with_node(
        dir.path(),
        Box::new(target.node(&target.url)),
        target.label(),
    )
    .unwrap();
    let hosted = Hosted::sign_in(dir, plane);
    target.verified(
        "flow.browser-device-approval",
        "device sign-in approved through the approval page's HTTP endpoints with a passkey, for a \
         control plane whose node is this target",
    );
    let plane = hosted.plane();
    let bearer = &hosted.session.access_token;
    let genesis = javelin_node_worker::genesis().unwrap();

    let site = hosted.create("site", true, Publish::Requests);
    let vault = hosted.create("vault", false, Publish::Direct);
    let node = target.node(&target.url);
    for (created, public) in [(&site, true), (&vault, false)] {
        assert!(created.created);
        let (held, permissions) = node_state(&node, created);
        assert_eq!(held.as_ref(), Some(&genesis), "one World 0 for every node");
        assert_eq!(permissions["epoch"], 1);
        assert_eq!(permissions["public"], public);
        assert_eq!(permissions["roles"]["admin"], "maintain");
        assert_eq!(permissions["roles"].as_object().unwrap().len(), 1);
    }
    let again = plane
        .create_repository(
            bearer,
            &create_request("vault", false, Publish::Direct),
            now(),
        )
        .unwrap();
    assert!(!again.created);
    assert_eq!(again.repository, vault.repository);
    assert_eq!(node_state(&node, &vault).1["epoch"], 1);
    let mut collision = create_request("vault", true, Publish::Direct);
    collision.operation = "create-vault-again".into();
    assert_eq!(
        plane
            .create_repository(bearer, &collision, now())
            .unwrap_err()
            .code(),
        "conflict"
    );
    let site_token = hosted.token(&site.repository);
    assert_eq!(
        plane
            .create_repository(
                &site_token,
                &create_request("x", true, Publish::Direct),
                now()
            )
            .unwrap_err()
            .code(),
        "forbidden"
    );
    target.verified(
        "flow.public-private",
        "public+requests and private+direct repositories created through the control plane are \
         provisioned on the node with the shared World 0 and epoch-1 permissions (creator \
         maintain, visibility applied)",
    );
    target.verified(
        "flow.retries",
        "repeating create with its operation ID returns the same repository and leaves the \
         node at epoch 1; a name collision is conflict; a repository token cannot create",
    );

    // A wrong admin token: the node refuses, and the control plane does not report success.
    let impostor = CloudflareNode::new(&target.url, "not-the-admin-token").unwrap();
    let probe = RepositoryId::parse("repo_u14_probe").unwrap();
    assert!(matches!(
        impostor.provision(&probe),
        Err(ControlError::Unauthorized(_))
    ));
    assert!(impostor.snapshot(&repository_id(&site)).is_err());
    target.verified(
        "flow.permissions",
        "the node refuses provision and state reads without its admin token; the creator is the \
         only role a new repository's permissions hold",
    );
    for (id, reason) in [
        (
            "gate4.exact-revision-authorization",
            "the Worker serves provision, permissions and state only (U13); review is not served",
        ),
        (
            "gate4.immutable-prior-revisions",
            "the Worker serves provision, permissions and state only (U13); review is not served",
        ),
        (
            "gate4.fork-data-without-credentials",
            "the Worker does not fork; ControlPlane::fork_repository needs a node that implements \
             NodeLink::fork",
        ),
        (
            "gate4.restricted-traces",
            "the Worker stores no Worlds or traces yet (U13 scope)",
        ),
        (
            "gate4.distinct-outage-outcomes",
            "the Worker has no repository wire for Layer writes or Publish yet, so the queued \
             node-down and control-plane-down outcomes cannot be driven through it",
        ),
        (
            "outage.coordinator-transactions",
            "workerd and miniflare are not accepted for coordinator transactions \
             (docs/online-design.md section 6); only a deployed Durable Object proves them",
        ),
    ] {
        target.not_verified(id, reason);
    }
}

#[test]
#[ignore = "needs a node URL; tools/acceptance/run.ts starts one on workerd"]
fn target_node_outage_and_lost_replies() {
    let target = Target::from_env();
    let dir = TempDir::new().unwrap();
    let control = dir.path().to_path_buf();
    // Every control plane here names the target's URL; the proxy, when there is one, stands in
    // for it. Without one, an unreachable address plays the node that is down.
    let proxy = target.loopback().map(Proxy::start);
    let via = proxy.as_ref().map_or_else(
        || target.url.clone(),
        |proxy| format!("http://{}", proxy.addr),
    );
    let plane = |url: &str| {
        ControlPlane::with_node(&control, Box::new(target.node(url)), target.label()).unwrap()
    };

    // The node is down: create fails with node_unavailable and nothing is reported done.
    let down = match &proxy {
        Some(proxy) => {
            proxy.set(Mode::Reset);
            plane(&via)
        }
        None => plane("http://127.0.0.1:1"),
    };
    let hosted = Hosted::sign_in(dir, down);
    let bearer = hosted.session.access_token.clone();
    let request = create_request("app", false, Publish::Direct);
    let refused = hosted
        .plane()
        .create_repository(&bearer, &request, now())
        .unwrap_err();
    assert_eq!(refused.code(), NODE_UNAVAILABLE);
    let mut hosted = hosted;
    hosted.plane = None;

    // A retry may not finish the create on another node.
    let elsewhere = ControlPlane::with_node(
        &control,
        Box::new(target.node(&target.url)),
        NodeTarget::cloudflare("http://127.0.0.1:1"),
    )
    .unwrap();
    assert_eq!(
        elsewhere
            .create_repository(&bearer, &request, now())
            .unwrap_err()
            .code(),
        "operation_id_reused"
    );
    drop(elsewhere);

    // The node is back: the same operation completes once.
    if let Some(proxy) = &proxy {
        proxy.set(Mode::Pass);
    }
    let live = plane(&via);
    let created = live.create_repository(&bearer, &request, now()).unwrap();
    assert_eq!(created.epoch, 1);
    let node = target.node(&target.url);
    assert_eq!(node_state(&node, &created).1["epoch"], 1);
    target.verified(
        "outage.node-down-create",
        "repo create with the node down fails with node_unavailable; a retry naming another \
         node is operation_id_reused; the retry with the same operation ID on the same node \
         completes at epoch 1",
    );

    let Some(proxy) = proxy else {
        target.not_verified(
            "gate4.lost-reply-one-outcome",
            "the target is not on loopback, so no proxy can drop a reply in front of it",
        );
        return;
    };
    let plane = live;

    // The provision reply is lost after the node stored World 0.
    let request = create_request("lost-provision", true, Publish::Requests);
    proxy.set(Mode::DropReplyOnce);
    assert_eq!(
        plane
            .create_repository(&bearer, &request, now())
            .unwrap_err()
            .code(),
        NODE_UNAVAILABLE
    );
    let first = plane.create_repository(&bearer, &request, now()).unwrap();
    // The permissions reply is lost after the node applied epoch 1.
    let request = create_request("lost-permissions", false, Publish::Direct);
    proxy.drop_reply_after(1);
    assert_eq!(
        plane
            .create_repository(&bearer, &request, now())
            .unwrap_err()
            .code(),
        NODE_UNAVAILABLE
    );
    let second = plane.create_repository(&bearer, &request, now()).unwrap();
    let genesis = javelin_node_worker::genesis().unwrap();
    for created in [&first, &second] {
        let (held, permissions) = node_state(&node, created);
        assert_eq!(held, Some(genesis.clone()));
        assert_eq!(permissions["epoch"], 1);
        let repeat = plane
            .create_repository(
                &bearer,
                &create_request(&created.name, created.public, created.publish),
                now(),
            )
            .unwrap();
        assert!(!repeat.created);
        assert_eq!(repeat.repository, created.repository);
    }
    assert!(proxy.forwarded() >= 6);
    target.verified(
        "gate4.lost-reply-one-outcome",
        "the node's reply to provision, then to the permissions push, is dropped after the node \
         acted; each retry of the same create completes with one World 0 and permissions at \
         epoch 1, and a repeat returns the same repository",
    );
}

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
