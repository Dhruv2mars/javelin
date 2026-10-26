//! Shared pieces: the report, the browser, the hosted deployment, the node and its wire.

pub use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, ErrorKind, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub use javelin::{
    CheckpointId, FileEntry, LayerId, ObjectId, OperationId, PublishAttribution, RepoPath,
    Repository, SparseChanges, VersionNumber,
    outbox::{Entry, Fault, Kind as Record, Outbox, Transport},
};
pub use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
pub use javelin_control::{
    ControlError, ControlPlane, CreateRequest, Created, ForkRequest, LocalNode, NodeLink,
    NodeTarget, Poll, Publish, RelyingParty, Session, answer,
};
pub use javelin_identity::{KeySet, Need, Scope, test_keys};
pub use javelin_node::{
    FileWorldLog, TraceVisibility, auth::Gate, bind_loopback, fork, review, serve,
};
pub use javelin_node_worker::CloudflareNode;
pub use javelin_protocol::{
    Message, ProtocolError,
    codec::{
        Kind, LayerOutcome, PublishOutcome, RequestId, RequestStatus, ReviewOutcome, decode, encode,
    },
    receiver::WorldLog,
};
pub use serde_json::{Value, json};
pub use tempfile::TempDir;
pub use webauthn_authenticator_rs::{WebauthnAuthenticator, softpasskey::SoftPasskey};
pub use webauthn_rs::prelude::{
    CreationChallengeResponse, RegisterPublicKeyCredential, RequestChallengeResponse, Url,
};

pub const ORIGIN: &str = "http://localhost:8080";
pub const NODE_UNAVAILABLE: &str = "node_unavailable";
pub const CONTROL_PLANE_UNAVAILABLE: &str = "control_plane_unavailable";
pub const REVIEWER: &[Scope] = &[
    Scope::Read,
    Scope::LayerWrite,
    Scope::ReviewWrite,
    Scope::ReviewApprove,
];

// ---------------------------------------------------------------------------------------------
// The report.

pub fn report(target: &str, id: &str, status: &str, detail: &str) {
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

pub fn verified(id: &str, detail: &str) {
    report("self-hosted", id, "VERIFIED", detail);
}

pub fn not_verified(id: &str, reason: &str) {
    report("self-hosted", id, "NOT VERIFIED", reason);
}

// ---------------------------------------------------------------------------------------------
// Small values.

/// The real clock: the approval page reads it, so every token and code here uses it too.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after 1970")
        .as_secs()
}

pub fn op(value: &str) -> OperationId {
    OperationId::parse(value).unwrap()
}

pub fn layer(name: &str) -> LayerId {
    LayerId::parse(name).unwrap()
}

pub fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}

pub fn request(id: &str) -> RequestId {
    RequestId::parse(id).unwrap()
}

pub fn create_request(name: &str, public: bool, publish: Publish) -> CreateRequest {
    CreateRequest {
        owner: "acme".into(),
        name: name.into(),
        public,
        publish,
        operation: format!("create-{name}"),
    }
}

pub fn repository_id(created: &Created) -> RepositoryId {
    RepositoryId::parse(&created.repository).unwrap()
}

