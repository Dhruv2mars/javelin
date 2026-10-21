//! Outbox replay against the loopback node, through a fault-injecting proxy. The proxy
//! sits between the replay engine and the node; each test chooses how the next
//! connection fails: reset before forwarding (node down), forward and drop the reply
//! (lost reply after commit), or delay. The node is the real `javelin-node` server.

use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use javelin::{
    FileEntry, LayerId, ObjectId, OperationId, PublishAttribution, RepoPath, Repository,
    SparseChanges, VersionNumber,
    outbox::{Entry, Fault, Kind, Outbox, Transport},
};
use javelin_node::{FileWorldLog, bind_loopback, serve};
use javelin_protocol::{
    Message,
    codec::{PublishOutcome, decode, encode},
    receiver::WorldLog,
};
use tempfile::TempDir;

const REPO: &str = "demo";

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}

fn layer(s: &str) -> LayerId {
    LayerId::parse(s).unwrap()
}

fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}

/// Starts a loopback node in `dir` with one Layer per `(layer, path)` pair over World 0.
/// Each Layer writes its own name into its path. Returns the node's address and each
/// Layer's head.
fn start_node(dir: &Path, layers: &[(&str, &str)]) -> (SocketAddr, Vec<ObjectId>) {
    let mut log = FileWorldLog::open(dir).unwrap();
    let mut heads = Vec::new();
    for (name, path) in layers {
        log.create_layer(&layer(name), v(0)).unwrap();
        let content = log.repository().put_blob(name.as_bytes()).unwrap();
        let changes = SparseChanges::new([(
            RepoPath::parse(path).unwrap(),
            Some(FileEntry::Regular {
                content,
                executable: false,
            }),
        )])
        .unwrap();
        let head = log
            .append(&op(&format!("write-{name}")), &layer(name), &changes)
            .unwrap();
        heads.push(head.checkpoint().object());
    }
    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let mut log = log;
        // Serves until the test process exits.
        serve(&listener, REPO, &mut log, None).unwrap();
    });
    (addr, heads)
}

/// The encoded request a client queues for one Layer's publish.
fn publish_payload(operation: &OperationId, name: &str, expected: ObjectId) -> Vec<u8> {
    encode(&Message::PublishLayer {
        operation: operation.clone(),
        layer: layer(name),
        expected,
        base: v(0),
        attribution: PublishAttribution::Human,
    })
    .unwrap()
}

fn queue_publish(outbox: &mut Outbox, name: &str, expected: ObjectId) -> u64 {
    let operation = op(&format!("publish-{name}"));
    let payload = publish_payload(&operation, name, expected);
    outbox
        .enqueue(Kind::Publish, name, &operation, &payload)
        .unwrap()
}

/// The World version the node has published, read from the repository on disk.
fn head(root: &Path) -> VersionNumber {
    Repository::open(root).unwrap().head().unwrap().number
}

/// The node's publish outcome, decoded from a recorded reply.
fn outcome(bytes: &[u8]) -> PublishOutcome {
    match decode(bytes).unwrap() {
        Message::PublishOutcome(outcome) => outcome,
        other => panic!("not a publish outcome: {other:?}"),
    }
}

fn published_number(bytes: &[u8]) -> VersionNumber {
    match outcome(bytes) {
        PublishOutcome::Published { number, .. } => number,
        other => panic!("not published: {other:?}"),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    /// Forward every connection.
    Pass,
    /// Close every connection before forwarding: the node is down.
    Reset,
    /// Forward the next connection, then close without relaying the reply: the node
    /// committed and the client never heard.
    DropReplyOnce,
    /// Wait before forwarding the next connection.
    DelayOnce(Duration),
}

struct State {
    mode: Mode,
    forwarded: usize,
}

/// A proxy in front of the node. `forwarded` counts requests that reached the node.
struct Proxy {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
}

impl Proxy {
    fn start(upstream: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State {
            mode: Mode::Pass,
            forwarded: 0,
        }));
        let shared = Arc::clone(&state);
        thread::spawn(move || {
            for client in listener.incoming() {
                let Ok(mut client) = client else { continue };
                let mode = {
                    let mut state = shared.lock().unwrap();
                    let mode = state.mode;
                    if !matches!(mode, Mode::Reset) {
                        state.mode = Mode::Pass;
                    }
                    mode
                };
                if mode == Mode::Reset {
                    continue;
                }
                let Ok(request) = read_request(&mut client) else {
                    continue;
                };
                if let Mode::DelayOnce(wait) = mode {
                    thread::sleep(wait);
                }
                shared.lock().unwrap().forwarded += 1;
                let reply = forward(upstream, &request);
                if mode != Mode::DropReplyOnce {
                    let _ = client.write_all(&reply);
                }
            }
        });
        Self { addr, state }
    }

    fn set(&self, mode: Mode) {
        self.state.lock().unwrap().mode = mode;
    }

    fn forwarded(&self) -> usize {
        self.state.lock().unwrap().forwarded
    }
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
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        data.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&data[..end]).into_owned();
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    while data.len() < end + length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        data.extend_from_slice(&chunk[..read]);
    }
    Ok(data)
}

