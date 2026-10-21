use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    thread,
};

use javelin::{LayerId, ObjectId, OperationId, PublishAttribution, VersionNumber};
use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_identity::{Scope, test_keys};
use javelin_node::{FileWorldLog, TraceVisibility, auth::Gate, bind_loopback, serve};
use javelin_protocol::{
    Message,
    codec::{Kind, PublishOutcome, code, decode, encode},
    receiver::WorldLog,
};
use tempfile::TempDir;

const NOW: u64 = 1_000_000;
const REPO: &str = "repo-1";

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn layer(s: &str) -> LayerId {
    LayerId::parse(s).unwrap()
}
fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}

fn permissions(visibility: Visibility) -> AppliedPermissions {
    AppliedPermissions {
        repository: RepositoryId::parse(REPO).unwrap(),
        epoch: 1,
        visibility,
        roles: [
            (PrincipalId::parse("alice").unwrap(), Role::Maintain),
            (PrincipalId::parse("bob").unwrap(), Role::Read),
        ]
        .into_iter()
        .collect(),
        required_approvals: 1,
    }
}

fn gate(visibility: Visibility) -> Gate {
    let mut gate = Gate::new(RepositoryId::parse(REPO).unwrap(), test_keys::keys());
    gate.apply(permissions(visibility)).unwrap();
    gate
}

fn token(sub: &str, scopes: &[Scope]) -> String {
    let claims = test_keys::claims(sub, REPO, scopes, NOW, NOW + 600, &format!("{sub}-tok"));
    test_keys::issuer().issue(&claims).unwrap()
}

/// A node over an empty repository, with one stored trace.
fn node(dir: &TempDir) -> (FileWorldLog, ObjectId) {
    let mut log = FileWorldLog::open(dir.path()).unwrap();
    let trace = log.repository().put_blob(b"agent trace").unwrap();
    (log, trace)
}

fn code_of<T: std::fmt::Debug>(result: Result<T, javelin_protocol::ProtocolError>) -> String {
    result.unwrap_err().code
}

#[test]
fn an_inherited_trace_follows_the_repository_visibility() {
    let dir = TempDir::new().unwrap();
    let (log, trace) = node(&dir);
    assert_eq!(
        log.traces().visibility(trace).unwrap(),
        TraceVisibility::Inherit
    );

    let public = gate(Visibility::Public);
    assert_eq!(
        log.traces()
            .read(&public, log.repo(), None, NOW, trace)
            .unwrap(),
        b"agent trace"
    );

    let private = gate(Visibility::Private);
    assert_eq!(
        code_of(log.traces().read(&private, log.repo(), None, NOW, trace)),
        code::UNAUTHORIZED
    );
    let bob = token("bob", &[Scope::Read]);
    assert_eq!(
        log.traces()
            .read(&private, log.repo(), Some(&bob), NOW, trace)
            .unwrap(),
        b"agent trace"
    );
}

#[test]
fn a_restricted_trace_is_readable_by_maintain_only() {
    let dir = TempDir::new().unwrap();
    let (log, trace) = node(&dir);
    let public = gate(Visibility::Public);
    let alice = token("alice", &[Scope::Read, Scope::Publish]);
    log.traces()
        .restrict(&public, log.repo(), Some(&alice), NOW, trace)
        .unwrap();
    assert_eq!(
        log.traces().visibility(trace).unwrap(),
        TraceVisibility::Restricted
    );

    assert_eq!(
        code_of(log.traces().read(&public, log.repo(), None, NOW, trace)),
        code::FORBIDDEN,
        "anonymous read of a public repository"
    );
    let bob = token("bob", &[Scope::Read]);
    assert_eq!(
        code_of(
            log.traces()
                .read(&public, log.repo(), Some(&bob), NOW, trace)
        ),
        code::FORBIDDEN,
        "Read role"
    );
    let alice_read = token("alice", &[Scope::Read]);
    assert_eq!(
        log.traces()
            .read(&public, log.repo(), Some(&alice_read), NOW, trace)
            .unwrap(),
        b"agent trace",
        "Maintain with a read token"
    );
}

#[test]
fn only_maintain_can_restrict_and_restriction_survives_a_restart() {
    let dir = TempDir::new().unwrap();
    let (log, trace) = node(&dir);
    let private = gate(Visibility::Private);
    let bob = token("bob", &[Scope::Publish]);
    assert_eq!(
        code_of(
            log.traces()
                .restrict(&private, log.repo(), Some(&bob), NOW, trace)
        ),
        code::FORBIDDEN
    );
    assert_eq!(
        log.traces().visibility(trace).unwrap(),
        TraceVisibility::Inherit
    );

    let alice = token("alice", &[Scope::Publish]);
    log.traces()
        .restrict(&private, log.repo(), Some(&alice), NOW, trace)
        .unwrap();
    // Restricting again is harmless and does not loosen anything.
    log.traces()
        .restrict(&private, log.repo(), Some(&alice), NOW, trace)
        .unwrap();
    drop(log);

    let reopened = FileWorldLog::open(dir.path()).unwrap();
    assert_eq!(
        reopened.traces().visibility(trace).unwrap(),
        TraceVisibility::Restricted
    );
}

