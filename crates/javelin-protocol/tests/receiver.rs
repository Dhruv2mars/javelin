//! Adversarial receiver behavior. Every refusal is reported to the peer, ends the
//! session, and leaves the World sequence unchanged.

mod support;

use javelin::{FileEntry, Manifest, ObjectId, OperationId, RepoPath, VersionNumber};
use javelin_protocol::{Kind, Message, Outcome, codec::encode, limits::*};
use support::*;

fn raw(id: ObjectId) -> Vec<u8> {
    id.as_bytes().to_vec()
}

fn msg(message: &Message) -> Vec<u8> {
    frame(&encode(message).unwrap())
}

fn publish(op: &str, expected: u64, manifest: ObjectId) -> Vec<u8> {
    msg(&Message::Publish {
        operation: OperationId::parse(op).unwrap(),
        expected: VersionNumber::new(expected).unwrap(),
        manifest,
    })
}

fn transfer(blobs: &[(ObjectId, Vec<u8>)], id: ObjectId, manifest: &Manifest) -> Vec<u8> {
    let mut input = hello();
    input.extend(msg(&Message::Blobs(blobs.to_vec())));
    input.extend(msg(&Message::Manifests(vec![(id, manifest.clone())])));
    input
}

#[test]
fn transfers_verifies_and_publishes_exact_outcome() {
    let (_s, mut source) = repo();
    let (id, blobs, manifest) = fixture(&mut source);
    let (_d, mut dest) = repo();
    let mut input = hello();
    let wanted: Vec<_> = std::iter::once((Kind::Manifest, id))
        .chain(blobs.iter().map(|(b, _)| (Kind::Blob, *b)))
        .collect();
    input.extend(msg(&Message::Want(wanted.clone())));
    input.extend(transfer(&blobs, id, &manifest)[hello().len()..].to_vec());
    input.extend(msg(&Message::Want(wanted.clone())));
    input.extend(publish("op-1", 0, id));
    let (result, replies) = session(&mut dest, &input);
    result.unwrap();
    let expected = Outcome::Accepted {
        number: VersionNumber::new(1).unwrap(),
        manifest: id,
        operation: OperationId::parse("op-1").unwrap(),
    };
    assert_eq!(
        replies,
        vec![
            Message::HelloOk { version: 1 },
            Message::Missing(wanted),
            Message::Stored { count: 3 },
            Message::Stored { count: 1 },
            Message::Missing(vec![]),
            Message::Outcome(expected.clone()),
        ]
    );
    let native = dest.compare_and_advance(
        &OperationId::parse("op-1").unwrap(),
        VersionNumber::new(0).unwrap(),
        id,
    );
    assert_eq!(
        Outcome::from_native(&native.unwrap(), id).unwrap(),
        expected
    );
    assert_eq!(dest.read_manifest(id).unwrap(), manifest);
    for (blob, bytes) in &blobs {
        assert_eq!(&dest.read_blob(*blob).unwrap(), bytes);
    }
}

#[test]
fn replay_returns_durable_outcome_and_changed_input_is_refused() {
    let (_s, mut source) = repo();
    let (id, blobs, manifest) = fixture(&mut source);
    let (_d, mut dest) = repo();
    let mut input = transfer(&blobs, id, &manifest);
    input.extend(publish("op-1", 0, id));
    input.extend(publish("op-1", 0, id));
    input.extend(publish("op-2", 0, id));
    let (result, replies) = session(&mut dest, &input);
    result.unwrap();
    assert_eq!(
        replies[3], replies[4],
        "identical replay returns the same outcome"
    );
    assert_eq!(
        replies[5],
        Message::Outcome(Outcome::Stale {
            actual: VersionNumber::new(1).unwrap()
        })
    );
    assert_eq!(dest.history().unwrap().len(), 2);
    // Same operation ID with a changed expected version or manifest.
    let mut changed = hello();
    changed.extend(publish("op-1", 1, id));
    refused(&mut dest, &changed, "operation_id_reused");
    let other = dest.put_manifest(&Manifest::new([]).unwrap()).unwrap();
    let mut changed = hello();
    changed.extend(publish("op-1", 0, other));
    refused(&mut dest, &changed, "operation_id_reused");
}

