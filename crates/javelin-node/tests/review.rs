use javelin::{
    CheckpointId, FileEntry, LayerId, ObjectId, OperationId, PublishAttribution, RepoPath,
    SparseChanges, VersionNumber,
};
use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_identity::{Scope, test_keys};
use javelin_node::{FileWorldLog, auth::Gate, review::handle};
use javelin_protocol::{
    Message, ProtocolError,
    codec::{PublishOutcome, RequestId, RequestStatus, ReviewOutcome},
    receiver::WorldLog,
};
use tempfile::TempDir;

const NOW: u64 = 1_000_000;
const REPO: &str = "repo-1";

fn op(value: &str) -> OperationId {
    OperationId::parse(value).unwrap()
}

fn layer() -> LayerId {
    LayerId::parse("work").unwrap()
}

fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}

fn request() -> RequestId {
    RequestId::parse("pr-1").unwrap()
}

/// alice authors and can approve, rita reviews, mona maintains. `rita` is `None` once
/// her role is removed.
fn permissions(epoch: u64, rita: Option<Role>) -> AppliedPermissions {
    let mut roles = vec![
        (PrincipalId::parse("alice").unwrap(), Role::Review),
        (PrincipalId::parse("mona").unwrap(), Role::Maintain),
    ];
    if let Some(role) = rita {
        roles.push((PrincipalId::parse("rita").unwrap(), role));
    }
    AppliedPermissions {
        repository: RepositoryId::parse(REPO).unwrap(),
        epoch,
        visibility: Visibility::Private,
        roles: roles.into_iter().collect(),
        required_approvals: 1,
    }
}

fn token(sub: &str, scopes: &[Scope]) -> String {
    let claims = test_keys::claims(sub, REPO, scopes, NOW, NOW + 600, sub);
    test_keys::issuer().issue(&claims).unwrap()
}

fn alice() -> String {
    token(
        "alice",
        &[Scope::Read, Scope::ReviewWrite, Scope::ReviewApprove],
    )
}

fn rita() -> String {
    token(
        "rita",
        &[Scope::Read, Scope::ReviewWrite, Scope::ReviewApprove],
    )
}

fn mona() -> String {
    token(
        "mona",
        &[
            Scope::Read,
            Scope::ReviewWrite,
            Scope::ReviewApprove,
            Scope::Publish,
        ],
    )
}

fn gate(rita_role: Option<Role>) -> Gate {
    let mut gate = Gate::new(RepositoryId::parse(REPO).unwrap(), test_keys::keys());
    gate.apply(permissions(1, rita_role)).unwrap();
    gate
}

/// A node with Layer `work` over World 0.
fn node(dir: &TempDir, rita_role: Option<Role>) -> (FileWorldLog, Gate) {
    let mut log = FileWorldLog::open(dir.path()).unwrap();
    log.create_layer(&layer(), v(0)).unwrap();
    (log, gate(rita_role))
}

/// Writes one file to `work` and returns the Layer's new checkpoint.
fn write(log: &mut FileWorldLog, operation: &str, path: &str, bytes: &[u8]) -> ObjectId {
    let content = log.repository().put_blob(bytes).unwrap();
    let changes = SparseChanges::new([(
        RepoPath::parse(path).unwrap(),
        Some(FileEntry::Regular {
            content,
            executable: false,
        }),
    )])
    .unwrap();
    log.append(&op(operation), &layer(), &changes)
        .unwrap()
        .checkpoint()
        .object()
}

/// The tree a checkpoint composes to: what `PublishRequest` must publish for it.
fn tree_of(log: &mut FileWorldLog, checkpoint: ObjectId) -> ObjectId {
    let repo = log.repository();
    let manifest = repo
        .checkpoint_manifest(CheckpointId::parse(&checkpoint.to_string()).unwrap())
        .unwrap();
    repo.put_snapshot(&manifest).unwrap()
}

fn send(
    log: &mut FileWorldLog,
    gate: &Gate,
    token: &str,
    message: Message,
) -> Result<Message, ProtocolError> {
    handle(log, gate, Some(token), NOW, &message)
}