#[test]
fn a_trace_that_is_not_stored_cannot_be_restricted() {
    let dir = TempDir::new().unwrap();
    let (log, _) = node(&dir);
    let private = gate(Visibility::Private);
    let alice = token("alice", &[Scope::Publish]);
    let absent = Kind::Blob.id(b"never stored");
    assert_eq!(
        code_of(
            log.traces()
                .restrict(&private, log.repo(), Some(&alice), NOW, absent)
        ),
        code::MISSING_OBJECT
    );
}

#[test]
fn the_wire_never_serves_a_restricted_trace() {
    let dir = TempDir::new().unwrap();
    let (mut log, inherited) = node(&dir);
    let restricted = log.repository().put_blob(b"private reasoning").unwrap();
    let public = gate(Visibility::Public);
    let alice = token("alice", &[Scope::Publish]);
    log.traces()
        .restrict(&public, log.repo(), Some(&alice), NOW, restricted)
        .unwrap();

    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut log = log;
        serve(&listener, "demo", &mut log, Some(2)).unwrap();
    });
    let refused = fetch(addr, &[(Kind::Blob, restricted)]);
    let served = fetch(addr, &[(Kind::Blob, inherited)]);
    server.join().unwrap();

    assert!(matches!(
        refused,
        Message::Error { ref code, .. } if code == "forbidden"
    ));
    assert!(matches!(served, Message::Objects(ref o) if o.len() == 1));
}

/// One `Get` on its own connection, over loopback.
fn fetch(addr: SocketAddr, items: &[(Kind, ObjectId)]) -> Message {
    let body = encode(&Message::Get(items.to_vec())).unwrap();
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "POST /v2/r/demo HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    decode(&reply[split + 4..]).unwrap()
}

/// A Layer with one checkpoint over World 0. Returns its head.
fn layer_with_checkpoint(log: &mut FileWorldLog, name: &str) -> ObjectId {
    log.create_layer(&layer(name), v(0)).unwrap();
    let content = log.repository().put_blob(name.as_bytes()).unwrap();
    let changes = javelin::SparseChanges::new([(
        javelin::RepoPath::parse(&format!("{name}.txt")).unwrap(),
        Some(javelin::FileEntry::Regular {
            content,
            executable: false,
        }),
    )])
    .unwrap();
    log.append(&op(&format!("write-{name}")), &layer(name), &changes)
        .unwrap()
        .checkpoint()
        .object()
}

fn publish(
    log: &mut FileWorldLog,
    layer_name: &str,
    operation: &str,
    expected: ObjectId,
    attribution: PublishAttribution,
) -> PublishOutcome {
    log.publish_layer(
        &op(operation),
        &layer(layer_name),
        expected,
        v(0),
        &attribution,
    )
    .unwrap()
}

#[test]
fn an_agent_publish_needs_a_trace_or_the_override() {
    let dir = TempDir::new().unwrap();
    let mut log = FileWorldLog::open(dir.path()).unwrap();
    let trace = log.repository().put_blob(b"agent trace").unwrap();

    let bare = layer_with_checkpoint(&mut log, "bare");
    let refused = publish(
        &mut log,
        "bare",
        "bare-publish",
        bare,
        PublishAttribution::Agent {
            context: None,
            allow_missing_trace: false,
        },
    );
    assert_eq!(
        refused,
        PublishOutcome::Refused {
            code: code::MISSING_AGENT_TRACE.into()
        }
    );
    assert_eq!(
        log.head().unwrap().number,
        v(0),
        "a refused Publish writes nothing"
    );

    let overridden = publish(
        &mut log,
        "bare",
        "bare-override",
        bare,
        PublishAttribution::Agent {
            context: None,
            allow_missing_trace: true,
        },
    );
    assert!(matches!(
        overridden,
        PublishOutcome::Published { number, .. } if number == v(1)
    ));

    let traced = layer_with_checkpoint(&mut log, "traced");
    let with_trace = publish(
        &mut log,
        "traced",
        "traced-publish",
        traced,
        PublishAttribution::Agent {
            context: Some(trace),
            allow_missing_trace: false,
        },
    );
    assert!(matches!(
        with_trace,
        PublishOutcome::Published { number, .. } if number == v(2)
    ));
}
