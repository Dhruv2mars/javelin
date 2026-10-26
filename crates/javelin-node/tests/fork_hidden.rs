//! Forks of a repository with hidden entries: the fork copies committed trees whole, so
//! its version IDs match the source's, and copies an opening only for a reader in its
//! audience.

use javelin::{
    FileEntry, LayerId, ObjectId, OperationId, POLICY_PATH, PublishAttribution, Reader, RepoPath,
    SparseChanges, VersionNumber,
};
use javelin_collaboration::Role;
use javelin_node::{FileWorldLog, fork};
use javelin_protocol::{codec::PublishOutcome, receiver::WorldLog};
use tempfile::TempDir;

fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}
fn path(s: &str) -> RepoPath {
    RepoPath::parse(s).unwrap()
}

fn publish(log: &mut FileWorldLog, name: &str, files: &[(&str, &[u8])]) {
    let layer = LayerId::parse(name).unwrap();
    let base = log.head().unwrap().number;
    log.create_layer(&layer, base).unwrap();
    let changes = SparseChanges::new(files.iter().map(|(p, bytes)| {
        let content = log.repository().put_blob(bytes).unwrap();
        (
            path(p),
            Some(FileEntry::Regular {
                content,
                executable: false,
            }),
        )
    }))
    .unwrap();
    let op = |s: &str| OperationId::parse(&format!("{s}-{name}")).unwrap();
    let expected = log
        .append(&op("write"), &layer, &changes)
        .unwrap()
        .checkpoint()
        .object();
    let outcome = log
        .publish_layer(
            &op("publish"),
            &layer,
            expected,
            base,
            &PublishAttribution::Human,
        )
        .unwrap();
    assert!(
        matches!(outcome, PublishOutcome::Published { .. }),
        "{outcome:?}"
    );
}

fn source(dir: &TempDir) -> FileWorldLog {
    let mut log = FileWorldLog::open(dir.path()).unwrap();
    publish(
        &mut log,
        "one",
        &[
            (POLICY_PATH, b".env maintainers\ninternal/** members\n"),
            (".env", b"TOKEN=1\n"),
            ("internal/notes", b"notes\n"),
            ("src/a", b"a\n"),
        ],
    );
    publish(
        &mut log,
        "two",
        &[(".env", b"TOKEN=2\n"), ("src/a", b"b\n")],
    );
    log
}

fn hidden_commitments(log: &FileWorldLog, n: u64) -> Vec<ObjectId> {
    log.repo()
        .committed_at(v(n))
        .unwrap()
        .entries()
        .values()
        .filter_map(|entry| match entry {
            FileEntry::Hidden { commitment, .. } => Some(*commitment),
            _ => None,
        })
        .collect()
}

