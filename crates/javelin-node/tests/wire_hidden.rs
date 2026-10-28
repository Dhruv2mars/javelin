//! Hidden entries on the node's wire, with real tokens: each read follows the caller's role
//! (anonymous is an outsider), and a write that touches a path outside the caller's
//! audiences is refused with `forbidden`, naming no path.

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
};

use javelin::{
    FileEntry, LayerId, LayerState, ObjectId, OperationId, POLICY_PATH, PublishAttribution, Reader,
    RepoPath, SparseChanges, VersionNumber,
};
use javelin_collaboration::{PrincipalId, Role, Visibility};
use javelin_identity::test_keys;
use javelin_node::{
    FileWorldLog,
    auth::{self, Gate},
    bind_loopback,
};
use javelin_protocol::{
    Message,
    codec::{Kind, LayerOutcome, PublishOutcome, RequestId, decode, encode},
    receiver::WorldLog,
};
use tempfile::TempDir;

#[path = "wire_support/mod.rs"]
mod wire_support;
use wire_support::{permissions, token, token_for};

const REPO: &str = "repo-1";
const POLICY: &[u8] = b".env maintainers\ninternal/** members\n";
const SECRET: &[u8] = b"TOKEN=1\n";
const NOTES: &[u8] = b"notes\n";

fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}
fn path(s: &str) -> RepoPath {
    RepoPath::parse(s).unwrap()
}
fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn layer(s: &str) -> LayerId {
    LayerId::parse(s).unwrap()
}

fn changes(log: &mut FileWorldLog, files: &[(&str, Option<&[u8]>)]) -> SparseChanges {
    SparseChanges::new(files.iter().map(|(p, bytes)| {
        let entry = bytes.map(|bytes| FileEntry::Regular {
            content: log.repository().put_blob(bytes).unwrap(),
            executable: false,
        });
        (path(p), entry)
    }))
    .unwrap()
}

/// Creates Layer `name` on the head, appends `files` and returns its checkpoint.
fn append(log: &mut FileWorldLog, name: &str, files: &[(&str, Option<&[u8]>)]) -> ObjectId {
    let base = log.head().unwrap().number;
    log.create_layer(&layer(name), base).unwrap();
    let changes = changes(log, files);
    log.append(&op(&format!("write-{name}")), &layer(name), &changes)
        .unwrap()
        .checkpoint()
        .object()
}

/// Appends and publishes `files` as Layer `name`; returns its checkpoint.
fn publish(log: &mut FileWorldLog, name: &str, files: &[(&str, Option<&[u8]>)]) -> ObjectId {
    let base = log.head().unwrap().number;
    let checkpoint = append(log, name, files);
    let outcome = log
        .publish_layer(
            &op(&format!("publish-{name}")),
            &layer(name),
            checkpoint,
            base,
            &PublishAttribution::Human,
        )
        .unwrap();
    assert!(
        matches!(outcome, PublishOutcome::Published { .. }),
        "{outcome:?}"
    );
    checkpoint
}

/// A public repository: `owner` holds Maintain and `member` holds Read. World 1 sets the
/// policy and holds a maintainers' `.env`, a members' `internal/notes` and public `src/a`.
fn node(dir: &TempDir) -> FileWorldLog {
    let mut permissions = permissions(REPO, Visibility::Public);
    permissions
        .roles
        .insert(PrincipalId::parse("member").unwrap(), Role::Read);
    auth::write_permissions(dir.path(), &permissions).unwrap();
    auth::write_keys(dir.path(), &test_keys::keys()).unwrap();
    let mut log = FileWorldLog::open(dir.path()).unwrap();
    publish(
        &mut log,
        "seed",
        &[
            (POLICY_PATH, Some(POLICY)),
            (".env", Some(SECRET)),
            ("internal/notes", Some(NOTES)),
            ("src/a", Some(b"a\n")),
        ],
    );
    log
}

fn start(dir: &TempDir, log: FileWorldLog) -> (SocketAddr, wire_support::Served) {
    let gate = Gate::load(dir.path(), REPO).unwrap();
    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let served = wire_support::serve_until_joined(listener, REPO, log, gate);
    (addr, served)
}

