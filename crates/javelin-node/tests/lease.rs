//! Writer leases on the wire, with real signed tokens: a Layer follows its owner across
//! devices, one device writes at a time, and `LayerTake` moves the lease
//! (`docs/direction.md`, Layers follow their owner; `docs/online-design.md` D21).

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    thread,
};

use javelin::{FileEntry, LayerId, ObjectId, OperationId, RepoPath, SparseChanges, VersionNumber};
use javelin_collaboration::{PrincipalId, Role};
use javelin_node::{FileWorldLog, bind_loopback, serve};
use javelin_protocol::{
    Message,
    codec::{Kind, LayerOutcome, code, decode, encode},
};
use tempfile::TempDir;

#[path = "wire_support/mod.rs"]
mod wire_support;
use wire_support::{OWNER, device_token, gate};

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

fn start() -> (TempDir, Node) {
    let dir = TempDir::new().unwrap();
    let mut log = FileWorldLog::open(dir.path()).unwrap();
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
    let mut gate = gate(REPO);
    let mut permissions =
        wire_support::permissions(REPO, javelin_collaboration::Visibility::Public);
    permissions.epoch = 2;
    permissions
        .roles
        .insert(PrincipalId::parse("stranger").unwrap(), Role::Maintain);
    gate.apply(permissions).unwrap();
    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let mut log = log;
        let _ = serve(&listener, REPO, &mut log, &gate, None);
    });
    (
        dir,
        Node {
            addr,
            first,
            other,
            edits,
        },
    )
}

/// One message with `token`, and the decoded reply.
fn send(addr: SocketAddr, token: &str, message: &Message) -> Message {
    let body = encode(message).unwrap();
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "POST /v2/r/{REPO} HTTP/1.1\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    decode(&reply[split + 4..]).unwrap()
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