#[test]
fn an_outsiders_fork_keeps_commitments_and_matching_ids() {
    let (src_dir, dst_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let src = source(&src_dir);
    let mut dst = FileWorldLog::open(dst_dir.path()).unwrap();
    fork::fork_for(&src, "repo-src", &mut dst, v(2), None).unwrap();
    for n in 0..=2 {
        let (ours, theirs) = (
            dst.repo().version(v(n)).unwrap(),
            src.repo().version(v(n)).unwrap(),
        );
        assert_eq!((ours.id, ours.tree), (theirs.id, theirs.tree));
        assert_eq!(
            dst.repo().committed_at(v(n)).unwrap(),
            src.repo().committed_at(v(n)).unwrap()
        );
    }
    // The fork holds commitments only: no opening, no hidden bytes, and its view leaves
    // hidden entries closed.
    let commitments = [hidden_commitments(&src, 1), hidden_commitments(&src, 2)].concat();
    assert_eq!(commitments.len(), 4);
    assert!(
        dst.repo()
            .openings(&commitments, &Reader::maintainer())
            .unwrap()
            .is_empty()
    );
    for secret in [&b"TOKEN=1\n"[..], b"TOKEN=2\n", b"notes\n"] {
        assert!(!dst.repo().has_blob(ObjectId::for_blob(secret)).unwrap());
    }
    let view = dst.repo().manifest_at(v(2)).unwrap();
    assert!(view.entries()[&path(".env")].is_hidden());
    assert!(view.entries()[&path("internal")].is_hidden());
    dst.repo().check().unwrap();
}

#[test]
fn a_fork_copies_the_openings_its_reader_may_read() {
    let src_dir = TempDir::new().unwrap();
    let src = source(&src_dir);
    let commitments = [hidden_commitments(&src, 1), hidden_commitments(&src, 2)].concat();
    for (role, opened) in [(Role::Read, 2), (Role::Maintain, 4)] {
        let dst_dir = TempDir::new().unwrap();
        let mut dst = FileWorldLog::open(dst_dir.path()).unwrap();
        fork::fork(&src, "repo-src", &mut dst, v(2), role).unwrap();
        let (ours, theirs) = (dst.repo().head().unwrap(), src.repo().head().unwrap());
        assert_eq!((ours.id, ours.tree), (theirs.id, theirs.tree));
        let held = dst
            .repo()
            .openings(&commitments, &Reader::maintainer())
            .unwrap();
        assert_eq!(held.len(), opened, "{role:?}");
        let view = dst.repo().manifest_at(v(2)).unwrap();
        assert!(!view.entries().contains_key("internal"));
        assert_eq!(
            view.entries()[&path("internal/notes")],
            src.repo().manifest_at(v(2)).unwrap().entries()[&path("internal/notes")]
        );
        assert_eq!(
            view.entries()[&path(".env")].is_hidden(),
            role != Role::Maintain
        );
        dst.repo().check().unwrap();
    }
}

/// One message to the loopback node; returns the decoded reply.
fn ask(
    addr: std::net::SocketAddr,
    message: &javelin_protocol::Message,
) -> javelin_protocol::Message {
    use std::io::{Read, Write};
    let body = javelin_protocol::codec::encode(message).unwrap();
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "POST /v2/r/demo HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    javelin_protocol::codec::decode(&reply[split + 4..]).unwrap()
}

#[test]
fn the_node_serves_an_anonymous_caller_commitments_only() {
    use javelin_protocol::{Message, codec::Kind};
    let dir = TempDir::new().unwrap();
    let mut log = source(&dir);
    // A maintainer's unpublished Layer that edits the hidden file, and one that does not.
    let edit = |log: &mut FileWorldLog, name: &str, p: &str, bytes: &[u8]| {
        let layer = LayerId::parse(name).unwrap();
        log.create_layer(&layer, v(2)).unwrap();
        let content = log.repository().put_blob(bytes).unwrap();
        let changes = SparseChanges::new([(
            path(p),
            Some(FileEntry::Regular {
                content,
                executable: false,
            }),
        )])
        .unwrap();
        log.append(
            &OperationId::parse(&format!("w-{name}")).unwrap(),
            &layer,
            &changes,
        )
        .unwrap()
        .checkpoint()
        .object()
    };
    let hidden_edit = edit(&mut log, "secret", ".env", b"TOKEN=3\n");
    let public_edit = edit(&mut log, "public", "src/a", b"c\n");
    let commitments = hidden_commitments(&log, 2);
    let secret = ObjectId::for_blob(b"TOKEN=2\n");
    let listener = javelin_node::bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        javelin_node::serve(&listener, "demo", &mut log, Some(5)).unwrap();
    });

    assert_eq!(
        ask(addr, &Message::Open(commitments)),
        Message::Opened(Vec::new())
    );
    let refused =
        |reply: Message| matches!(reply, Message::Error { code, .. } if code == "missing_object");
    assert!(refused(ask(
        addr,
        &Message::Get(vec![(Kind::Blob, secret)])
    )));
    assert!(refused(ask(
        addr,
        &Message::Get(vec![(Kind::Checkpoint, hidden_edit)])
    )));
    assert!(matches!(
        ask(addr, &Message::Get(vec![(Kind::Checkpoint, public_edit)])),
        Message::Objects(objects) if objects.len() == 1
    ));
    assert_eq!(
        ask(addr, &Message::Want(vec![(Kind::Blob, secret)])),
        Message::Missing(vec![(Kind::Blob, secret)])
    );
    server.join().unwrap();
}

