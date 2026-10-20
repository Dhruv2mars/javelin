//! Adversarial receiver behavior. Every refusal is reported to the peer, ends the
//! session, and leaves the World sequence unchanged.

mod support;

use javelin::{FileEntry, Manifest, ObjectId, OperationId, RepoPath, VersionNumber};
use javelin_protocol::{Kind, Message, Outcome, codec::encode, limits::*};
use support::*;

fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}

fn publish(op: &str, expected: u64, tree: ObjectId) -> Vec<u8> {
    msg(&Message::Publish {
        operation: OperationId::parse(op).unwrap(),
        expected: v(expected),
        tree,
    })
}

/// Hello, then every blob and chunk of the fixture in one batch.
fn transfer(
    source: &javelin::Repository,
    root: ObjectId,
    blobs: &[(ObjectId, Vec<u8>)],
) -> Vec<u8> {
    let mut input = hello();
    input.extend(msg(&objects(blobs, &chunks(source, root))));
    input
}

/// One raw `Objects` item.
fn object(kind: u8, id: ObjectId, bytes: &[u8]) -> Vec<u8> {
    let mut body = vec![0x20];
    body.extend(1u32.to_be_bytes());
    body.push(kind);
    body.extend(id.as_bytes());
    body.extend((bytes.len() as u32).to_be_bytes());
    body.extend(bytes);
    body
}

/// A raw tree chunk.
fn chunk(level: u8, items: &[(u8, &[u8], ObjectId)]) -> Vec<u8> {
    let mut out = vec![level];
    for (tag, name, id) in items {
        out.push(*tag);
        out.extend((name.len() as u16).to_be_bytes());
        out.extend(*name);
        out.extend(id.as_bytes());
    }
    out
}

#[test]
fn transfers_verifies_and_publishes_exact_outcome() {
    let (_s, mut source) = repo();
    let (root, blobs, manifest) = fixture(&mut source);
    let all = chunks(&source, root);
    let (_d, mut dest) = repo();
    let wanted: Vec<_> = std::iter::once((Kind::Tree, root))
        .chain(blobs.iter().map(|(b, _)| (Kind::Blob, *b)))
        .collect();
    let mut input = hello();
    input.extend(msg(&Message::Want(wanted.clone())));
    input.extend(msg(&objects(&blobs, &all)));
    input.extend(msg(&Message::Want(wanted.clone())));
    input.extend(publish("op-1", 0, root));
    let (result, replies) = session(&mut dest, &input);
    result.unwrap();
    let expected = Outcome::Accepted {
        number: v(1),
        tree: root,
        operation: OperationId::parse("op-1").unwrap(),
    };
    assert_eq!(
        replies,
        vec![
            Message::HelloOk { version: 2 },
            Message::Missing(wanted),
            Message::Stored {
                count: (blobs.len() + all.len()) as u32
            },
            Message::Missing(vec![]),
            Message::Outcome(expected.clone()),
        ]
    );
    assert_eq!(dest.manifest_at(v(1)).unwrap(), manifest);
    assert_eq!(dest.head().unwrap().tree, root);
    for (blob, bytes) in &blobs {
        assert_eq!(&dest.read_blob(*blob).unwrap(), bytes);
    }
    dest.check().unwrap();
}

#[test]
fn replay_returns_durable_outcome_and_changed_input_is_refused() {
    let (_s, mut source) = repo();
    let (root, blobs, _) = fixture(&mut source);
    let (_d, mut dest) = repo();
    let mut input = transfer(&source, root, &blobs);
    input.extend(publish("op-1", 0, root));
    input.extend(publish("op-1", 0, root));
    input.extend(publish("op-2", 0, root));
    let (result, replies) = session(&mut dest, &input);
    result.unwrap();
    assert_eq!(
        replies[2], replies[3],
        "identical replay returns the same outcome"
    );
    assert_eq!(
        replies[4],
        Message::Outcome(Outcome::Stale { actual: v(1) })
    );
    assert_eq!(dest.history().unwrap().len(), 2);
    let mut changed = hello();
    changed.extend(publish("op-1", 1, root));
    refused(&mut dest, &changed, "operation_id_reused");
    let mut changed = hello();
    changed.extend(publish("op-1", 0, javelin::tree::empty()));
    refused(&mut dest, &changed, "operation_id_reused");
    dest.check().unwrap();
}

