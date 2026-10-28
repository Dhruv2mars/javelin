//! The node's gate on the wire: every message is decided with the request's
//! `Authorization: Bearer` token, an anonymous caller only reads a public repository (D24),
//! and refusals are `Error` replies. The served gate is the one stored in the node directory.

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    thread,
};

use javelin::{
    FileEntry, LayerId, ObjectId, OperationId, PublishAttribution, RepoPath, SparseChanges,
    VersionNumber,
};
use javelin_collaboration::{PrincipalId, Role, Visibility};
use javelin_identity::{Scope, test_keys};
use javelin_node::{
    FileWorldLog,
    auth::{self, Gate},
    bind_loopback, serve,
};
use javelin_protocol::{
    Message,
    codec::{
        CheckOutcome, CheckReport, Conclusion, Kind, LayerOutcome, PublishOutcome, decode, encode,
    },
    receiver::WorldLog,
};
use tempfile::TempDir;

#[path = "wire_support/mod.rs"]
mod wire_support;
use wire_support::{now, permissions, token, token_for};

const REPO: &str = "repo-1";

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}

/// A node with Layer `work` holding one checkpoint. `reader` has the Read role. Returns the
/// log, the Layer's head and the tree that head composes to.
fn seeded(dir: &TempDir, visibility: Visibility) -> (FileWorldLog, ObjectId, ObjectId) {
    let mut permissions = permissions(REPO, visibility);
    permissions
        .roles
        .insert(PrincipalId::parse("reader").unwrap(), Role::Read);
    auth::write_permissions(dir.path(), &permissions).unwrap();
    auth::write_keys(dir.path(), &test_keys::keys()).unwrap();
    let mut log = FileWorldLog::open(dir.path()).unwrap();
    let layer = LayerId::parse("work").unwrap();
    log.create_layer(&layer, VersionNumber::new(0).unwrap())
        .unwrap();
    let content = log.repository().put_blob(b"one").unwrap();
    let changes = SparseChanges::new([(
        RepoPath::parse("a.txt").unwrap(),
        Some(FileEntry::Regular {
            content,
            executable: false,
        }),
    )])
    .unwrap();
    let head = log
        .append(&op("write-1"), &layer, &changes)
        .unwrap()
        .checkpoint()
        .object();
    let repo = log.repository();
    let manifest = repo
        .checkpoint_manifest(javelin::CheckpointId::parse(&head.to_string()).unwrap())
        .unwrap();
    let tree = repo.put_snapshot(&manifest).unwrap();
    (log, head, tree)
}

/// Serves `connections` connections with the gate stored in `dir`.
fn start(
    dir: &TempDir,
    log: FileWorldLog,
    connections: usize,
) -> (SocketAddr, thread::JoinHandle<()>) {
    let gate = Gate::load(dir.path(), REPO).unwrap();
    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut log = log;
        serve(&listener, REPO, &mut log, &gate, Some(connections)).unwrap();
    });
    (addr, server)
}

/// One message with `headers`, and the status and decoded reply.
fn send(addr: SocketAddr, headers: &str, message: &Message) -> (u16, Option<Message>) {
    let body = encode(message).unwrap();
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "POST /v2/r/{REPO} HTTP/1.1\r\n{headers}Content-Length: {}\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let status = std::str::from_utf8(&reply[..split])
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    (status, decode(&reply[split + 4..]).ok())
}

fn bearer(token: &str) -> String {
    format!("Authorization: Bearer {token}\r\n")
}

fn error_code(reply: Option<Message>) -> String {
    match reply {
        Some(Message::Error { code, .. }) => code,
        other => panic!("expected an error, got {other:?}"),
    }
}

fn publish(head: ObjectId) -> Message {
    Message::PublishLayer {
        operation: op("publish-1"),
        layer: LayerId::parse("work").unwrap(),
        expected: head,
        base: VersionNumber::new(0).unwrap(),
        attribution: PublishAttribution::Human,
    }
}

const RESOLVE: Message = Message::Resolve { version: None };