/// One message with an optional bearer token; returns the decoded reply.
fn send(addr: SocketAddr, token: Option<&str>, message: &Message) -> Message {
    let body = encode(message).unwrap();
    let mut stream = TcpStream::connect(addr).unwrap();
    let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    write!(
        stream,
        "POST /v2/r/{REPO} HTTP/1.1\r\n{auth}Content-Length: {}\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    decode(&reply[split + 4..]).unwrap()
}

fn error(reply: Message) -> (String, String) {
    match reply {
        Message::Error { code, detail } => (code, detail),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn commitment(log: &FileWorldLog, p: &str) -> ObjectId {
    match &log
        .repo()
        .committed_at(log.head().unwrap().number)
        .unwrap()
        .entries()[&path(p)]
    {
        FileEntry::Hidden { commitment, .. } => *commitment,
        other => panic!("{p} is not hidden: {other:?}"),
    }
}

#[test]
fn reads_follow_the_callers_role() {
    let dir = TempDir::new().unwrap();
    let mut log = node(&dir);
    let (env, internal) = (commitment(&log, ".env"), commitment(&log, "internal"));
    let (secret, notes) = (ObjectId::for_blob(SECRET), ObjectId::for_blob(NOTES));
    // A maintainer's unpublished edit of `.env`, and a Layer that changes public paths only.
    let hidden_edit = append(&mut log, "secret", &[(".env", Some(b"TOKEN=2\n"))]);
    let public_edit = append(&mut log, "public", &[("src/a", Some(b"b\n"))]);
    let (owner, member) = (token(REPO), token_for("member", REPO));
    let stranger = token_for("stranger", REPO);
    let (addr, server) = start(&dir, log);
    let open = Message::Open(vec![env, internal]);
    let opened = |reply: Message| match reply {
        Message::Opened(openings) => openings.into_iter().map(|(c, _)| c).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    let served = |reply: Message| matches!(reply, Message::Objects(objects) if objects.len() == 1);
    let get = |kind: Kind, id: ObjectId| Message::Get(vec![(kind, id)]);

    // An anonymous caller is an outsider: commitments only.
    assert!(opened(send(addr, None, &open)).is_empty());
    assert_eq!(
        error(send(addr, None, &get(Kind::Blob, notes))).0,
        "missing_object"
    );
    // A Layer is work above the public frontier: an outsider reads no checkpoint, and is
    // refused before anything is looked up, so the refusal says nothing about either.
    for checkpoint in [
        hidden_edit,
        public_edit,
        ObjectId::for_checkpoint(b"{}never"),
    ] {
        assert_eq!(
            error(send(addr, None, &get(Kind::Checkpoint, checkpoint))).0,
            "forbidden"
        );
    }
    // A public repository lets any signed-in caller read, but without a granted role the
    // caller is still an outsider.
    assert!(opened(send(addr, Some(&stranger), &open)).is_empty());
    assert_eq!(
        error(send(addr, Some(&stranger), &get(Kind::Blob, notes))).0,
        "missing_object"
    );

    // A member reads the members' content, not the maintainers'.
    assert_eq!(opened(send(addr, Some(&member), &open)), vec![internal]);
    assert!(served(send(addr, Some(&member), &get(Kind::Blob, notes))));
    assert_eq!(
        error(send(addr, Some(&member), &get(Kind::Blob, secret))).0,
        "missing_object"
    );
    assert_eq!(
        send(
            addr,
            Some(&member),
            &Message::Want(vec![(Kind::Blob, secret), (Kind::Blob, notes)])
        ),
        Message::Missing(vec![(Kind::Blob, secret)])
    );
    assert_eq!(
        error(send(
            addr,
            Some(&member),
            &get(Kind::Checkpoint, hidden_edit)
        ))
        .0,
        "missing_object"
    );

    // A maintainer reads everything.
    assert_eq!(opened(send(addr, Some(&owner), &open)), vec![env, internal]);
    assert!(served(send(addr, Some(&owner), &get(Kind::Blob, secret))));
    assert!(served(send(
        addr,
        Some(&owner),
        &get(Kind::Checkpoint, hidden_edit)
    )));
    // A refused hidden object looks exactly like one that was never stored.
    let never = ObjectId::for_blob(b"never stored");
    let (code, detail) = error(send(addr, Some(&member), &get(Kind::Blob, never)));
    assert_eq!(
        error(send(addr, Some(&member), &get(Kind::Blob, secret))),
        (
            code,
            detail.replace(&never.to_string(), &secret.to_string())
        )
    );
    server.join().unwrap();
}

#[test]
fn an_edit_made_while_hidden_stays_hidden_after_the_rule_is_removed() {
    let dir = TempDir::new().unwrap();
    let mut log = node(&dir);
    let secret = publish(&mut log, "secret", &[(".env", Some(b"TOKEN=old\n"))]);
    publish(
        &mut log,
        "disclose",
        &[(POLICY_PATH, Some(b"")), (".env", Some(b"TOKEN=public\n"))],
    );
    let public = publish(&mut log, "public", &[(".env", Some(b"TOKEN=new\n"))]);
    // Changes staged while the rule held, never appended.
    let staged = SparseChanges::new([(
        path(".env"),
        Some(FileEntry::Regular {
            content: ObjectId::for_blob(b"TOKEN=staged\n"),
            executable: false,
        }),
    )])
    .unwrap()
    .canonical_bytes();
    let staged_id = ObjectId::for_checkpoint(&staged);
    log.store_changes(staged_id, &staged).unwrap();
    // The head policy alone would now serve the old edit.
    assert!(
        log.repo()
            .policy(log.head().unwrap().number)
            .unwrap()
            .is_empty()
    );
    let member = token_for("member", REPO);
    let (addr, server) = start(&dir, log);
    let never = ObjectId::for_checkpoint(b"{}never");
    let (code, detail) = error(send(
        addr,
        Some(&member),
        &Message::Get(vec![(Kind::Checkpoint, never)]),
    ));
    assert_eq!(
        error(send(
            addr,
            Some(&member),
            &Message::Get(vec![(Kind::Checkpoint, secret)])
        )),
        (
            code,
            detail.replace(&never.to_string(), &secret.to_string())
        )
    );
    assert!(matches!(
        send(
            addr,
            Some(&member),
            &Message::Get(vec![(Kind::Checkpoint, public)])
        ),
        Message::Objects(objects) if objects.len() == 1
    ));
    assert_eq!(
        send(
            addr,
            Some(&member),
            &Message::Want(vec![(Kind::Checkpoint, staged_id)])
        ),
        Message::Missing(vec![(Kind::Checkpoint, staged_id)])
    );
    server.join().unwrap();
}

#[test]
fn writes_outside_the_callers_audiences_are_forbidden() {
    let dir = TempDir::new().unwrap();
    let mut log = node(&dir);
    // The member's own Layer, and a maintainer's Layer that edits `.env`.
    let mine = append(&mut log, "mine", &[]);
    let secret = append(&mut log, "secret", &[(".env", Some(b"TOKEN=2\n"))]);
    let policy_edit = append(
        &mut log,
        "policy-edit",
        &[(POLICY_PATH, Some(b"src/** members\n"))],
    );
    let maintainer_layer = append(&mut log, "owner", &[]);
    let (owner, member) = (token(REPO), token_for("member", REPO));
    let (addr, server) = start(&dir, log);
    // Each writer uploads its own blobs and changes, as a client does.
    let upload = |token: &str, files: &[(&str, &[u8])]| {
        let blobs = files
            .iter()
            .map(|(_, bytes)| (Kind::Blob, ObjectId::for_blob(bytes), bytes.to_vec()))
            .collect();
        assert!(matches!(
            send(addr, Some(token), &Message::Objects(blobs)),
            Message::Stored { .. }
        ));
        let bytes = SparseChanges::new(files.iter().map(|(p, bytes)| {
            let entry = FileEntry::Regular {
                content: ObjectId::for_blob(bytes),
                executable: false,
            };
            (path(p), Some(entry))
        }))
        .unwrap()
        .canonical_bytes();
        let id = ObjectId::for_checkpoint(&bytes);
        let reply = send(
            addr,
            Some(token),
            &Message::Objects(vec![(Kind::Checkpoint, id, bytes)]),
        );
        assert!(matches!(reply, Message::Stored { .. }), "{reply:?}");
        id
    };
    let to_env = upload(&member, &[(".env", b"TOKEN=member\n")]);
    let to_notes = upload(&member, &[("internal/notes", b"member notes\n")]);
    let to_policy = upload(&member, &[(POLICY_PATH, b"")]);
    let to_env_again = upload(&owner, &[(".env", b"TOKEN=owner\n")]);
    let append_msg =
        |name: &str, parent: ObjectId, changes: ObjectId, n: &str| Message::LayerAppend {
            operation: op(n),
            layer: layer(name),
            parent: Some(parent),
            checkpoint: changes,
        };
    let forbidden = |reply: Message| {
        let (code, detail) = error(reply);
        assert_eq!(code, "forbidden");
        assert!(
            !detail.contains(".env") && !detail.contains(POLICY_PATH),
            "{detail}"
        );
    };

    // A member may write the members' paths, not the maintainers' or the policy.
    forbidden(send(
        addr,
        Some(&member),
        &append_msg("mine", mine, to_env, "m-env"),
    ));
    forbidden(send(
        addr,
        Some(&member),
        &append_msg("mine", mine, to_policy, "m-policy"),
    ));
    let Message::LayerOutcome(LayerOutcome::Accepted { checkpoint, .. }) = send(
        addr,
        Some(&member),
        &append_msg("mine", mine, to_notes, "m-notes"),
    ) else {
        panic!("the member's write to its own audience was refused")
    };
    // A maintainer's hidden edit is absent to the member: stacking it or proposing it
    // for review answers exactly as for a checkpoint that was never stored.
    let never = ObjectId::for_checkpoint(b"{}never");
    let stack = |source: ObjectId, n: &str| Message::LayerStack {
        operation: op(n),
        target: layer("mine"),
        target_head: checkpoint,
        source,
    };
    assert_eq!(
        send(addr, Some(&member), &stack(secret, "m-stack")),
        send(addr, Some(&member), &stack(never, "m-stack-never"))
    );
    let review = |checkpoint: ObjectId, n: &str| Message::OpenRequest {
        operation: op(n),
        request: RequestId::parse(n).unwrap(),
        layer: layer("secret"),
        checkpoint,
        base: v(1),
    };
    assert_eq!(
        send(addr, Some(&member), &review(secret, "m-review")),
        send(addr, Some(&member), &review(never, "m-review-never"))
    );
    // A visible change the member may still not make, to the policy, is forbidden.
    forbidden(send(
        addr,
        Some(&member),
        &stack(policy_edit, "m-stack-policy"),
    ));

    // A maintainer writes and publishes the hidden path.
    let Message::LayerOutcome(LayerOutcome::Accepted { checkpoint, .. }) = send(
        addr,
        Some(&owner),
        &append_msg("owner", maintainer_layer, to_env_again, "o-env"),
    ) else {
        panic!("the maintainer's write was refused")
    };
    let reply = send(
        addr,
        Some(&owner),
        &Message::PublishLayer {
            operation: op("o-publish"),
            layer: layer("owner"),
            expected: checkpoint,
            base: v(1),
            attribution: PublishAttribution::Human,
        },
    );
    assert!(
        matches!(reply, Message::PublishOutcome(PublishOutcome::Published { number, .. }) if number == v(2)),
        "{reply:?}"
    );
    let log = server.join().unwrap();
    let view = log.repo().manifest_at(v(2)).unwrap();
    assert_eq!(
        view.entries()[&path(".env")].content(),
        Some(ObjectId::for_blob(b"TOKEN=owner\n"))
    );
    log.repo().check().unwrap();
}

#[test]
fn a_conflict_with_a_hidden_change_names_no_path_to_a_reader_outside_it() {
    let dir = TempDir::new().unwrap();
    let mut log = node(&dir);
    // A Layer on World 1 edits `.env`; meanwhile World 2 edits it too.
    let late = append(&mut log, "late", &[(".env", Some(b"late\n"))]);
    publish(&mut log, "first", &[(".env", Some(b"two\n"))]);
    let message = Message::PublishLayer {
        operation: op("publish-late"),
        layer: layer("late"),
        expected: late,
        base: v(1),
        attribution: PublishAttribution::Human,
    };
    let reply = javelin_protocol::handle_request_as(&mut log, message.clone(), &Reader::member([]))
        .unwrap();
    assert_eq!(
        reply,
        Message::PublishOutcome(PublishOutcome::Conflict { paths: Vec::new() })
    );
    let reply =
        javelin_protocol::handle_request_as(&mut log, message, &Reader::maintainer()).unwrap();
    assert_eq!(
        reply,
        Message::PublishOutcome(PublishOutcome::Conflict {
            paths: vec![path(".env")]
        })
    );
}

/// One tree chunk with a single file `f` naming `blob`.
fn tree_naming(blob: ObjectId) -> (ObjectId, Vec<u8>) {
    use javelin::tree::{MemoryStore, Store, Trees};
    let trees = Trees::new(MemoryStore::default());
    let entry = FileEntry::Regular {
        content: blob,
        executable: false,
    };
    let root = trees.build([(&path("f"), &entry)]).unwrap();
    (root, trees.store().load(root).unwrap())
}

#[test]
fn an_upload_never_confirms_a_hidden_object_but_its_uploader_reads_it_back() {
    let dir = TempDir::new().unwrap();
    let mut log = node(&dir);
    let mine_layer = append(&mut log, "mine", &[]);
    let (member, stranger) = (token_for("member", REPO), token_for("stranger", REPO));
    let (addr, server) = start(&dir, log);
    let upload = |token: &str, kind: Kind, id: ObjectId, bytes: Vec<u8>| {
        send(
            addr,
            Some(token),
            &Message::Objects(vec![(kind, id, bytes)]),
        )
    };

    // Greptile's sequence: a member uploads a tree chunk naming a guessed blob ID. The
    // published maintainers-only `.env` and a blob that was never stored get the same reply.
    let secret = ObjectId::for_blob(SECRET);
    let never = ObjectId::for_blob(b"TOKEN=guess\n");
    let (guessed, bytes) = tree_naming(secret);
    let (code, detail) = error(upload(&member, Kind::Tree, guessed, bytes));
    let (absent, bytes) = tree_naming(never);
    let reference = error(upload(&member, Kind::Tree, absent, bytes));
    assert_eq!(
        (
            code,
            detail.replace(&secret.to_string(), &never.to_string())
        ),
        reference
    );
    assert_eq!(reference.0, "missing_object");

    // The same for an append whose changes name the guessed blob at a public path.
    let append_naming = |blob: ObjectId, n: &str| {
        let bytes = SparseChanges::new([(
            path("src/copy"),
            Some(FileEntry::Regular {
                content: blob,
                executable: false,
            }),
        )])
        .unwrap()
        .canonical_bytes();
        let changes = ObjectId::for_checkpoint(&bytes);
        assert!(matches!(
            upload(&member, Kind::Checkpoint, changes, bytes),
            Message::Stored { .. }
        ));
        send(
            addr,
            Some(&member),
            &Message::LayerAppend {
                operation: op(n),
                layer: layer("mine"),
                parent: Some(mine_layer),
                checkpoint: changes,
            },
        )
    };
    let reply = append_naming(secret, "guess-secret");
    assert_eq!(reply, append_naming(never, "guess-never"));
    assert_eq!(
        reply,
        Message::LayerOutcome(LayerOutcome::Refused {
            code: "missing_object".into()
        })
    );

    // The member uploads a blob of its own: it may confirm and read that blob, and name
    // it in a tree, before any version publishes it.
    let mine = b"member draft\n".to_vec();
    let id = ObjectId::for_blob(&mine);
    assert!(matches!(
        upload(&member, Kind::Blob, id, mine),
        Message::Stored { count: 1 }
    ));
    assert_eq!(
        send(addr, Some(&member), &Message::Want(vec![(Kind::Blob, id)])),
        Message::Missing(Vec::new())
    );
    assert!(matches!(
        send(addr, Some(&member), &Message::Get(vec![(Kind::Blob, id)])),
        Message::Objects(objects) if objects.len() == 1
    ));
    let (root, bytes) = tree_naming(id);
    assert!(matches!(
        upload(&member, Kind::Tree, root, bytes),
        Message::Stored { count: 1 }
    ));

    // Another caller learns nothing about it: unpublished and not theirs.
    assert_eq!(
        send(
            addr,
            Some(&stranger),
            &Message::Want(vec![(Kind::Blob, id)])
        ),
        Message::Missing(vec![(Kind::Blob, id)])
    );
    assert_eq!(
        error(send(
            addr,
            Some(&stranger),
            &Message::Get(vec![(Kind::Blob, id)])
        ))
        .0,
        "missing_object"
    );
    assert_eq!(
        error(send(addr, None, &Message::Get(vec![(Kind::Blob, id)]))).0,
        "missing_object"
    );
    server.join().unwrap();
}

#[test]
fn a_probe_of_another_kind_confirms_nothing() {
    let dir = TempDir::new().unwrap();
    let log = node(&dir);
    let member = token_for("member", REPO);
    let (addr, server) = start(&dir, log);
    let secret = ObjectId::for_blob(SECRET);
    let never = ObjectId::for_blob(b"TOKEN=guess\n");
    // Asking for the hidden blob as a tree or a checkpoint answers as for an absent one.
    for id in [secret, never] {
        assert_eq!(
            send(addr, Some(&member), &Message::Want(vec![(Kind::Tree, id)])),
            Message::Missing(vec![(Kind::Tree, id)])
        );
    }
    let (code, detail) = error(send(
        addr,
        Some(&member),
        &Message::Get(vec![(Kind::Checkpoint, never)]),
    ));
    assert_eq!(
        error(send(
            addr,
            Some(&member),
            &Message::Get(vec![(Kind::Checkpoint, secret)])
        )),
        (
            code,
            detail.replace(&never.to_string(), &secret.to_string())
        )
    );
    server.join().unwrap();
}

#[test]
fn an_append_cannot_drop_an_edit_the_writer_cannot_see() {
    let dir = TempDir::new().unwrap();
    let mut log = node(&dir);
    // A maintainer's Layer edits `.env`; a member's empty append against it would drop it.
    let shared = append(&mut log, "shared", &[(".env", Some(b"TOKEN=2\n"))]);
    let member = token_for("member", REPO);
    let (addr, server) = start(&dir, log);
    let empty = SparseChanges::default().canonical_bytes();
    let changes = ObjectId::for_checkpoint(&empty);
    assert!(matches!(
        send(
            addr,
            Some(&member),
            &Message::Objects(vec![(Kind::Checkpoint, changes, empty)])
        ),
        Message::Stored { .. }
    ));
    let append = |parent: ObjectId, n: &str| Message::LayerAppend {
        operation: op(n),
        layer: layer("shared"),
        parent: Some(parent),
        checkpoint: changes,
    };
    // The unreadable head is a parent the member cannot name: stale, as for any other.
    let reply = send(addr, Some(&member), &append(shared, "drop"));
    assert_eq!(
        reply,
        send(
            addr,
            Some(&member),
            &append(ObjectId::for_checkpoint(b"{}"), "other")
        )
    );
    assert!(
        matches!(reply, Message::LayerOutcome(LayerOutcome::Stale { .. })),
        "{reply:?}"
    );
    let log = server.join().unwrap();
    let LayerState::Active { head } = log.repo().layer_state(&layer("shared")).unwrap() else {
        panic!("the Layer moved")
    };
    assert_eq!(head.checkpoint().object(), shared);
}