#[test]
fn refuses_unsupported_version_and_messages_before_hello() {
    let (_d, mut dest) = repo();
    refused(
        &mut dest,
        &msg(&Message::Hello { min: 1, max: 1 }),
        "unsupported_version",
    );
    refused(
        &mut dest,
        &msg(&Message::Want(vec![])),
        "unexpected_message",
    );
    let mut bad_magic = encode(&Message::Hello { min: 2, max: 2 }).unwrap();
    bad_magic[1] = b'X';
    refused(&mut dest, &frame(&bad_magic), "malformed");
    let mut twice = hello();
    twice.extend(hello());
    refused(&mut dest, &twice, "unexpected_message");
}

#[test]
fn refuses_objects_whose_bytes_do_not_match_their_claim() {
    let (_s, mut source) = repo();
    let (root, blobs, _) = fixture(&mut source);
    let all = chunks(&source, root);
    let (_d, mut dest) = repo();
    // The claim is a real blob ID, but the bytes belong to a different blob.
    let mut input = hello();
    input.extend(msg(&Message::Objects(vec![
        (Kind::Blob, blobs[2].0, blobs[2].1.clone()),
        (Kind::Blob, blobs[0].0, blobs[1].1.clone()),
    ])));
    refused(&mut dest, &input, "id_mismatch");
    // Valid prefix storage is retained, but mismatched bytes are never stored.
    assert_eq!(dest.read_blob(blobs[2].0).unwrap(), blobs[2].1);
    assert!(!dest.has_blob(blobs[0].0).unwrap());
    // A tree chunk claimed under another chunk's ID.
    let mut input = transfer(&source, root, &blobs);
    input.truncate(hello().len());
    input.extend(msg(&objects(&blobs, &[])));
    input.extend(frame(&object(4, root, &all[0].1)));
    refused(&mut dest, &input, "id_mismatch");
    assert!(!dest.has_tree(root).unwrap());
    // IDs are kind-separated: tree bytes claimed as a blob do not match.
    let mut input = hello();
    input.extend(frame(&object(1, all[0].0, &all[0].1)));
    refused(&mut dest, &input, "id_mismatch");
}

#[test]
fn refuses_chunks_before_what_they_name_and_wrong_kinds() {
    let (_s, mut source) = repo();
    let (root, blobs, _) = fixture(&mut source);
    let all = chunks(&source, root);
    let (_d, mut dest) = repo();
    // A chunk before its blobs, and the root before its subdirectories.
    let mut input = hello();
    input.extend(msg(&objects(&[], &all)));
    refused(&mut dest, &input, "missing_object");
    let mut input = hello();
    input.extend(msg(&objects(&blobs, &all[all.len() - 1..])));
    refused(&mut dest, &input, "missing_object");
    assert!(!dest.has_tree(root).unwrap());
    // Publishing a root the receiver lacks.
    let mut input = hello();
    input.extend(publish("op-1", 0, root));
    refused(&mut dest, &input, "missing_object");
    // A leaf naming a stored chunk as file content, and Want/Publish with the wrong kind.
    let mut input = transfer(&source, root, &blobs);
    let bad = chunk(0, &[(1, b"x", root)]);
    input.extend(frame(&object(4, ObjectId::for_tree(&bad), &bad)));
    refused(&mut dest, &input, "wrong_object_kind");
    let mut input = hello();
    input.extend(msg(&Message::Want(vec![(Kind::Tree, blobs[0].0)])));
    refused(&mut dest, &input, "wrong_object_kind");
    let mut input = hello();
    input.extend(msg(&Message::Want(vec![(Kind::Blob, root)])));
    refused(&mut dest, &input, "wrong_object_kind");
    let mut input = hello();
    input.extend(publish("op-1", 0, blobs[0].0));
    refused(&mut dest, &input, "wrong_object_kind");
}

#[test]
fn checkpoint_in_a_mixed_batch_refuses_the_whole_batch() {
    // Checkpoints are not stored yet, so a batch holding one stores none of it.
    let (_d, mut dest) = repo();
    let bytes = b"a blob that must not be stored".to_vec();
    let blob = Kind::Blob.id(&bytes);
    let checkpoint = b"{}".to_vec();
    let mut input = hello();
    input.extend(msg(&Message::Objects(vec![
        (Kind::Blob, blob, bytes),
        (
            Kind::Checkpoint,
            Kind::Checkpoint.id(&checkpoint),
            checkpoint,
        ),
    ])));
    refused(&mut dest, &input, "malformed");
    assert!(!dest.has_blob(blob).unwrap());
}