#[test]
fn refuses_unsupported_version_and_messages_before_hello() {
    let (_d, mut dest) = repo();
    refused(
        &mut dest,
        &msg(&Message::Hello { min: 2, max: 9 }),
        "unsupported_version",
    );
    refused(
        &mut dest,
        &msg(&Message::Want(vec![])),
        "unexpected_message",
    );
    let mut bad_magic = encode(&Message::Hello { min: 1, max: 1 }).unwrap();
    bad_magic[1] = b'X';
    refused(&mut dest, &frame(&bad_magic), "malformed");
    let mut twice = hello();
    twice.extend(hello());
    refused(&mut dest, &twice, "unexpected_message");
}

#[test]
fn refuses_blob_with_valid_hash_of_other_content() {
    let (_s, mut source) = repo();
    let (id, blobs, manifest) = fixture(&mut source);
    let (_d, mut dest) = repo();
    let mut input = hello();
    // The claim is a real blob ID, but the bytes belong to a different blob.
    input.extend(msg(&Message::Blobs(vec![
        blobs[2].clone(),
        (blobs[0].0, blobs[1].1.clone()),
    ])));
    refused(&mut dest, &input, "id_mismatch");
    assert!(matches!(
        dest.read_blob(blobs[0].0),
        Err(javelin::Error::MissingObject(_))
    ));
    // Valid prefix storage is retained, but mismatched bytes are never stored.
    assert_eq!(dest.read_blob(blobs[2].0).unwrap(), blobs[2].1);
    assert!(!dest.has_blob(blobs[1].0).unwrap());
    let retained = Manifest::new([(
        RepoPath::parse("retained").unwrap(),
        FileEntry::Regular {
            content: blobs[2].0,
            executable: false,
        },
    )])
    .unwrap();
    let retained_id = dest.put_manifest(&retained).unwrap();
    let outcome = dest
        .compare_and_advance(
            &OperationId::parse("retained").unwrap(),
            VersionNumber::new(0).unwrap(),
            retained_id,
        )
        .unwrap();
    assert_eq!(
        Outcome::from_native(&outcome, retained_id).unwrap(),
        Outcome::Accepted {
            number: VersionNumber::new(1).unwrap(),
            manifest: retained_id,
            operation: OperationId::parse("retained").unwrap(),
        }
    );
    // A manifest ID for a blob claim is also a mismatch: IDs are kind-separated.
    let mut input = hello();
    input.extend(msg(&Message::Blobs(vec![(id, manifest.canonical_bytes())])));
    refused(&mut dest, &input, "id_mismatch");
}

#[test]
fn refuses_manifest_with_mismatched_claim_without_publishing() {
    let (_s, mut source) = repo();
    let (id, blobs, manifest) = fixture(&mut source);
    let smaller = Manifest::new([(
        RepoPath::parse("only").unwrap(),
        FileEntry::Regular {
            content: blobs[0].0,
            executable: false,
        },
    )])
    .unwrap();
    let actual = ObjectId::for_manifest(&smaller);
    let (_d, mut dest) = repo();
    let mut input = hello();
    input.extend(msg(&Message::Blobs(blobs.clone())));
    input.extend(msg(&Message::Manifests(vec![(id, smaller)])));
    refused(&mut dest, &input, "id_mismatch");
    assert!(!dest.has_manifest(actual).unwrap());
    let _ = manifest;
    let mut input = hello();
    input.extend(publish("op-1", 0, id));
    refused(&mut dest, &input, "missing_object");
}

#[test]
fn refuses_wrong_kind_references() {
    let (_s, mut source) = repo();
    let (id, blobs, manifest) = fixture(&mut source);
    let (_d, mut dest) = repo();
    let mut input = transfer(&blobs, id, &manifest);
    input.extend(msg(&Message::Want(vec![(Kind::Manifest, blobs[0].0)])));
    refused(&mut dest, &input, "wrong_object_kind");
    let mut input = hello();
    input.extend(msg(&Message::Want(vec![(Kind::Blob, id)])));
    refused(&mut dest, &input, "wrong_object_kind");
    let mut input = hello();
    input.extend(publish("op-1", 0, blobs[0].0));
    refused(&mut dest, &input, "wrong_object_kind");
    // A manifest whose entry points at a manifest instead of a blob.
    let nested = Manifest::new([(
        RepoPath::parse("x").unwrap(),
        FileEntry::Regular {
            content: id,
            executable: false,
        },
    )])
    .unwrap();
    assert!(
        source.put_manifest(&nested).is_err(),
        "native API also refuses this graph"
    );
    let nested_id = ObjectId::for_manifest(&nested);
    let mut input = hello();
    input.extend(msg(&Message::Manifests(vec![(nested_id, nested)])));
    refused(&mut dest, &input, "wrong_object_kind");
}

