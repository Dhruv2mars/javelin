//! Writer leases on the wire, with real signed tokens: a Layer follows its owner across
//! devices, one device writes at a time, and `LayerTake` moves the lease
//! (`docs/direction.md`, Layers follow their owner; `docs/online-design.md` D21).

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
};

use javelin::{FileEntry, LayerId, ObjectId, OperationId, RepoPath, SparseChanges, VersionNumber};
use javelin_collaboration::{PrincipalId, Role};
use javelin_identity::test_keys;
use javelin_node::{
    FileWorldLog,
    auth::{self, Gate},
    bind_loopback, serve,
};
use javelin_protocol::{
    Message,
    codec::{Kind, LayerOutcome, LeaseHolder, code, decode, encode},
};
use tempfile::TempDir;

#[path = "wire_support/mod.rs"]
mod wire_support;
use wire_support::{OWNER, device_token};

const REPO: &str = "repo-1";
const LAPTOP: &str = "dev-laptop";
const DESKTOP: &str = "dev-desktop";

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}

fn layer(s: &str) -> LayerId {
    LayerId::parse(s).unwrap()
}

/// One file `path` holding `bytes`, as a changes object and its bytes.
fn changes(log: &mut FileWorldLog, path: &str, bytes: &[u8]) -> (ObjectId, Vec<u8>) {
    use javelin_protocol::receiver::WorldLog;
    let content = log.repository().put_blob(bytes).unwrap();
    let changes = SparseChanges::new([(
        RepoPath::parse(path).unwrap(),
        Some(FileEntry::Regular {
            content,
            executable: false,
        }),
    )])
    .unwrap();
    let bytes = changes.canonical_bytes();
    (Kind::Checkpoint.id(&bytes), bytes)
}

/// A node with Layers `work` and `other` over World 0, each at its empty first head, and
/// three changes objects for `work` whose blobs are stored. `stranger` writes too.
struct Node {
    addr: SocketAddr,
    first: ObjectId,
    other: ObjectId,
    edits: Vec<(ObjectId, Vec<u8>)>,
}

/// Seeds the node directory `dir`: its stored keys and permissions, the two Layers and the
/// edits' blobs. Returns the log and the node without an address.
fn seed(dir: &Path) -> (FileWorldLog, Node) {
    let mut permissions =
        wire_support::permissions(REPO, javelin_collaboration::Visibility::Public);
    permissions
        .roles
        .insert(PrincipalId::parse("stranger").unwrap(), Role::Maintain);
    auth::write_permissions(dir, &permissions).unwrap();
    auth::write_keys(dir, &test_keys::keys()).unwrap();
    let mut log = FileWorldLog::open(dir).unwrap();
    let base = VersionNumber::new(0).unwrap();
    let first = log
        .create_layer(&layer("work"), base)
        .unwrap()
        .checkpoint()
        .object();
    let other = log
        .create_layer(&layer("other"), base)
        .unwrap()
        .checkpoint()
        .object();
    let edits = (1..=3)
        .map(|n| changes(&mut log, "notes.txt", format!("edit {n}").as_bytes()))
        .collect();
    let node = Node {
        addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        first,
        other,
        edits,
    };
    (log, node)
}

/// A seeded node, served in process until the test process exits.
fn start() -> (TempDir, Node) {
    let dir = TempDir::new().unwrap();
    let (log, mut node) = seed(dir.path());
    let gate = Gate::load(dir.path(), REPO).unwrap();
    let listener = bind_loopback("127.0.0.1:0").unwrap();
    node.addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let mut log = log;
        let _ = serve(&listener, REPO, &mut log, &gate, None);
    });
    (dir, node)
}