#[test]
fn a_public_repository_reads_anonymously_and_writes_with_a_token() {
    let dir = TempDir::new().unwrap();
    let (log, head, _) = seeded(&dir, Visibility::Public);
    let (addr, server) = start(&dir, log, 4);

    let (status, read) = send(addr, "", &RESOLVE);
    assert_eq!(status, 200);
    assert!(matches!(read, Some(Message::Resolved { .. })));
    assert_eq!(
        error_code(send(addr, "", &publish(head)).1),
        "unauthorized",
        "an anonymous write"
    );
    // A Read member's direct Publish is decided, not an error: it says to open a request.
    let reader = token_for("reader", REPO);
    assert_eq!(
        send(addr, &bearer(&reader), &publish(head)).1,
        Some(Message::PublishOutcome(PublishOutcome::Refused {
            code: "insufficient_role".into()
        })),
        "the Read role cannot Publish"
    );
    let (_, published) = send(addr, &bearer(&token(REPO)), &publish(head));
    assert!(matches!(
        published,
        Some(Message::PublishOutcome(PublishOutcome::Published { .. }))
    ));
    server.join().unwrap();
}

#[test]
fn a_private_repository_refuses_callers_without_a_valid_token() {
    let dir = TempDir::new().unwrap();
    let (log, _, _) = seeded(&dir, Visibility::Private);
    let (addr, server) = start(&dir, log, 6);

    assert_eq!(error_code(send(addr, "", &RESOLVE).1), "unauthorized");
    let other = token("repo-2");
    assert_eq!(
        error_code(send(addr, &bearer(&other), &RESOLVE).1),
        "forbidden",
        "a token for another repository"
    );
    let t = now();
    let expired = test_keys::issuer()
        .issue(&test_keys::claims(
            "owner",
            REPO,
            &[Scope::Read],
            t - 700,
            t - 100,
            "old",
        ))
        .unwrap();
    assert_eq!(
        error_code(send(addr, &bearer(&expired), &RESOLVE).1),
        "unauthorized"
    );
    let forged = javelin_identity::Issuer::new(test_keys::KID, test_keys::signing_key(9))
        .unwrap()
        .issue(&test_keys::claims(
            "owner",
            REPO,
            &[Scope::Read],
            t,
            t + 600,
            "forged",
        ))
        .unwrap();
    assert_eq!(
        error_code(send(addr, &bearer(&forged), &RESOLVE).1),
        "unauthorized"
    );
    // Review messages pass the same gate.
    let read_request = Message::ReadRequest {
        request: javelin_protocol::codec::RequestId::parse("req-1").unwrap(),
    };
    assert_eq!(error_code(send(addr, "", &read_request).1), "unauthorized");
    let (_, read) = send(addr, &bearer(&token(REPO)), &RESOLVE);
    assert!(matches!(read, Some(Message::Resolved { .. })));
    server.join().unwrap();
}

#[test]
fn a_credential_that_is_not_one_bearer_token_is_a_bad_request() {
    let dir = TempDir::new().unwrap();
    let (log, _, _) = seeded(&dir, Visibility::Public);
    let (addr, server) = start(&dir, log, 3);

    assert_eq!(
        send(addr, "Authorization: Basic b3duZXI6cHc=\r\n", &RESOLVE).0,
        400
    );
    assert_eq!(send(addr, "Authorization: Bearer\r\n", &RESOLVE).0, 400);
    let twice = format!("{}{}", bearer(&token(REPO)), bearer(&token(REPO)));
    assert_eq!(send(addr, &twice, &RESOLVE).0, 400);
    server.join().unwrap();
}

#[test]
fn a_node_without_permissions_refuses_everything() {
    let dir = TempDir::new().unwrap();
    auth::write_keys(dir.path(), &test_keys::keys()).unwrap();
    let log = FileWorldLog::open(dir.path()).unwrap();
    let (addr, server) = start(&dir, log, 2);

    assert_eq!(error_code(send(addr, "", &RESOLVE).1), "unauthorized");
    assert_eq!(
        error_code(send(addr, &bearer(&token(REPO)), &RESOLVE).1),
        "forbidden"
    );
    server.join().unwrap();
}