/// A token for a member the control plane cannot name yet, signed by the public test key.
pub fn member(repository: &str, sub: &str, scopes: &[Scope]) -> String {
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

pub struct Response {
    pub status: u16,
    pub head: String,
    pub body: String,
}

/// One HTTP/1.1 request with `Connection: close`, read to the end.
pub fn http(
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

pub fn json_of(response: &Response) -> Value {
    serde_json::from_str(&response.body)
        .unwrap_or_else(|error| panic!("body is not JSON ({error}): {}", response.body))
}

/// What a browser does to approve a device code: enroll a passkey with the admin token, sign in
/// with it, and approve the code with the sign-in cookie.
pub fn browser(addr: &str, admin: &str, user_code: &str) {
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
pub fn approve_in_browser(plane: &ControlPlane, admin: &str, user_code: &str) {
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

pub struct Hosted {
    pub dir: TempDir,
    /// `None` while the control plane is down.
    pub plane: Option<ControlPlane>,
    pub session: Session,
    /// What the nodes trust: the control plane's key, and the public test key for members.
    pub keys: KeySet,
}

impl Hosted {
    /// Bootstraps the control plane and signs this device in: `device_start`, approval in the
    /// browser over HTTP, then `device_poll`.
    pub fn start() -> Self {
        let dir = TempDir::new().unwrap();
        let plane = ControlPlane::open(dir.path()).unwrap();
        Self::sign_in(dir, plane)
    }

    pub fn sign_in(dir: TempDir, plane: ControlPlane) -> Self {
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

    pub fn plane(&self) -> &ControlPlane {
        self.plane.as_ref().expect("the control plane is up")
    }

    pub fn nodes(&self) -> LocalNode {
        LocalNode::new(self.dir.path().join("nodes"))
    }

    pub fn create(&self, name: &str, public: bool, publish: Publish) -> Created {
        self.plane()
            .create_repository(
                &self.session.access_token,
                &create_request(name, public, publish),
                now(),
            )
            .unwrap()
    }

    /// The control plane's access token for `repository`, as its creator.
    pub fn token(&self, repository: &str) -> String {
        self.plane()
            .repository_token(&self.session.access_token, repository, now())
            .unwrap()
            .access_token
    }

    pub fn node_dir(&self, repository: &str) -> PathBuf {
        self.nodes()
            .repository_dir(&RepositoryId::parse(repository).unwrap())
    }

    /// The node's gate, with the permissions the node holds.
    pub fn gate(&self, repository: &str) -> Gate {
        self.nodes()
            .gate(&RepositoryId::parse(repository).unwrap(), self.keys.clone())
            .unwrap()
    }

    pub fn log(&self, repository: &str) -> FileWorldLog {
        FileWorldLog::open(self.node_dir(repository)).unwrap()
    }

    /// Pushes permissions at `epoch`: the creator (`admin`) keeps Maintain, plus `members`.
    pub fn push(&self, created: &Created, epoch: u64, members: &[(&str, Role)], required: u32) {
        self.nodes()
            .apply(&permissions(created, epoch, members, required))
            .unwrap();
    }
}

pub fn permissions(
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
pub fn write(
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

pub fn file(path: &str, content: ObjectId) -> SparseChanges {
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
pub fn tree_of(log: &mut FileWorldLog, checkpoint: ObjectId) -> ObjectId {
    let repo = log.repository();
    let manifest = repo
        .checkpoint_manifest(CheckpointId::parse(&checkpoint.to_string()).unwrap())
        .unwrap();
    repo.put_snapshot(&manifest).unwrap()
}

pub fn send(log: &mut FileWorldLog, gate: &Gate, token: &str, message: Message) -> Message {
    review::handle(log, gate, Some(token), now(), &message).unwrap()
}

pub fn open_request(
    operation: &str,
    id: &str,
    name: &str,
    checkpoint: ObjectId,
    base: u64,
) -> Message {
    Message::OpenRequest {
        operation: op(operation),
        request: request(id),
        layer: layer(name),
        checkpoint,
        base: v(base),
    }
}

pub fn revise(operation: &str, id: &str, expected: u32, checkpoint: ObjectId) -> Message {
    Message::Revise {
        operation: op(operation),
        request: request(id),
        expected_revision: expected,
        checkpoint,
        base: v(0),
    }
}

pub fn approve(operation: &str, id: &str, revision: u32) -> Message {
    Message::Approve {
        operation: op(operation),
        request: request(id),
        revision,
    }
}

pub fn publish_request(operation: &str, id: &str, revision: u32, base: u64) -> Message {
    Message::PublishRequest {
        operation: op(operation),
        request: request(id),
        revision,
        base: v(base),
    }
}

pub fn accepted(reply: Message) -> u32 {
    match reply {
        Message::ReviewOutcome(ReviewOutcome::Accepted { revision }) => revision,
        other => panic!("expected accepted, got {other:?}"),
    }
}

pub fn refused(reply: Message) -> String {
    match reply {
        Message::ReviewOutcome(ReviewOutcome::Refused { code })
        | Message::PublishOutcome(PublishOutcome::Refused { code }) => code,
        other => panic!("expected refused, got {other:?}"),
    }
}

pub fn published(reply: &Message) -> (VersionNumber, ObjectId) {
    match reply {
        Message::PublishOutcome(PublishOutcome::Published { number, tree, .. }) => (*number, *tree),
        other => panic!("expected published, got {other:?}"),
    }
}

/// Every stored row of a request, in order: the review partition's hash chain.
/// `(seq, operation, command, reply, chain)` of one stored review row.
pub type ReviewRow = (i64, String, Vec<u8>, Vec<u8>, String);

pub fn review_rows(node_dir: &Path, id: &str) -> Vec<ReviewRow> {
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
pub fn serve_forever(log: FileWorldLog, name: &str) -> SocketAddr {
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
pub fn serve_then_return(
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
pub fn post(addr: SocketAddr, name: &str, body: &[u8]) -> Result<Vec<u8>, &'static str> {
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

pub fn call(addr: SocketAddr, name: &str, message: &Message) -> Result<Message, &'static str> {
    let reply = post(addr, name, &encode(message).unwrap())?;
    Ok(decode(&reply).unwrap())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// Forward every request.
    Pass,
    /// Close every connection before forwarding: the node is down.
    Reset,
    /// Forward the next request, then close without relaying the reply: the node decided and
    /// the client never heard.
    DropReplyOnce,
}

#[derive(Default)]
pub struct ProxyState {
    pub reset: bool,
    pub forwarded: usize,
    /// The `forwarded` count whose reply is dropped.
    pub drop_at: Option<usize>,
}

/// A proxy in front of a node, one request per connection: it forces `Connection: close` on
/// both sides so a keep-alive client cannot carry two requests past one decision.
pub struct Proxy {
    pub addr: SocketAddr,
    pub state: Arc<Mutex<ProxyState>>,
}

impl Proxy {
    pub fn start(upstream: SocketAddr) -> Self {
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

    pub fn set(&self, mode: Mode) {
        let mut state = self.state.lock().unwrap();
        state.reset = mode == Mode::Reset;
        state.drop_at = (mode == Mode::DropReplyOnce).then_some(state.forwarded + 1);
    }

    /// Forwards `passed` requests, then drops the reply to the one after.
    pub fn drop_reply_after(&self, passed: usize) {
        let mut state = self.state.lock().unwrap();
        state.reset = false;
        state.drop_at = Some(state.forwarded + passed + 1);
    }

    pub fn forwarded(&self) -> usize {
        self.state.lock().unwrap().forwarded
    }
}

/// `message` (a request or a reply) with its `Connection` header replaced by `close`.
pub fn close(message: &[u8]) -> Vec<u8> {
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
pub fn read_request(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
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

pub fn forward(upstream: SocketAddr, request: &[u8]) -> Vec<u8> {
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

/// This suite's stand-in for a client outbox transport, which the product does not have yet:
/// each record's payload is one request to the node. It models token expiry only: an expired
/// token needs the control plane, and while that is down the record waits with
/// `control_plane_unavailable`. No credential reaches the node.
pub struct Wire<'a> {
    pub addr: SocketAddr,
    pub name: String,
    pub clock: u64,
    pub token_expires: u64,
    pub refresh_token: String,
    pub plane: Option<&'a ControlPlane>,
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

pub fn publish_payload(operation: &str, name: &str, expected: ObjectId, base: u64) -> Vec<u8> {
    encode(&Message::PublishLayer {
        operation: op(operation),
        layer: layer(name),
        expected,
        base: v(base),
        attribution: PublishAttribution::Human,
    })
    .unwrap()
}

pub fn head(node_dir: &Path) -> VersionNumber {
    Repository::open(node_dir).unwrap().head().unwrap().number
}

pub fn code_of<T: std::fmt::Debug>(result: Result<T, ProtocolError>) -> String {
    result.unwrap_err().code
}