/// Publishes `files` (None deletes) as Layer `name` and returns its head checkpoint.
fn publish_checkpoint(
    log: &mut FileWorldLog,
    name: &str,
    files: &[(&str, Option<&[u8]>)],
) -> ObjectId {
    let layer = LayerId::parse(name).unwrap();
    let base = log.head().unwrap().number;
    log.create_layer(&layer, base).unwrap();
    let changes = SparseChanges::new(files.iter().map(|(p, bytes)| {
        let entry = bytes.map(|bytes| FileEntry::Regular {
            content: log.repository().put_blob(bytes).unwrap(),
            executable: false,
        });
        (path(p), entry)
    }))
    .unwrap();
    let op = |s: &str| OperationId::parse(&format!("{s}-{name}")).unwrap();
    let checkpoint = log
        .append(&op("write"), &layer, &changes)
        .unwrap()
        .checkpoint()
        .object();
    let outcome = log
        .publish_layer(
            &op("publish"),
            &layer,
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

#[test]
fn an_edit_made_while_hidden_stays_hidden_after_the_rule_is_removed() {
    use javelin_protocol::{Message, codec::Kind};
    let dir = TempDir::new().unwrap();
    let mut log = FileWorldLog::open(dir.path()).unwrap();
    publish_checkpoint(
        &mut log,
        "policy",
        &[
            (POLICY_PATH, Some(b".env maintainers\n")),
            ("src/a", Some(b"a\n")),
        ],
    );
    // A maintainer edits the hidden `.env` and publishes it.
    let secret = publish_checkpoint(&mut log, "secret", &[(".env", Some(b"TOKEN=old\n"))]);
    // Later the rule is removed and `.env` is replaced with public content.
    let disclose = publish_checkpoint(
        &mut log,
        "disclose",
        &[(POLICY_PATH, Some(b"")), (".env", Some(b"TOKEN=public\n"))],
    );
    // A Layer made after the rule was removed touches only visible paths.
    let public = publish_checkpoint(&mut log, "public", &[(".env", Some(b"TOKEN=new\n"))]);
    // A Layer staged while the rule held, never appended.
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
    // The head policy alone would now serve the old edit; the policy that governed it
    // does not.
    assert!(
        log.repo()
            .policy(log.head().unwrap().number)
            .unwrap()
            .is_empty()
    );
    let checkpoint = |id: ObjectId| javelin::CheckpointId::parse(&id.to_string()).unwrap();
    assert!(
        !log.repo()
            .checkpoint_visible(checkpoint(secret), &Reader::outsider())
            .unwrap()
    );
    assert!(
        log.repo()
            .checkpoint_visible(checkpoint(secret), &Reader::maintainer())
            .unwrap()
    );
    assert!(
        log.repo()
            .checkpoint_visible(checkpoint(public), &Reader::outsider())
            .unwrap()
    );
    // The disclosing Layer was made while `.env` was still hidden, so it stays refused.
    assert!(
        !log.repo()
            .checkpoint_visible(checkpoint(disclose), &Reader::outsider())
            .unwrap()
    );

    let listener = javelin_node::bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        javelin_node::serve(&listener, "demo", &mut log, Some(4)).unwrap();
    });
    // Refused exactly as a checkpoint that was never stored.
    let never = ObjectId::for_checkpoint(b"{}never");
    let Message::Error { code, detail } = ask(addr, &Message::Get(vec![(Kind::Checkpoint, never)]))
    else {
        panic!("a missing checkpoint was served")
    };
    let Message::Error {
        code: refused,
        detail: why,
    } = ask(addr, &Message::Get(vec![(Kind::Checkpoint, secret)]))
    else {
        panic!("the hidden edit's checkpoint was served")
    };
    assert_eq!(
        (refused, why),
        (
            code,
            detail.replace(&never.to_string(), &secret.to_string())
        )
    );
    assert!(matches!(
        ask(addr, &Message::Get(vec![(Kind::Checkpoint, public)])),
        Message::Objects(objects) if objects.len() == 1
    ));
    assert_eq!(
        ask(addr, &Message::Want(vec![(Kind::Checkpoint, staged_id)])),
        Message::Missing(vec![(Kind::Checkpoint, staged_id)])
    );
    server.join().unwrap();
}

#[test]
fn a_conflict_with_a_hidden_change_names_no_path() {
    use javelin_protocol::Message;
    let dir = TempDir::new().unwrap();
    let mut log = FileWorldLog::open(dir.path()).unwrap();
    publish_checkpoint(
        &mut log,
        "policy",
        &[
            (POLICY_PATH, Some(b".env maintainers\n")),
            (".env", Some(b"one\n")),
        ],
    );
    // A Layer on World 1 edits `.env`; meanwhile World 2 edits it too.
    let layer = LayerId::parse("late").unwrap();
    log.create_layer(&layer, v(1)).unwrap();
    let content = log.repository().put_blob(b"late\n").unwrap();
    let changes = SparseChanges::new([(
        path(".env"),
        Some(FileEntry::Regular {
            content,
            executable: false,
        }),
    )])
    .unwrap();
    let head = log
        .append(&OperationId::parse("write-late").unwrap(), &layer, &changes)
        .unwrap()
        .checkpoint()
        .object();
    publish_checkpoint(&mut log, "first", &[(".env", Some(b"two\n"))]);
    let listener = javelin_node::bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        javelin_node::serve(&listener, "demo", &mut log, Some(1)).unwrap();
    });
    let reply = ask(
        addr,
        &Message::PublishLayer {
            operation: OperationId::parse("publish-late").unwrap(),
            layer,
            expected: head,
            base: v(1),
            attribution: PublishAttribution::Human,
        },
    );
    assert_eq!(
        reply,
        Message::PublishOutcome(PublishOutcome::Conflict { paths: Vec::new() })
    );
    server.join().unwrap();
}