#[test]
fn a_check_report_goes_over_the_wire_with_the_reporter_token() {
    let dir = TempDir::new().unwrap();
    let (log, _, tree) = seeded(&dir, Visibility::Private);
    let (addr, server) = start(&dir, log, 3);
    let t = now();
    let report = Message::CheckReport(CheckReport {
        operation: op("report-1"),
        repository: REPO.into(),
        subject_tree: tree,
        subject_checkpoint: None,
        name: "ci/unit".into(),
        conclusion: Conclusion::Success,
        details_url: None,
        summary: None,
        started_at: t,
        completed_at: t,
    });
    let mut claims = test_keys::claims("reader", REPO, &[Scope::CheckReport], t, t + 600, "ci");
    claims.checks = vec!["ci/*".into()];
    let reporter = test_keys::issuer().issue(&claims).unwrap();

    assert_eq!(error_code(send(addr, "", &report).1), "unauthorized");
    let (_, first) = send(addr, &bearer(&reporter), &report);
    let Some(Message::CheckOutcome(CheckOutcome::Accepted { seq, .. })) = first else {
        panic!("expected an accepted report, got {first:?}");
    };
    let (_, again) = send(addr, &bearer(&reporter), &report);
    assert!(
        matches!(again, Some(Message::CheckOutcome(CheckOutcome::Accepted { seq: s, .. })) if s == seq),
        "a retry replays the stored result"
    );
    server.join().unwrap();
}

#[test]
fn a_layer_limited_token_uploads_objects_but_writes_only_its_own_layer() {
    let dir = TempDir::new().unwrap();
    let (mut log, head, _) = seeded(&dir, Visibility::Private);
    // A second Layer the agent's token does not name.
    let other = LayerId::parse("other").unwrap();
    log.create_layer(&other, VersionNumber::new(0).unwrap())
        .unwrap();
    let mut permissions = permissions(REPO, Visibility::Private);
    permissions
        .roles
        .insert(PrincipalId::parse("agent").unwrap(), Role::Maintain);
    auth::write_permissions(dir.path(), &permissions).unwrap();
    let (addr, server) = start(&dir, log, 6);
    let t = now();
    let mut claims = test_keys::claims(
        "agent",
        REPO,
        &[Scope::Read, Scope::LayerWrite, Scope::Publish],
        t,
        t + 600,
        "agent-work",
    );
    claims.layer = Some("work".into());
    let agent = bearer(&test_keys::issuer().issue(&claims).unwrap());

    // Content-addressed uploads name no Layer: scope and role decide them.
    let content = b"two".to_vec();
    let blob = Kind::Blob.id(&content);
    let changes = SparseChanges::new([(
        RepoPath::parse("a.txt").unwrap(),
        Some(FileEntry::Regular {
            content: blob,
            executable: false,
        }),
    )])
    .unwrap();
    let bytes = changes.canonical_bytes();
    let checkpoint = Kind::Checkpoint.id(&bytes);
    let (_, missing) = send(addr, &agent, &Message::Want(vec![(Kind::Blob, blob)]));
    assert_eq!(missing, Some(Message::Missing(vec![(Kind::Blob, blob)])));
    let (_, stored) = send(
        addr,
        &agent,
        &Message::Objects(vec![(Kind::Blob, blob, content)]),
    );
    assert_eq!(stored, Some(Message::Stored { count: 1 }));
    let (_, stored) = send(
        addr,
        &agent,
        &Message::Objects(vec![(Kind::Checkpoint, checkpoint, bytes)]),
    );
    assert_eq!(stored, Some(Message::Stored { count: 1 }));

    // Its own Layer takes the append; another Layer refuses it, and refuses its Publish.
    let append = |layer: &str, parent: Option<ObjectId>| Message::LayerAppend {
        operation: op(&format!("append-{layer}")),
        layer: LayerId::parse(layer).unwrap(),
        parent,
        checkpoint,
    };
    let (_, appended) = send(addr, &agent, &append("work", Some(head)));
    assert!(
        matches!(
            appended,
            Some(Message::LayerOutcome(LayerOutcome::Accepted { .. }))
        ),
        "{appended:?}"
    );
    assert_eq!(
        error_code(send(addr, &agent, &append("other", None)).1),
        "forbidden"
    );
    let publish_other = Message::PublishLayer {
        operation: op("publish-other"),
        layer: other,
        expected: head,
        base: VersionNumber::new(0).unwrap(),
        attribution: PublishAttribution::Human,
    };
    assert_eq!(
        error_code(send(addr, &agent, &publish_other).1),
        "forbidden"
    );
    server.join().unwrap();
}