fn open(operation: &str, checkpoint: ObjectId) -> Message {
    Message::OpenRequest {
        operation: op(operation),
        request: request(),
        layer: layer(),
        checkpoint,
        base: v(0),
    }
}

fn revise(operation: &str, expected: u32, checkpoint: ObjectId) -> Message {
    Message::Revise {
        operation: op(operation),
        request: request(),
        expected_revision: expected,
        checkpoint,
        base: v(0),
    }
}

fn approve(operation: &str, revision: u32) -> Message {
    Message::Approve {
        operation: op(operation),
        request: request(),
        revision,
    }
}

fn publish(operation: &str, revision: u32) -> Message {
    Message::PublishRequest {
        operation: op(operation),
        request: request(),
        revision,
        base: v(0),
    }
}

fn read() -> Message {
    Message::ReadRequest { request: request() }
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

fn published(reply: Message) -> (VersionNumber, ObjectId) {
    match reply {
        Message::PublishOutcome(PublishOutcome::Published { number, tree, .. }) => (number, tree),
        other => panic!("expected published, got {other:?}"),
    }
}

#[test]
fn publish_request_publishes_the_approved_checkpoint_not_the_layer_head() {
    let dir = TempDir::new().unwrap();
    let (mut log, gate) = node(&dir, Some(Role::Review));
    let first = write(&mut log, "write-1", "a.txt", b"one");
    let expected_tree = tree_of(&mut log, first);
    assert_eq!(
        accepted(send(&mut log, &gate, &alice(), open("open-1", first)).unwrap()),
        1
    );
    assert_eq!(
        accepted(send(&mut log, &gate, &rita(), approve("approve-1", 1)).unwrap()),
        1
    );
    // The Layer moves on without a new revision. Revision 1 still publishes its own tree.
    write(&mut log, "write-2", "b.txt", b"two");
    let (number, tree) =
        published(send(&mut log, &gate, &mona(), publish("publish-1", 1)).unwrap());
    assert_eq!(number, v(1));
    assert_eq!(tree, expected_tree);
}

#[test]
fn only_the_author_revises() {
    let dir = TempDir::new().unwrap();
    let (mut log, gate) = node(&dir, Some(Role::Review));
    let first = write(&mut log, "write-1", "a.txt", b"one");
    send(&mut log, &gate, &alice(), open("open-1", first)).unwrap();
    let second = write(&mut log, "write-2", "b.txt", b"two");
    let reply = send(&mut log, &gate, &rita(), revise("revise-1", 1, second)).unwrap();
    assert_eq!(refused(reply), "not_author");
}

#[test]
fn a_revision_number_is_immutable() {
    let dir = TempDir::new().unwrap();
    let (mut log, gate) = node(&dir, Some(Role::Review));
    let first = write(&mut log, "write-1", "a.txt", b"one");
    send(&mut log, &gate, &alice(), open("open-1", first)).unwrap();
    let second = write(&mut log, "write-2", "b.txt", b"two");
    // Resubmitting revision 1 over the observed revision 0 is refused, and nothing moves.
    let reply = send(&mut log, &gate, &alice(), revise("revise-1", 0, second)).unwrap();
    assert_eq!(refused(reply), "stale_revision");
    let state = send(&mut log, &gate, &alice(), read()).unwrap();
    assert!(matches!(state, Message::RequestState { revision: 1, .. }));
}

#[test]
fn approval_for_revision_one_does_not_count_for_revision_two() {
    let dir = TempDir::new().unwrap();
    let (mut log, gate) = node(&dir, Some(Role::Review));
    let first = write(&mut log, "write-1", "a.txt", b"one");
    send(&mut log, &gate, &alice(), open("open-1", first)).unwrap();
    send(&mut log, &gate, &rita(), approve("approve-1", 1)).unwrap();
    let second = write(&mut log, "write-2", "b.txt", b"two");
    assert_eq!(
        accepted(send(&mut log, &gate, &alice(), revise("revise-1", 1, second)).unwrap()),
        2
    );
    let reply = send(&mut log, &gate, &mona(), publish("publish-1", 2)).unwrap();
    assert_eq!(refused(reply), "not_enough_approvals");
}

#[test]
fn self_approval_is_refused() {
    let dir = TempDir::new().unwrap();
    let (mut log, gate) = node(&dir, Some(Role::Review));
    let first = write(&mut log, "write-1", "a.txt", b"one");
    send(&mut log, &gate, &alice(), open("open-1", first)).unwrap();
    let reply = send(&mut log, &gate, &alice(), approve("approve-1", 1)).unwrap();
    assert_eq!(refused(reply), "self_approval");
}

#[test]
fn publish_of_a_superseded_revision_is_refused() {
    let dir = TempDir::new().unwrap();
    let (mut log, gate) = node(&dir, Some(Role::Review));
    let first = write(&mut log, "write-1", "a.txt", b"one");
    send(&mut log, &gate, &alice(), open("open-1", first)).unwrap();
    send(&mut log, &gate, &rita(), approve("approve-1", 1)).unwrap();
    let second = write(&mut log, "write-2", "b.txt", b"two");
    send(&mut log, &gate, &alice(), revise("revise-1", 1, second)).unwrap();
    let reply = send(&mut log, &gate, &mona(), publish("publish-1", 1)).unwrap();
    assert_eq!(refused(reply), "stale_revision");
}

#[test]
fn a_lost_reply_publishes_once() {
    let dir = TempDir::new().unwrap();
    let (mut log, gate) = node(&dir, Some(Role::Review));
    let first = write(&mut log, "write-1", "a.txt", b"one");
    send(&mut log, &gate, &alice(), open("open-1", first)).unwrap();
    send(&mut log, &gate, &rita(), approve("approve-1", 1)).unwrap();
    let first_reply = send(&mut log, &gate, &mona(), publish("publish-1", 1)).unwrap();
    // The client retries the same operation after losing the reply.
    let retry = send(&mut log, &gate, &mona(), publish("publish-1", 1)).unwrap();
    assert_eq!(retry, first_reply);
    // A different operation cannot publish the same request again.
    let reply = send(&mut log, &gate, &mona(), publish("publish-2", 1)).unwrap();
    assert_eq!(refused(reply), "not_open");
    assert_eq!(published(first_reply).0, v(1));
}

#[test]
fn an_approver_whose_role_is_removed_before_publish_blocks_it() {
    let dir = TempDir::new().unwrap();
    let (mut log, mut gate) = node(&dir, Some(Role::Review));
    let first = write(&mut log, "write-1", "a.txt", b"one");
    send(&mut log, &gate, &alice(), open("open-1", first)).unwrap();
    send(&mut log, &gate, &rita(), approve("approve-1", 1)).unwrap();
    gate.apply(permissions(2, None)).unwrap();
    let reply = send(&mut log, &gate, &mona(), publish("publish-1", 1)).unwrap();
    assert_eq!(refused(reply), "not_enough_approvals");
}

#[test]
fn a_restart_rebuilds_the_request_from_its_chain() {
    let dir = TempDir::new().unwrap();
    {
        let (mut log, gate) = node(&dir, Some(Role::Review));
        let first = write(&mut log, "write-1", "a.txt", b"one");
        send(&mut log, &gate, &alice(), open("open-1", first)).unwrap();
        send(&mut log, &gate, &rita(), approve("approve-1", 1)).unwrap();
    }
    // The node restarts, rebuilds the request from its chain, and still publishes it.
    let mut log = FileWorldLog::open(dir.path()).unwrap();
    let gate = gate(Some(Role::Review));
    assert_eq!(
        send(&mut log, &gate, &alice(), read()).unwrap(),
        Message::RequestState {
            request: request(),
            state: javelin_protocol::codec::RequestStatus::Open,
            revision: 1,
        }
    );
    let (number, _) = published(send(&mut log, &gate, &mona(), publish("publish-1", 1)).unwrap());
    assert_eq!(number, v(1));
}

#[test]
fn an_unknown_request_is_a_protocol_error() {
    let dir = TempDir::new().unwrap();
    let (mut log, gate) = node(&dir, Some(Role::Review));
    let error = send(&mut log, &gate, &alice(), read()).unwrap_err();
    assert_eq!(error.code, "unknown_request");
}

/// The Layer that publishes revision `index` of `pr-1`, named as `review.rs` names it.
fn review_layer(index: u32) -> LayerId {
    LayerId::parse(&format!(
        "review-{}",
        ObjectId::for_blob(format!("pr-1/{index}").as_bytes())
    ))
    .unwrap()
}

fn state_of(reply: Message) -> (RequestStatus, u32) {
    match reply {
        Message::RequestState {
            state, revision, ..
        } => (state, revision),
        other => panic!("expected request state, got {other:?}"),
    }
}

#[test]
fn publish_refuses_a_revision_layer_at_another_checkpoint() {
    let dir = TempDir::new().unwrap();
    let (mut log, gate) = node(&dir, Some(Role::Review));
    let first = write(&mut log, "write-1", "a.txt", b"one");
    // The revision's Layer exists at World 0, not at the approved checkpoint.
    log.create_layer(&review_layer(1), v(0)).unwrap();
    assert_eq!(
        accepted(send(&mut log, &gate, &alice(), open("open-1", first)).unwrap()),
        1
    );
    assert_eq!(
        accepted(send(&mut log, &gate, &rita(), approve("approve-1", 1)).unwrap()),
        1
    );
    assert_eq!(
        refused(send(&mut log, &gate, &mona(), publish("publish-1", 1)).unwrap()),
        "layer_stale"
    );
}

#[test]
fn a_publish_whose_record_was_lost_is_completed_only_by_its_own_operation() {
    let dir = TempDir::new().unwrap();
    let (mut log, gate) = node(&dir, Some(Role::Review));
    let first = write(&mut log, "write-1", "a.txt", b"one");
    accepted(send(&mut log, &gate, &alice(), open("open-1", first)).unwrap());
    accepted(send(&mut log, &gate, &rita(), approve("approve-1", 1)).unwrap());
    let (number, tree) =
        published(send(&mut log, &gate, &mona(), publish("publish-1", 1)).unwrap());

    // A crash: the World published revision 1, but publish-1's record is lost.
    drop(log);
    let records = rusqlite::Connection::open(dir.path().join("node.db")).unwrap();
    records
        .execute("DELETE FROM review_rows WHERE operation = 'publish-1'", [])
        .unwrap();
    drop(records);
    let (mut log, gate) = node(&dir, Some(Role::Review));

    // The request reads as Published, and no later action is accepted.
    assert_eq!(
        state_of(send(&mut log, &gate, &mona(), read()).unwrap()),
        (RequestStatus::Published, 1)
    );
    let second = write(&mut log, "write-2", "b.txt", b"two");
    assert_eq!(
        refused(send(&mut log, &gate, &alice(), revise("revise-2", 1, second)).unwrap()),
        "not_open"
    );
    assert_eq!(
        refused(send(&mut log, &gate, &rita(), approve("approve-2", 1)).unwrap()),
        "not_open"
    );
    // Another operation cannot take over the recorded publication.
    assert_eq!(
        refused(send(&mut log, &gate, &mona(), publish("publish-2", 1)).unwrap()),
        "layer_published"
    );
    // Its own retry gets the same version back.
    assert_eq!(
        published(send(&mut log, &gate, &mona(), publish("publish-1", 1)).unwrap()),
        (number, tree)
    );
}

#[test]
fn an_ordinary_publish_layer_cannot_publish_a_revision_layer() {
    let dir = TempDir::new().unwrap();
    let (mut log, _gate) = node(&dir, Some(Role::Review));
    let head = log.create_layer(&review_layer(1), v(0)).unwrap();

    // The revision was never approved, so a plain PublishLayer on its Layer is refused.
    let reply = log
        .publish_layer(
            &op("ordinary-1"),
            &review_layer(1),
            head.checkpoint().object(),
            v(0),
            &PublishAttribution::Human,
        )
        .unwrap();
    assert_eq!(refused(Message::PublishOutcome(reply)), "layer_reserved");
}