#[test]
fn refuses_manifest_before_its_blobs() {
    let (_s, mut source) = repo();
    let (id, blobs, manifest) = fixture(&mut source);
    let (_d, mut dest) = repo();
    let mut input = hello();
    input.extend(msg(&Message::Blobs(blobs[..2].to_vec())));
    input.extend(msg(&Message::Manifests(vec![(id, manifest)])));
    refused(&mut dest, &input, "missing_object");
    let mut input = hello();
    input.extend(publish("op-1", 0, id));
    refused(&mut dest, &input, "missing_object");
}

type RawEntry<'a> = (&'a [u8], u8, ObjectId);

/// Builds a Manifests body by hand so invalid shapes can be expressed.
fn manifest_body(claim: ObjectId, entries: &[RawEntry]) -> Vec<u8> {
    let mut body = vec![0x21];
    body.extend(1u32.to_be_bytes());
    body.extend(raw(claim));
    body.extend((entries.len() as u32).to_be_bytes());
    for (path, kind, id) in entries {
        body.extend((path.len() as u16).to_be_bytes());
        body.extend(*path);
        body.push(*kind);
        body.extend(raw(*id));
    }
    body
}

#[test]
fn refuses_malformed_manifests() {
    let (_s, mut source) = repo();
    let (id, blobs, _) = fixture(&mut source);
    let b = blobs[0].0;
    let (_d, mut dest) = repo();
    let cases: Vec<(Vec<RawEntry>, &str)> = vec![
        (vec![(b"b", 0, b), (b"a", 0, b)], "malformed"),
        (vec![(b"a", 0, b), (b"a", 0, b)], "malformed"),
        (vec![(b"../x", 0, b)], "invalid_input"),
        (vec![(b".javelin/x", 0, b)], "invalid_input"),
        (vec![(b"a", 0, b), (b"a/b", 0, b)], "invalid_input"),
        (vec![(b"a", 7, b)], "malformed"),
        (vec![(&[0xff, 0xfe], 0, b)], "malformed"),
    ];
    for (entries, code) in cases {
        let mut input = hello();
        input.extend(msg(&Message::Blobs(blobs.clone())));
        input.extend(frame(&manifest_body(id, &entries)));
        refused(&mut dest, &input, code);
    }
}

#[test]
fn refuses_oversize_counts_lengths_and_depth_before_allocating() {
    let (_s, mut source) = repo();
    let (id, blobs, _) = fixture(&mut source);
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
    let mut want = vec![0x10];
    want.extend(u32::MAX.to_be_bytes());
    refused(&mut dest, &with(want), "limit_exceeded");
    let mut many = vec![0x20];
    many.extend(((MAX_BLOBS + 1) as u32).to_be_bytes());
    refused(&mut dest, &with(many), "limit_exceeded");
    let mut manifests = vec![0x21];
    manifests.extend(((MAX_MANIFESTS + 1) as u32).to_be_bytes());
    refused(&mut dest, &with(manifests), "limit_exceeded");
    let mut entries = manifest_body(id, &[]);
    let at = entries.len() - 4;
    entries[at..].copy_from_slice(&((MAX_ENTRIES + 1) as u32).to_be_bytes());
    refused(&mut dest, &with(entries), "limit_exceeded");
    // A count within its limit but beyond the bytes present.
    let mut short = vec![0x10];
    short.extend(10u32.to_be_bytes());
    refused(&mut dest, &with(short), "malformed");
    // A blob length above the per-object limit.
    let mut big = vec![0x20];
    big.extend(1u32.to_be_bytes());
    big.extend(raw(blobs[0].0));
    big.extend(((MAX_BLOB + 1) as u32).to_be_bytes());
    refused(&mut dest, &with(big), "limit_exceeded");
    // Path length and depth.
    let long = vec![b'a'; MAX_PATH + 1];
    refused(
        &mut dest,
        &with(manifest_body(id, &[(&long, 0, blobs[0].0)])),
        "limit_exceeded",
    );
    let deep = vec!["d"; MAX_DEPTH + 1].join("/");
    refused(
        &mut dest,
        &with(manifest_body(id, &[(deep.as_bytes(), 0, blobs[0].0)])),
        "limit_exceeded",
    );
    let ok_depth = vec!["d"; MAX_DEPTH].join("/");
    let (result, _) = session(
        &mut dest,
        &with(manifest_body(id, &[(ok_depth.as_bytes(), 0, blobs[0].0)])),
    );
    assert_ne!(
        result.unwrap_err().code,
        "limit_exceeded",
        "depth limit is inclusive"
    );
}