/// Sends the request to the node and returns the whole reply. Failures reply with nothing.
fn forward(upstream: SocketAddr, request: &[u8]) -> Vec<u8> {
    let Ok(mut node) = TcpStream::connect(upstream) else {
        return Vec::new();
    };
    let mut reply = Vec::new();
    if node.write_all(request).is_ok() {
        let _ = node.read_to_end(&mut reply);
    }
    reply
}

/// The replay engine's transport for the loopback node: one `PublishLayer` per
/// connection, the reply recorded as the outcome. `token` stands for a valid credential.
struct Loopback {
    addr: SocketAddr,
    token: Arc<AtomicBool>,
}

impl Loopback {
    fn new(addr: SocketAddr, token: &Arc<AtomicBool>) -> Self {
        Self {
            addr,
            token: Arc::clone(token),
        }
    }
}

impl Transport for Loopback {
    fn deliver(&mut self, entry: &Entry) -> Result<Vec<u8>, Fault> {
        if !self.token.load(Ordering::SeqCst) {
            return Err(Fault::ControlPlaneUnavailable("token expired".into()));
        }
        // The node serves only PublishLayer. Append waits for U5.
        if entry.kind != Kind::Publish {
            return Err(Fault::NodeUnavailable(format!(
                "node does not serve {} yet",
                entry.kind.as_str()
            )));
        }
        let unreachable = |error: io::Error| Fault::NodeUnavailable(error.to_string());
        let mut stream = TcpStream::connect(self.addr).map_err(unreachable)?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(unreachable)?;
        let head = format!(
            "POST /v2/r/{REPO} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n",
            entry.payload.len()
        );
        stream
            .write_all(head.as_bytes())
            .and_then(|()| stream.write_all(&entry.payload))
            .map_err(unreachable)?;
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).map_err(unreachable)?;
        let Some(split) = reply.windows(4).position(|w| w == b"\r\n\r\n") else {
            return Err(Fault::NodeUnavailable("no reply".into()));
        };
        let status = std::str::from_utf8(&reply[..split])
            .ok()
            .and_then(|head| head.split(' ').nth(1))
            .and_then(|code| code.parse::<u16>().ok());
        if status != Some(200) {
            return Err(Fault::NodeUnavailable(format!("status {status:?}")));
        }
        let body = reply[split + 4..].to_vec();
        match decode(&body) {
            Ok(Message::PublishOutcome(_)) => Ok(body),
            // The node could not decide: the same operation is retried later.
            Ok(Message::Error { code, .. }) => Err(Fault::NodeUnavailable(code)),
            _ => Err(Fault::NodeUnavailable("unexpected reply".into())),
        }
    }
}

fn valid_token() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(true))
}

#[test]
fn node_down_then_up_replays_each_record_once_in_journal_order() {
    let root = TempDir::new().unwrap();
    let layers = [("l0", "f0"), ("l1", "f1"), ("l2", "f2"), ("l3", "f3")];
    let (node, heads) = start_node(root.path(), &layers);
    let proxy = Proxy::start(node);
    let token = valid_token();
    let mut outbox = Outbox::open(root.path(), REPO).unwrap();
    for ((name, _), expected) in layers.iter().zip(&heads) {
        queue_publish(&mut outbox, name, *expected);
    }

    proxy.set(Mode::Reset);
    let down = outbox
        .replay(&mut Loopback::new(proxy.addr, &token))
        .unwrap();
    assert!(down.delivered.is_empty());
    let stall = down.stalled.unwrap();
    assert_eq!((stall.seq, stall.fault.code()), (1, "node_unavailable"));
    assert_eq!(proxy.forwarded(), 0);
    assert_eq!(outbox.pending().count(), 4);

    proxy.set(Mode::Pass);
    let up = outbox
        .replay(&mut Loopback::new(proxy.addr, &token))
        .unwrap();
    assert!(up.stalled.is_none());
    let seqs: Vec<u64> = up.delivered.iter().map(|d| d.seq).collect();
    assert_eq!(seqs, [1, 2, 3, 4]);
    // Each Layer's publish landed as its own World version, in journal order.
    let numbers: Vec<_> = up
        .delivered
        .iter()
        .map(|d| published_number(&d.outcome))
        .collect();
    assert_eq!(numbers, [v(1), v(2), v(3), v(4)]);
    assert_eq!(proxy.forwarded(), 4);
    assert_eq!(head(root.path()), v(4));
    assert_eq!(outbox.pending().count(), 0);
}