/// The `javelin-node` binary serving `dir`, with `failpoint` set when given. Returns the
/// process and its address.
fn spawn(dir: &Path, failpoint: Option<&str>) -> (Child, SocketAddr) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_javelin-node"));
    command
        .args(["serve", dir.to_str().unwrap(), REPO, "127.0.0.1:0"])
        .stderr(Stdio::piped())
        .env_remove("JAVELIN_FAILPOINT");
    if let Some(failpoint) = failpoint {
        command.env("JAVELIN_FAILPOINT", failpoint);
    }
    let mut child = command.spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let addr = line.trim().rsplit(' ').next().unwrap().parse().unwrap();
    (child, addr)
}

/// One message with `token`, and the decoded reply.
fn send(addr: SocketAddr, token: &str, message: &Message) -> Message {
    try_send(addr, token, message).expect("the node did not reply")
}

/// One message with `token`, and the decoded reply, or `None` when the node closed first.
fn try_send(addr: SocketAddr, token: &str, message: &Message) -> Option<Message> {
    let body = encode(message).unwrap();
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "POST /v2/r/{REPO} HTTP/1.1\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .ok()?;
    stream.write_all(&body).ok()?;
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).ok()?;
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n")?;
    decode(&reply[split + 4..]).ok()
}

fn device(dev: &str) -> String {
    device_token(OWNER, REPO, dev, None)
}

impl Node {
    /// Uploads edit `n` and appends it to `work` after `parent`.
    fn append(&self, token: &str, operation: &str, parent: ObjectId, n: usize) -> LayerOutcome {
        let (id, bytes) = self.edits[n].clone();
        let stored = send(
            self.addr,
            token,
            &Message::Objects(vec![(Kind::Checkpoint, id, bytes)]),
        );
        assert_eq!(stored, Message::Stored { count: 1 });
        match send(
            self.addr,
            token,
            &Message::LayerAppend {
                operation: op(operation),
                layer: layer("work"),
                parent: Some(parent),
                checkpoint: id,
            },
        ) {
            Message::LayerOutcome(outcome) => outcome,
            other => panic!("not a Layer outcome: {other:?}"),
        }
    }

    fn take(&self, token: &str, operation: &str, name: &str) -> Message {
        send(
            self.addr,
            token,
            &Message::LayerTake {
                operation: op(operation),
                layer: layer(name),
            },
        )
    }

    /// The node's `(seq, head)` for `work`.
    fn head(&self) -> (u64, ObjectId) {
        match send(
            self.addr,
            &device(LAPTOP),
            &Message::LayerHead {
                layer: layer("work"),
            },
        ) {
            Message::LayerState { seq, head, .. } => (seq, head),
            other => panic!("not a Layer state: {other:?}"),
        }
    }
}

fn accepted(outcome: &LayerOutcome) -> (u64, ObjectId) {
    match outcome {
        LayerOutcome::Accepted { seq, checkpoint } => (*seq, *checkpoint),
        other => panic!("not accepted: {other:?}"),
    }
}

fn error_code(reply: Message) -> String {
    match reply {
        Message::Error { code, .. } => code,
        other => panic!("expected an error, got {other:?}"),
    }
}

#[test]
fn the_first_append_takes_the_lease_and_another_device_is_stale_without_a_write() {
    let (_dir, node) = start();
    let laptop = device(LAPTOP);
    let desktop = device(DESKTOP);

    let (seq, head) = accepted(&node.append(&laptop, "append-1", node.first, 0));
    assert_eq!(seq, 1);

    // The desktop holds no lease: its append, even on the current head, is stale.
    assert_eq!(
        node.append(&desktop, "append-2", head, 1),
        LayerOutcome::Stale { head }
    );
    assert_eq!(node.head(), (1, head), "nothing was written");
    // Nothing was recorded for the refused operation either: the laptop can use it.
    let (seq, _) = accepted(&node.append(&laptop, "append-2", head, 1));
    assert_eq!(seq, 2);
}