#[test]
fn refuses_malformed_tree_chunks() {
    let (_s, mut source) = repo();
    let (_, blobs, _) = fixture(&mut source);
    let b = blobs[0].0;
    let (_d, mut dest) = repo();
    let cases: Vec<Vec<u8>> = vec![
        chunk(0, &[(1, b"b", b), (1, b"a", b)]),
        chunk(0, &[(1, b"a", b), (1, b"a", b)]),
        chunk(0, &[(1, b"..", b)]),
        chunk(0, &[(1, b".javelin", b)]),
        chunk(0, &[(1, b"a/b", b)]),
        chunk(0, &[(7, b"a", b)]),
        chunk(0, &[(0, b"a", b)]),
        chunk(1, &[(1, b"a", b)]),
        chunk(1, &[]),
        chunk(0, &[(1, &[0xff, 0xfe], b)]),
        [chunk(0, &[(1, b"a", b)]), vec![9]].concat(),
        vec![],
    ];
    for bytes in cases {
        let mut input = hello();
        input.extend(msg(&objects(&blobs, &[])));
        input.extend(frame(&object(4, ObjectId::for_tree(&bytes), &bytes)));
        refused(&mut dest, &input, "malformed");
    }
}

#[test]
fn publish_refuses_a_tree_that_is_not_canonical() {
    let (_s, mut source) = repo();
    let (root, blobs, _) = fixture(&mut source);
    let (_d, mut dest) = repo();
    let mut input = transfer(&source, root, &blobs);
    // A directory with no entries is not a canonical snapshot.
    let empty = javelin::tree::empty();
    let hollow = chunk(0, &[(4, b"empty", empty), (1, b"f", blobs[0].0)]);
    let hollow_id = ObjectId::for_tree(&hollow);
    input.extend(frame(&object(4, hollow_id, &hollow)));
    input.extend(publish("hollow", 0, hollow_id));
    refused(&mut dest, &input, "invalid_input");
    // The same entries cut into two leaf chunks instead of one.
    let a = chunk(0, &[(1, b"a", blobs[0].0)]);
    let c = chunk(0, &[(1, b"c", blobs[0].0)]);
    let (a_id, c_id) = (ObjectId::for_tree(&a), ObjectId::for_tree(&c));
    let split = chunk(1, &[(0, b"a", a_id), (0, b"c", c_id)]);
    let split_id = ObjectId::for_tree(&split);
    let mut input = hello();
    input.extend(frame(&object(4, a_id, &a)));
    input.extend(frame(&object(4, c_id, &c)));
    input.extend(frame(&object(4, split_id, &split)));
    input.extend(publish("split", 0, split_id));
    refused(&mut dest, &input, "invalid_input");
    // A symlink with an empty target fails content validation.
    let empty_blob = dest.put_blob(b"").unwrap();
    let link = chunk(0, &[(3, b"l", empty_blob)]);
    let link_id = ObjectId::for_tree(&link);
    let mut input = hello();
    input.extend(frame(&object(4, link_id, &link)));
    input.extend(publish("link", 0, link_id));
    refused(&mut dest, &input, "invalid_input");
    dest.check().unwrap();
}

#[test]
fn refuses_oversize_counts_and_lengths_before_allocating() {
    let (_s, mut source) = repo();
    let (_, blobs, _) = fixture(&mut source);
    let (_d, mut dest) = repo();
    let with = |body: Vec<u8>| {
        let mut input = hello();
        input.extend(frame(&body));
        input
    };
    // Frame length above the limit is refused from the header alone.
    let mut input = hello();
    input.extend(((MAX_FRAME + 1) as u32).to_be_bytes());
    refused(&mut dest, &input, "limit_exceeded");
    // Counts above their limits.
    for tag in [0x10, 0x12] {
        let mut want = vec![tag];
        want.extend(u32::MAX.to_be_bytes());
        refused(&mut dest, &with(want), "limit_exceeded");
    }
    let mut many = vec![0x20];
    many.extend(((MAX_OBJECTS + 1) as u32).to_be_bytes());
    refused(&mut dest, &with(many), "limit_exceeded");
    // A count within its limit but beyond the bytes present.
    let mut short = vec![0x10];
    short.extend(10u32.to_be_bytes());
    refused(&mut dest, &with(short), "malformed");
    // Object lengths above the per-kind limits.
    for (kind, limit) in [(1u8, MAX_BLOB), (4, MAX_TREE)] {
        let mut big = vec![0x20];
        big.extend(1u32.to_be_bytes());
        big.push(kind);
        big.extend(blobs[0].0.as_bytes());
        big.extend(((limit + 1) as u32).to_be_bytes());
        refused(&mut dest, &with(big), "limit_exceeded");
    }
    // Unknown kinds, including the removed manifest kind.
    let mut manifest = vec![0x10];
    manifest.extend(1u32.to_be_bytes());
    manifest.push(2);
    manifest.extend([0; 32]);
    refused(&mut dest, &with(manifest), "malformed");
}