#[test]
fn refuses_truncation_and_trailing_bytes() {
    let (_s, mut source) = repo();
    let (id, blobs, manifest) = fixture(&mut source);
    let (_d, mut dest) = repo();
    let full = transfer(&blobs, id, &manifest);
    // Cut inside the last frame's body and inside a header.
    refused(&mut dest, &full[..full.len() - 3], "truncated");
    let mut header_cut = hello();
    header_cut.extend([0, 0]);
    refused(&mut dest, &header_cut, "truncated");
    // A frame whose body ends inside a field.
    let body = encode(&Message::Blobs(blobs.clone())).unwrap();
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
    input.extend(publish("op-9", 5, id));
    refused(&mut dest, &input, "missing_version");
    assert_eq!(dest.history().unwrap().len(), 1);
}

#[test]
fn messages_round_trip() {
    let (_s, mut source) = repo();
    let (id, blobs, manifest) = fixture(&mut source);
    let op = OperationId::parse("op.1_a-b").unwrap();
    let messages = vec![
        Message::Hello { min: 1, max: 3 },
        Message::HelloOk { version: 1 },
        Message::Want(vec![(Kind::Manifest, id), (Kind::Blob, blobs[0].0)]),
        Message::Missing(vec![]),
        Message::Blobs(blobs),
        Message::Manifests(vec![(id, manifest)]),
        Message::Stored { count: 3 },
        Message::Publish {
            operation: op.clone(),
            expected: VersionNumber::new(7).unwrap(),
            manifest: id,
        },
        Message::Outcome(Outcome::Accepted {
            number: VersionNumber::new(8).unwrap(),
            manifest: id,
            operation: op,
        }),
        Message::Outcome(Outcome::Stale {
            actual: VersionNumber::new(9).unwrap(),
        }),
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
    let big = Message::Blobs(vec![(id, vec![0; MAX_BLOB + 1])]);
    assert_eq!(encode(&big).map(|_| ()).unwrap_err().code, "limit_exceeded");
    let deep = vec!["d"; MAX_DEPTH + 1].join("/");
    let manifest = Manifest::new([(
        RepoPath::parse(&deep).unwrap(),
        FileEntry::Regular {
            content: id,
            executable: false,
        },
    )])
    .unwrap();
    let message = Message::Manifests(vec![(id, manifest)]);
    assert_eq!(
        encode(&message).map(|_| ()).unwrap_err().code,
        "limit_exceeded"
    );
    let many = Message::Want(vec![(Kind::Blob, id); MAX_WANT + 1]);
    assert_eq!(
        encode(&many).map(|_| ()).unwrap_err().code,
        "limit_exceeded"
    );
}

#[test]
fn refuses_duplicate_want_items() {
    let (_s, mut source) = repo();
    let (id, blobs, manifest) = fixture(&mut source);
    let (_d, mut dest) = repo();
    let mut input = transfer(&blobs, id, &manifest);
    input.extend(msg(&Message::Want(vec![(Kind::Blob, blobs[0].0); 2])));
    refused(&mut dest, &input, "malformed");
    // The same ID under both kinds is two distinct items, not a duplicate.
    let mut input = hello();
    input.extend(msg(&Message::Want(vec![
        (Kind::Blob, id),
        (Kind::Manifest, id),
    ])));
    refused(&mut dest, &input, "wrong_object_kind");
}