#[test]
fn take_moves_the_lease_once_and_a_retry_replays_it() {
    let (_dir, node) = start();
    let laptop = device(LAPTOP);
    let desktop = device(DESKTOP);
    let (_, head) = accepted(&node.append(&laptop, "append-1", node.first, 0));

    let taken = node.take(&desktop, "take-1", "work");
    assert_eq!(
        taken,
        Message::LayerOutcome(LayerOutcome::Accepted {
            seq: 1,
            checkpoint: head
        })
    );
    // The laptop's next append is stale; its earlier, accepted one still replays.
    assert_eq!(
        node.append(&laptop, "append-2", head, 1),
        LayerOutcome::Stale { head }
    );
    assert_eq!(
        accepted(&node.append(&laptop, "append-1", node.first, 0)),
        (1, head),
        "a lost reply's retry returns the recorded outcome"
    );
    let (seq, moved) = accepted(&node.append(&desktop, "append-3", head, 1));
    assert_eq!(seq, 2);

    // A retry replays the recorded outcome and does not move the lease again.
    let back = node.take(&laptop, "take-2", "work");
    assert!(matches!(
        back,
        Message::LayerOutcome(LayerOutcome::Accepted { seq: 2, .. })
    ));
    assert_eq!(node.take(&desktop, "take-1", "work"), taken);
    assert_eq!(
        node.append(&desktop, "append-4", moved, 2),
        LayerOutcome::Stale { head: moved },
        "the replayed take left the lease with the laptop"
    );
    // The same operation from another device is a different request.
    assert_eq!(
        node.take(&laptop, "take-1", "work"),
        Message::LayerOutcome(LayerOutcome::Refused {
            code: code::OPERATION_ID_REUSED.into()
        })
    );
    assert_eq!(
        node.take(&laptop, "take-3", "missing"),
        Message::LayerOutcome(LayerOutcome::Refused {
            code: "unknown_layer".into()
        })
    );
}

#[test]
fn only_the_owner_takes_the_lease_and_a_layer_limited_token_only_its_own() {
    let (_dir, node) = start();
    let laptop = device(LAPTOP);
    accepted(&node.append(&laptop, "append-1", node.first, 0));

    let stranger = device_token("stranger", REPO, "dev-stranger", None);
    assert_eq!(
        error_code(node.take(&stranger, "take-1", "work")),
        code::FORBIDDEN
    );
    let (_, head) = node.head();
    assert_eq!(
        node.append(&stranger, "append-2", head, 1),
        LayerOutcome::Stale { head },
        "another principal cannot write either"
    );

    // An agent's token, limited to `other`, cannot reach `work`'s lease; on its own Layer
    // it takes the lease, and the owner's other device can take it back.
    let agent = device_token(OWNER, REPO, DESKTOP, Some("other"));
    assert_eq!(
        error_code(node.take(&agent, "take-2", "work")),
        code::FORBIDDEN
    );
    assert_eq!(
        node.take(&agent, "take-3", "other"),
        Message::LayerOutcome(LayerOutcome::Accepted {
            seq: 0,
            checkpoint: node.other
        })
    );
    assert!(matches!(
        node.take(&laptop, "take-4", "other"),
        Message::LayerOutcome(LayerOutcome::Accepted { .. })
    ));
    assert_eq!(
        error_code(node.take(&stranger, "take-5", "other")),
        code::FORBIDDEN
    );
}

#[test]
fn a_stack_from_a_device_without_the_lease_is_stale() {
    let (_dir, node) = start();
    let laptop = device(LAPTOP);
    let desktop = device(DESKTOP);
    let (_, head) = accepted(&node.append(&laptop, "append-1", node.first, 0));
    let stack = |token: &str| {
        send(
            node.addr,
            token,
            &Message::LayerStack {
                operation: op("stack-1"),
                target: layer("work"),
                target_head: head,
                source: node.other,
            },
        )
    };
    assert_eq!(
        stack(&desktop),
        Message::LayerOutcome(LayerOutcome::Stale { head })
    );
    assert_eq!(node.head(), (1, head));
}