#[test]
fn snapshots_beyond_the_old_entry_cap_transfer_and_publish() {
    // Version 1 refused manifests over 65,536 entries; trees have no such cap.
    let (_s, mut source) = repo();
    let blob = source.put_blob(b"same").unwrap();
    let manifest = Manifest::new((0..70_000).map(|n| {
        (
            RepoPath::parse(&format!("d{}/f{n}", n % 300)).unwrap(),
            FileEntry::Regular {
                content: blob,
                executable: false,
            },
        )
    }))
    .unwrap();
    let root = source.put_snapshot(&manifest).unwrap();
    let (_d, mut dest) = repo();
    let mut input = hello();
    let all = chunks(&source, root);
    input.extend(msg(&objects(&[(blob, b"same".to_vec())], &[])));
    for batch in all.chunks(MAX_OBJECTS) {
        input.extend(msg(&objects(&[], batch)));
    }
    input.extend(publish("big", 0, root));
    let (result, _) = session(&mut dest, &input);
    result.unwrap();
    assert_eq!(dest.manifest_at(v(1)).unwrap(), manifest);
}

#[test]
fn refuses_truncation_and_trailing_bytes() {
    let (_s, mut source) = repo();
    let (root, blobs, _) = fixture(&mut source);
    let (_d, mut dest) = repo();
    let full = transfer(&source, root, &blobs);
    // Cut inside the last frame's body and inside a header.
    refused(&mut dest, &full[..full.len() - 3], "truncated");
    let mut header_cut = hello();
    header_cut.extend([0, 0]);
    refused(&mut dest, &header_cut, "truncated");
    // A frame whose body ends inside a field.
    let body = encode(&objects(&blobs, &[])).unwrap();
    let mut input = hello();
    input.extend(frame(&body[..body.len() - 1]));
    refused(&mut dest, &input, "malformed");
    // Trailing bytes after a complete message.
    let mut body = encode(&Message::Want(vec![])).unwrap();
    body.push(0);
    let mut input = hello();
    input.extend(frame(&body));
    refused(&mut dest, &input, "malformed");
    let mut input = hello();
    input.extend(frame(&[]));
    refused(&mut dest, &input, "malformed");
    let mut input = hello();
    input.extend(frame(&[0x55]));
    refused(&mut dest, &input, "malformed");
    // Publish expecting a version beyond head never advances.
    let mut input = full.clone();
    input.extend(publish("op-9", 5, root));
    refused(&mut dest, &input, "missing_version");
    assert_eq!(dest.history().unwrap().len(), 1);
}

#[test]
fn messages_round_trip() {
    let (_s, mut source) = repo();
    let (root, blobs, _) = fixture(&mut source);
    let op = OperationId::parse("op.1_a-b").unwrap();
    let messages = vec![
        Message::Hello { min: 1, max: 3 },
        Message::HelloOk { version: 2 },
        Message::Want(vec![(Kind::Tree, root), (Kind::Blob, blobs[0].0)]),
        Message::Missing(vec![]),
        Message::Get(vec![(Kind::Tree, root)]),
        objects(&blobs, &chunks(&source, root)),
        Message::Stored { count: 3 },
        Message::Publish {
            operation: op.clone(),
            expected: v(7),
            tree: root,
        },
        Message::Outcome(Outcome::Accepted {
            number: v(8),
            tree: root,
            operation: op,
        }),
        Message::Outcome(Outcome::Stale { actual: v(9) }),
        Message::Resolve { version: None },
        Message::Resolve {
            version: Some(v(0)),
        },
        Message::Resolved {
            number: v(3),
            id: blobs[0].0,
            tree: root,
        },
        Message::Error {
            code: "id_mismatch".into(),
            detail: "é".repeat(2000),
        },
        Message::Error {
            code: "é".into(),
            detail: "UTF-8 code".into(),
        },
    ];
    for message in messages {
        let decoded = javelin_protocol::codec::decode(&encode(&message).unwrap()).unwrap();
        match (&message, &decoded) {
            (
                Message::Error { code, detail },
                Message::Error {
                    code: got_code,
                    detail: got,
                },
            ) => {
                assert_eq!(got_code, code);
                assert!(got.len() <= MAX_DETAIL && detail.starts_with(got.as_str()))
            }
            _ => assert_eq!(decoded, message),
        }
    }
}