#[test]
fn lost_reply_after_commit_replays_the_recorded_outcome_once() {
    let root = TempDir::new().unwrap();
    let (node, heads) = start_node(root.path(), &[("l0", "f0")]);
    let proxy = Proxy::start(node);
    let token = valid_token();
    let mut outbox = Outbox::open(root.path(), REPO).unwrap();
    queue_publish(&mut outbox, "l0", heads[0]);

    proxy.set(Mode::DropReplyOnce);
    let lost = outbox
        .replay(&mut Loopback::new(proxy.addr, &token))
        .unwrap();
    assert!(lost.delivered.is_empty());
    assert_eq!(lost.stalled.unwrap().fault.code(), "node_unavailable");
    assert_eq!(
        head(root.path()),
        v(1),
        "the node committed before the reply was lost"
    );

    // The client restarts: the journal still holds the record, and the retry is answered
    // from the node's recorded outcome.
    drop(outbox);
    let mut outbox = Outbox::open(root.path(), REPO).unwrap();
    assert_eq!(outbox.pending().count(), 1);
    let retry = outbox
        .replay(&mut Loopback::new(proxy.addr, &token))
        .unwrap();
    assert!(retry.stalled.is_none());
    assert_eq!(published_number(&retry.delivered[0].outcome), v(1));
    assert_eq!(proxy.forwarded(), 2);
    assert_eq!(
        head(root.path()),
        v(1),
        "one durable outcome, one World version"
    );
}

#[test]
fn a_record_queued_before_a_crash_is_sent_once_after_restart() {
    let root = TempDir::new().unwrap();
    let (node, heads) = start_node(root.path(), &[("l0", "f0")]);
    let proxy = Proxy::start(node);
    let token = valid_token();

    // Crash after the journal append and before any send.
    let mut outbox = Outbox::open(root.path(), REPO).unwrap();
    queue_publish(&mut outbox, "l0", heads[0]);
    drop(outbox);

    let mut outbox = Outbox::open(root.path(), REPO).unwrap();
    let report = outbox
        .replay(&mut Loopback::new(proxy.addr, &token))
        .unwrap();
    assert_eq!(report.delivered.len(), 1);
    assert_eq!(proxy.forwarded(), 1);
    assert_eq!(head(root.path()), v(1));

    let again = outbox
        .replay(&mut Loopback::new(proxy.addr, &token))
        .unwrap();
    assert_eq!(again, Default::default());
    assert_eq!(
        proxy.forwarded(),
        1,
        "an acknowledged record is not sent again"
    );
}

#[test]
fn a_slow_node_delays_delivery_without_duplicating_it() {
    let root = TempDir::new().unwrap();
    let (node, heads) = start_node(root.path(), &[("l0", "f0")]);
    let proxy = Proxy::start(node);
    let token = valid_token();
    let mut outbox = Outbox::open(root.path(), REPO).unwrap();
    queue_publish(&mut outbox, "l0", heads[0]);

    proxy.set(Mode::DelayOnce(Duration::from_millis(200)));
    let started = Instant::now();
    let report = outbox
        .replay(&mut Loopback::new(proxy.addr, &token))
        .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert_eq!(report.delivered.len(), 1);
    assert_eq!(proxy.forwarded(), 1);
    assert_eq!(head(root.path()), v(1));
}

#[test]
fn a_conflicting_publish_is_recorded_as_its_outcome_not_retried() {
    let root = TempDir::new().unwrap();
    let (node, heads) = start_node(root.path(), &[("l0", "shared.txt"), ("l1", "shared.txt")]);
    let proxy = Proxy::start(node);
    let token = valid_token();
    let mut outbox = Outbox::open(root.path(), REPO).unwrap();
    queue_publish(&mut outbox, "l0", heads[0]);
    queue_publish(&mut outbox, "l1", heads[1]);

    let report = outbox
        .replay(&mut Loopback::new(proxy.addr, &token))
        .unwrap();
    assert!(report.stalled.is_none());
    assert_eq!(published_number(&report.delivered[0].outcome), v(1));
    assert!(
        matches!(
            outcome(&report.delivered[1].outcome),
            PublishOutcome::Conflict { .. }
        ),
        "{:?}",
        outcome(&report.delivered[1].outcome)
    );
    assert_eq!(outbox.pending().count(), 0);
    assert_eq!(head(root.path()), v(1));
}

#[test]
fn an_expired_token_holds_the_record_until_it_is_renewed() {
    let root = TempDir::new().unwrap();
    let (node, heads) = start_node(root.path(), &[("l0", "f0")]);
    let proxy = Proxy::start(node);
    let token = valid_token();
    let mut outbox = Outbox::open(root.path(), REPO).unwrap();
    queue_publish(&mut outbox, "l0", heads[0]);

    token.store(false, Ordering::SeqCst);
    let held = outbox
        .replay(&mut Loopback::new(proxy.addr, &token))
        .unwrap();
    let stall = held.stalled.unwrap();
    assert_eq!(stall.fault.code(), "control_plane_unavailable");
    assert_eq!(
        proxy.forwarded(),
        0,
        "nothing reaches the node without a credential"
    );
    assert_eq!(outbox.pending().count(), 1, "nothing is dropped");

    token.store(true, Ordering::SeqCst);
    let renewed = outbox
        .replay(&mut Loopback::new(proxy.addr, &token))
        .unwrap();
    assert_eq!(renewed.delivered.len(), 1);
    assert_eq!(head(root.path()), v(1));
}