#[test]
fn a_crash_between_the_first_append_and_its_lease_keeps_the_layer_with_its_owner() {
    let dir = TempDir::new().unwrap();
    let (log, mut node) = seed(dir.path());
    drop(log);
    let laptop = device(LAPTOP);

    // The node stops right after the first append commits, before its lease is written.
    let (mut child, addr) = spawn(dir.path(), Some("layer_capture_after_commit"));
    node.addr = addr;
    let (id, bytes) = node.edits[0].clone();
    assert_eq!(
        send(
            addr,
            &laptop,
            &Message::Objects(vec![(Kind::Checkpoint, id, bytes)])
        ),
        Message::Stored { count: 1 }
    );
    let append = Message::LayerAppend {
        operation: op("append-1"),
        layer: layer("work"),
        parent: Some(node.first),
        checkpoint: id,
    };
    assert_eq!(
        try_send(addr, &laptop, &append),
        None,
        "no reply before the crash"
    );
    assert_eq!(child.wait().unwrap().code(), Some(86));

    // After the restart the Layer is the first append's principal's: another principal
    // cannot take it, and neither can it write, while the owner's other device can take it.
    let (mut child, addr) = spawn(dir.path(), None);
    node.addr = addr;
    let stranger = device_token("stranger", REPO, "dev-stranger", None);
    assert_eq!(
        error_code(node.take(&stranger, "take-1", "work")),
        code::FORBIDDEN
    );
    let (seq, head) = node.head();
    assert_eq!(seq, 1, "the append that committed before the crash is kept");
    assert_eq!(
        node.append(&stranger, "append-2", head, 1),
        LayerOutcome::Stale { head }
    );
    // The laptop holds the lease the claim became: its retry replays, its next write lands.
    assert_eq!(
        accepted(&node.append(&laptop, "append-1", node.first, 0)),
        (1, head)
    );
    assert!(matches!(
        node.take(&device(DESKTOP), "take-2", "work"),
        Message::LayerOutcome(LayerOutcome::Accepted { seq: 1, .. })
    ));
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn a_crash_before_the_first_append_commits_leaves_no_owner() {
    let dir = TempDir::new().unwrap();
    let (log, mut node) = seed(dir.path());
    drop(log);
    let laptop = device(LAPTOP);
    let (mut child, addr) = spawn(dir.path(), Some("layer_capture_before_commit"));
    node.addr = addr;
    let (id, bytes) = node.edits[0].clone();
    send(
        addr,
        &laptop,
        &Message::Objects(vec![(Kind::Checkpoint, id, bytes)]),
    );
    let append = Message::LayerAppend {
        operation: op("append-1"),
        layer: layer("work"),
        parent: Some(node.first),
        checkpoint: id,
    };
    assert_eq!(try_send(addr, &laptop, &append), None);
    assert_eq!(child.wait().unwrap().code(), Some(86));

    // Nothing was written, so the claim is void: the Layer is still free, and the
    // retry's write takes it.
    let (mut child, addr) = spawn(dir.path(), None);
    node.addr = addr;
    assert_eq!(node.head().0, 0);
    let (seq, head) = accepted(&node.append(&laptop, "append-1", node.first, 0));
    assert_eq!(seq, 1);
    assert_eq!(
        node.append(&device(DESKTOP), "append-2", head, 1),
        LayerOutcome::Stale { head }
    );
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn layer_head_tells_the_caller_whether_its_device_holds_the_lease() {
    let (_dir, node) = start();
    let laptop = device(LAPTOP);
    let lease = |token: &str| match send(
        node.addr,
        token,
        &Message::LayerHead {
            layer: layer("work"),
        },
    ) {
        Message::LayerState { lease, .. } => lease,
        other => panic!("not a Layer state: {other:?}"),
    };
    assert_eq!(lease(&laptop), LeaseHolder::Free);
    accepted(&node.append(&laptop, "append-1", node.first, 0));
    assert_eq!(lease(&laptop), LeaseHolder::Caller);
    assert_eq!(lease(&device(DESKTOP)), LeaseHolder::Other);
}