#[test]
fn encoder_refuses_messages_the_receiver_would_refuse() {
    let id = ObjectId::parse(&"ab".repeat(32)).unwrap();
    let big = Message::Objects(vec![(Kind::Blob, id, vec![0; MAX_BLOB + 1])]);
    assert_eq!(encode(&big).map(|_| ()).unwrap_err().code, "limit_exceeded");
    let big = Message::Objects(vec![(Kind::Tree, id, vec![0; MAX_TREE + 1])]);
    assert_eq!(encode(&big).map(|_| ()).unwrap_err().code, "limit_exceeded");
    let many = Message::Objects(vec![(Kind::Blob, id, vec![]); MAX_OBJECTS + 1]);
    assert_eq!(
        encode(&many).map(|_| ()).unwrap_err().code,
        "limit_exceeded"
    );
    let many = Message::Want(vec![(Kind::Blob, id); MAX_WANT + 1]);
    assert_eq!(
        encode(&many).map(|_| ()).unwrap_err().code,
        "limit_exceeded"
    );
}

#[test]
fn refuses_duplicate_requests() {
    let (_s, mut source) = repo();
    let (root, blobs, _) = fixture(&mut source);
    let (_d, mut dest) = repo();
    for tag in [0x10, 0x12] {
        let mut input = transfer(&source, root, &blobs);
        let mut body = encode(&Message::Want(vec![(Kind::Blob, blobs[0].0); 2])).unwrap();
        body[0] = tag;
        input.extend(frame(&body));
        refused(&mut dest, &input, "malformed");
    }
    // The same ID under both kinds is two distinct items, not a duplicate.
    let mut input = hello();
    input.extend(msg(&Message::Want(vec![
        (Kind::Blob, root),
        (Kind::Tree, root),
    ])));
    refused(&mut dest, &input, "wrong_object_kind");
}

#[test]
fn get_returns_a_bounded_prefix_and_refuses_missing_objects() {
    let (_s, mut source) = repo();
    let big: Vec<_> = (0..3u8)
        .map(|i| {
            let bytes = vec![i; MAX_BLOB];
            (source.put_blob(&bytes).unwrap(), bytes)
        })
        .collect();
    let mut input = hello();
    input.extend(msg(&Message::Get(
        big.iter().map(|(id, _)| (Kind::Blob, *id)).collect(),
    )));
    let (result, replies) = session(&mut source, &input);
    result.unwrap();
    // Two 8 MiB blobs do not fit one 16 MiB frame with their headers.
    match &replies[1] {
        Message::Objects(objects) => {
            assert_eq!(objects.len(), 1);
            assert_eq!(objects[0], (Kind::Blob, big[0].0, big[0].1.clone()));
        }
        other => panic!("{other:?}"),
    }
    let mut input = hello();
    input.extend(msg(&Message::Get(vec![(
        Kind::Tree,
        ObjectId::for_tree(b"\x00\x01"),
    )])));
    refused(&mut source, &input, "missing_object");
}

#[test]
fn publish_refuses_trees_deeper_than_native_paths() {
    let (_s, mut source) = repo();
    let (_, blobs, _) = fixture(&mut source);
    let (_d, mut dest) = repo();
    let mut input = hello();
    input.extend(msg(&objects(&blobs[..1], &[])));
    // A chain of javelin::MAX_DEPTH directories above one file.
    let mut below = chunk(0, &[(1, b"f", blobs[0].0)]);
    let mut id = ObjectId::for_tree(&below);
    input.extend(frame(&object(4, id, &below)));
    for _ in 0..javelin::MAX_DEPTH {
        below = chunk(0, &[(4, b"d", id)]);
        id = ObjectId::for_tree(&below);
        input.extend(frame(&object(4, id, &below)));
    }
    input.extend(publish("deep", 0, id));
    refused(&mut dest, &input, "invalid_input");
}
