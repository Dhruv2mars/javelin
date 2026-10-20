//! Codec tests for the version 2 hosting messages: every variant round-trips, and
//! unknown tags, retired kinds, malformed fields and oversize values are refused with
//! the codes in `codec::code`.

use javelin::{LayerId, ObjectId, OperationId, PublishAttribution, RepoPath, VersionNumber};
use javelin_protocol::{
    Kind, Message,
    codec::{
        LayerOutcome, LayerStatus, PublishOutcome, RequestId, RequestStatus, ReviewOutcome, Role,
        Visibility, code, decode, encode, read_frame, write_frame,
    },
    limits::{MAX_CHECKPOINT, MAX_COMMENT, MAX_FRAME, MAX_WANT},
};

const WANT: u8 = 0x10;
const OBJECTS: u8 = 0x20;
const APPLIED: u8 = 0x06;
const LAYER_OUTCOME: u8 = 0x33;
const LAYER_APPEND: u8 = 0x34;
const LAYER_STATE: u8 = 0x36;
const PUBLISH_LAYER: u8 = 0x38;
const PUBLISH_OUTCOME: u8 = 0x39;
const REVIEW_OUTCOME: u8 = 0x51;
const COMMENT: u8 = 0x54;
const REQUEST_STATE: u8 = 0x56;

fn id(n: u8) -> ObjectId {
    ObjectId::from_bytes([n; 32])
}

fn op(value: &str) -> OperationId {
    OperationId::parse(value).unwrap()
}

fn layer() -> LayerId {
    LayerId::parse("layer-a").unwrap()
}

fn request() -> RequestId {
    RequestId::parse("req-1").unwrap()
}

fn version(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}

/// Encodes, decodes, and checks the body and the framed stream both return `message`.
fn roundtrip(message: Message) {
    let body = encode(&message).unwrap();
    assert_eq!(decode(&body).unwrap(), message);
    let mut wire = Vec::new();
    write_frame(&mut wire, &message).unwrap();
    assert_eq!(read_frame(&mut wire.as_slice()).unwrap(), Some(message));
}

fn refused(body: &[u8]) -> String {
    decode(body).unwrap_err().code
}

/// A length-prefixed frame around `body`, as `read_frame` expects.
fn frame(body: &[u8]) -> Vec<u8> {
    let mut wire = (body.len() as u32).to_be_bytes().to_vec();
    wire.extend_from_slice(body);
    wire
}

#[test]
fn every_hosting_message_round_trips() {
    let messages = [
        Message::Permissions,
        Message::Applied {
            epoch: 7,
            caller_role: Role::Maintain,
            visibility: Visibility::Private,
        },
        Message::Applied {
            epoch: 0,
            caller_role: Role::Read,
            visibility: Visibility::Public,
        },
        Message::Applied {
            epoch: 2,
            caller_role: Role::Review,
            visibility: Visibility::Public,
        },
        Message::LayerAppend {
            operation: op("append-1"),
            layer: layer(),
            parent: None,
            checkpoint: id(1),
        },
        Message::LayerAppend {
            operation: op("append-2"),
            layer: layer(),
            parent: Some(id(2)),
            checkpoint: id(3),
        },
        Message::LayerOutcome(LayerOutcome::Accepted {
            seq: 4,
            checkpoint: id(3),
        }),
        Message::LayerOutcome(LayerOutcome::Stale { head: id(5) }),
        Message::LayerOutcome(LayerOutcome::Refused {
            code: code::LAYER_STALE.into(),
        }),
        Message::LayerHead { layer: layer() },
        Message::LayerState {
            layer: layer(),
            base: version(3),
            seq: 4,
            head: id(3),
            state: LayerStatus::Active,
        },
        Message::LayerState {
            layer: layer(),
            base: version(3),
            seq: 9,
            head: id(4),
            state: LayerStatus::Published,
        },
        Message::LayerState {
            layer: layer(),
            base: version(0),
            seq: 0,
            head: id(0),
            state: LayerStatus::Discarded,
        },
        Message::LayerStack {
            operation: op("stack-1"),
            target: layer(),
            target_head: id(1),
            source: id(2),
        },
        Message::PublishLayer {
            operation: op("publish-1"),
            layer: layer(),
            expected: id(3),
            base: version(2),
            attribution: PublishAttribution::Human,
        },
        Message::PublishLayer {
            operation: op("publish-2"),
            layer: layer(),
            expected: id(3),
            base: version(2),
            attribution: PublishAttribution::Agent {
                context: Some(id(9)),
                allow_missing_trace: false,
            },
        },
        Message::PublishLayer {
            operation: op("publish-3"),
            layer: layer(),
            expected: id(3),
            base: version(2),
            attribution: PublishAttribution::Agent {
                context: None,
                allow_missing_trace: true,
            },
        },
        Message::PublishOutcome(PublishOutcome::Published {
            number: version(4),
            id: id(4),
            tree: id(5),
        }),
        Message::PublishOutcome(PublishOutcome::Conflict {
            paths: vec![
                RepoPath::parse("src/lib.rs").unwrap(),
                RepoPath::parse("a/b/c").unwrap(),
            ],
        }),
        Message::PublishOutcome(PublishOutcome::Conflict { paths: Vec::new() }),
        Message::PublishOutcome(PublishOutcome::LayerStale { head: id(6) }),
        Message::PublishOutcome(PublishOutcome::Refused {
            code: code::MISSING_AGENT_TRACE.into(),
        }),
        Message::PublishRequest {
            operation: op("request-publish-1"),
            request: request(),
            revision: 2,
            base: version(3),
        },
        Message::OpenRequest {
            operation: op("open-1"),
            request: request(),
            layer: layer(),
            checkpoint: id(7),
            base: version(3),
        },
        Message::ReviewOutcome(ReviewOutcome::Accepted { revision: 1 }),
        Message::ReviewOutcome(ReviewOutcome::Refused {
            code: code::NOT_AUTHOR.into(),
        }),
        Message::Revise {
            operation: op("revise-1"),
            request: request(),
            expected_revision: 1,
            checkpoint: id(8),
            base: version(3),
        },
        Message::Approve {
            operation: op("approve-1"),
            request: request(),
            revision: 2,
        },
        Message::Comment {
            operation: op("comment-1"),
            request: request(),
            revision: Some(2),
            body: "looks right".into(),
        },
        Message::Comment {
            operation: op("comment-2"),
            request: request(),
            revision: None,
            body: String::new(),
        },
        Message::ReadRequest { request: request() },
        Message::RequestState {
            request: request(),
            state: RequestStatus::Open,
            revision: 2,
        },
        Message::RequestState {
            request: request(),
            state: RequestStatus::Published,
            revision: 1,
        },
        Message::RequestState {
            request: request(),
            state: RequestStatus::Closed,
            revision: 3,
        },
    ];
    for message in messages {
        roundtrip(message);
    }
}

#[test]
fn checkpoint_kind_round_trips_in_object_messages() {
    let bytes = br#"{"changes":{}}"#.to_vec();
    let checkpoint = Kind::Checkpoint.id(&bytes);
    roundtrip(Message::Want(vec![(Kind::Checkpoint, checkpoint)]));
    roundtrip(Message::Get(vec![(Kind::Checkpoint, checkpoint)]));
    roundtrip(Message::Objects(vec![(
        Kind::Checkpoint,
        checkpoint,
        bytes,
    )]));
}

#[test]
fn checkpoint_kind_hashes_as_its_own_kind() {
    let bytes = br#"{"changes":{}}"#;
    assert_eq!(Kind::Checkpoint.id(bytes), ObjectId::for_checkpoint(bytes));
    assert_ne!(Kind::Checkpoint.id(bytes), Kind::Blob.id(bytes));
    assert_ne!(Kind::Checkpoint.id(bytes), Kind::Tree.id(bytes));
    assert_eq!(Kind::Checkpoint.limit(), MAX_CHECKPOINT);
}

#[test]
fn checkpoint_objects_are_bounded() {
    let at_limit = vec![b'x'; MAX_CHECKPOINT];
    roundtrip(Message::Objects(vec![(
        Kind::Checkpoint,
        Kind::Checkpoint.id(&at_limit),
        at_limit,
    )]));

    let over = vec![b'x'; MAX_CHECKPOINT + 1];
    let oversize = Message::Objects(vec![(Kind::Checkpoint, id(0), over)]);
    assert_eq!(encode(&oversize).unwrap_err().code, code::LIMIT_EXCEEDED);

    let mut body = vec![OBJECTS, 0, 0, 0, 1, 3];
    body.extend_from_slice(id(0).as_bytes());
    body.extend_from_slice(&((MAX_CHECKPOINT + 1) as u32).to_be_bytes());
    assert_eq!(refused(&body), code::LIMIT_EXCEEDED);
}

#[test]
fn unknown_tags_are_malformed() {
    for tag in [0x00, 0x04, 0x05, 0x07, 0x32, 0x3b, 0x4f, 0x57, 0x7e] {
        assert_eq!(refused(&[tag]), code::MALFORMED, "tag {tag:#04x}");
    }
}

#[test]
fn unknown_and_retired_object_kinds_are_malformed() {
    // Kind 2 was Manifest and is retired; 0, 5 and 9 were never assigned.
    for kind in [0u8, 2, 5, 9] {
        let mut body = vec![WANT, 0, 0, 0, 1, kind];
        body.extend_from_slice(id(1).as_bytes());
        assert_eq!(refused(&body), code::MALFORMED, "kind {kind}");
    }
}

#[test]
fn truncated_bodies_are_malformed() {
    let messages = [
        Message::LayerState {
            layer: layer(),
            base: version(3),
            seq: 4,
            head: id(3),
            state: LayerStatus::Active,
        },
        Message::PublishLayer {
            operation: op("publish-1"),
            layer: layer(),
            expected: id(3),
            base: version(2),
            attribution: PublishAttribution::Agent {
                context: Some(id(9)),
                allow_missing_trace: true,
            },
        },
        Message::Comment {
            operation: op("comment-1"),
            request: request(),
            revision: Some(2),
            body: "text".into(),
        },
    ];
    for message in messages {
        let body = encode(&message).unwrap();
        for cut in 1..body.len() {
            assert_eq!(refused(&body[..cut]), code::MALFORMED, "cut {cut}");
        }
    }
}

#[test]
fn trailing_bytes_are_malformed() {
    let mut body = encode(&Message::Permissions).unwrap();
    body.push(0);
    assert_eq!(refused(&body), code::MALFORMED);
}

#[test]
fn frame_boundaries_are_checked() {
    assert_eq!(
        read_frame(&mut frame(&[]).as_slice()).unwrap_err().code,
        code::MALFORMED
    );
    assert_eq!(
        read_frame(&mut [0, 0, 0, 5, 1].as_slice())
            .unwrap_err()
            .code,
        code::TRUNCATED
    );
    let oversize = ((MAX_FRAME + 1) as u32).to_be_bytes();
    assert_eq!(
        read_frame(&mut oversize.as_slice()).unwrap_err().code,
        code::LIMIT_EXCEEDED
    );
    assert_eq!(read_frame(&mut [].as_slice()).unwrap(), None);
}

#[test]
fn absent_optional_ids_must_be_zeroed() {
    // Flag 0 with a non-zero ID payload is a second encoding of "absent".
    let mut body = vec![LAYER_APPEND, 1, b'o', 1, b'l', 0];
    body.extend_from_slice(&[1; 32]);
    body.extend_from_slice(&[2; 32]);
    assert_eq!(refused(&body), code::MALFORMED);

    let mut body = vec![LAYER_APPEND, 1, b'o', 1, b'l', 2];
    body.extend_from_slice(&[0; 32]);
    body.extend_from_slice(&[2; 32]);
    assert_eq!(refused(&body), code::MALFORMED);
}

#[test]
fn invalid_enum_codes_are_malformed() {
    let mut applied = encode(&Message::Applied {
        epoch: 1,
        caller_role: Role::Read,
        visibility: Visibility::Public,
    })
    .unwrap();
    assert_eq!(applied[0], APPLIED);
    let role = applied.len() - 2;
    applied[role] = 9;
    assert_eq!(refused(&applied), code::MALFORMED, "role");
    applied[role] = 1;
    applied[role + 1] = 2;
    assert_eq!(refused(&applied), code::MALFORMED, "visibility");

    let mut state = encode(&Message::LayerState {
        layer: layer(),
        base: version(1),
        seq: 1,
        head: id(1),
        state: LayerStatus::Active,
    })
    .unwrap();
    *state.last_mut().unwrap() = 3;
    assert_eq!(refused(&state), code::MALFORMED, "layer state");

    let mut request_state = encode(&Message::RequestState {
        request: request(),
        state: RequestStatus::Open,
        revision: 1,
    })
    .unwrap();
    assert_eq!(request_state[0], REQUEST_STATE);
    let status = request_state.len() - 5;
    request_state[status] = 3;
    assert_eq!(refused(&request_state), code::MALFORMED, "request state");

    let mut layer_outcome = encode(&Message::LayerOutcome(LayerOutcome::Accepted {
        seq: 1,
        checkpoint: id(1),
    }))
    .unwrap();
    assert_eq!(layer_outcome[0], LAYER_OUTCOME);
    layer_outcome[1] = 3;
    assert_eq!(refused(&layer_outcome), code::MALFORMED, "layer outcome");

    let mut review = encode(&Message::ReviewOutcome(ReviewOutcome::Accepted {
        revision: 1,
    }))
    .unwrap();
    assert_eq!(review[0], REVIEW_OUTCOME);
    review[1] = 2;
    assert_eq!(refused(&review), code::MALFORMED, "review outcome");

    let mut publish = encode(&Message::PublishOutcome(PublishOutcome::LayerStale {
        head: id(1),
    }))
    .unwrap();
    assert_eq!(publish[0], PUBLISH_OUTCOME);
    publish[1] = 4;
    assert_eq!(refused(&publish), code::MALFORMED, "publish outcome");

    let mut attribution = encode(&Message::PublishLayer {
        operation: op("p"),
        layer: layer(),
        expected: id(1),
        base: version(1),
        attribution: PublishAttribution::Human,
    })
    .unwrap();
    assert_eq!(attribution[0], PUBLISH_LAYER);
    *attribution.last_mut().unwrap() = 2;
    assert_eq!(refused(&attribution), code::MALFORMED, "attribution");
}

#[test]
fn request_ids_follow_the_collaboration_rule() {
    // Colons are allowed, as in the collaboration crate's RequestId.
    let colon = RequestId::parse("req:1").unwrap();
    roundtrip(Message::ReadRequest { request: colon });
    assert!(RequestId::parse(&"a".repeat(128)).is_ok());
    assert!(RequestId::parse(&"a".repeat(129)).is_err());
    assert!(RequestId::parse("a/b").is_err());
}

#[test]
fn invalid_identifiers_are_malformed() {
    assert_eq!(refused(&[0x35, 0]), code::MALFORMED, "empty Layer ID");
    let mut body = vec![0x56, 3];
    body.extend_from_slice(b"a b");
    body.push(0);
    body.extend_from_slice(&[0; 4]);
    assert_eq!(refused(&body), code::MALFORMED, "request ID with a space");
    assert!(RequestId::parse("").is_err());
    assert!(RequestId::parse("a b").is_err());
}

#[test]
fn comment_bodies_are_bounded_and_utf8() {
    roundtrip(Message::Comment {
        operation: op("comment-max"),
        request: request(),
        revision: None,
        body: "x".repeat(MAX_COMMENT),
    });

    let over = Message::Comment {
        operation: op("comment-over"),
        request: request(),
        revision: None,
        body: "x".repeat(MAX_COMMENT + 1),
    };
    assert_eq!(encode(&over).unwrap_err().code, code::LIMIT_EXCEEDED);

    let mut body = vec![COMMENT, 1, b'o', 1, b'r', 0, 0, 0, 0, 0];
    body.extend_from_slice(&((MAX_COMMENT + 1) as u32).to_be_bytes());
    body.extend(std::iter::repeat_n(b'x', MAX_COMMENT + 1));
    assert_eq!(refused(&body), code::LIMIT_EXCEEDED);

    let mut body = vec![COMMENT, 1, b'o', 1, b'r', 0, 0, 0, 0, 0, 0, 0, 0, 1, 0xff];
    assert_eq!(refused(&body), code::MALFORMED, "comment is not UTF-8");
    body[5] = 2;
    assert_eq!(refused(&body), code::MALFORMED, "revision flag");
}

#[test]
fn conflict_lists_are_bounded() {
    let paths: Vec<RepoPath> = (0..=MAX_WANT)
        .map(|i| RepoPath::parse(&format!("p{i}")).unwrap())
        .collect();
    let over = Message::PublishOutcome(PublishOutcome::Conflict { paths });
    assert_eq!(encode(&over).unwrap_err().code, code::LIMIT_EXCEEDED);

    // 4096 paths of 4096 bytes is over one frame, so the frame bound refuses it.
    let long: Vec<RepoPath> = (0..MAX_WANT)
        .map(|i| RepoPath::parse(&format!("{i:04}{}", "a".repeat(4092))).unwrap())
        .collect();
    let frame_over = Message::PublishOutcome(PublishOutcome::Conflict { paths: long });
    assert_eq!(encode(&frame_over).unwrap_err().code, code::LIMIT_EXCEEDED);
}

#[test]
fn refusal_codes_are_bounded_text() {
    let long = Message::LayerOutcome(LayerOutcome::Refused {
        code: "x".repeat(256),
    });
    assert_eq!(encode(&long).unwrap_err().code, code::MALFORMED);
}

#[test]
fn refusal_codes_match_the_contract() {
    let spelled = [
        (code::UNSUPPORTED_VERSION, "unsupported_version"),
        (code::UNAUTHORIZED, "unauthorized"),
        (code::FORBIDDEN, "forbidden"),
        (code::STALE_PERMISSIONS, "stale_permissions"),
        (code::OPERATION_ID_REUSED, "operation_id_reused"),
        (code::ID_MISMATCH, "id_mismatch"),
        (code::MISSING_OBJECT, "missing_object"),
        (code::LAYER_STALE, "layer_stale"),
        (code::CONFLICT, "conflict"),
        (code::NOT_OPEN, "not_open"),
        (code::NOT_AUTHOR, "not_author"),
        (code::SELF_APPROVAL, "self_approval"),
        (code::STALE_REVISION, "stale_revision"),
        (code::UNKNOWN_REVISION, "unknown_revision"),
        (code::NOT_ENOUGH_APPROVALS, "not_enough_approvals"),
        (code::MISSING_AGENT_TRACE, "missing_agent_trace"),
        (code::LAYER_PUBLISHED, "layer_published"),
        (code::STORAGE_FULL, "storage_full"),
        (code::NODE_UNAVAILABLE, "node_unavailable"),
        (code::CONTROL_PLANE_UNAVAILABLE, "control_plane_unavailable"),
        (code::MALFORMED, "malformed"),
        (code::LIMIT_EXCEEDED, "limit_exceeded"),
        (code::TRUNCATED, "truncated"),
    ];
    for (constant, text) in spelled {
        assert_eq!(constant, text);
    }
}

#[test]
fn hosting_bodies_encode_at_their_documented_sizes() {
    let applied = encode(&Message::Applied {
        epoch: 1,
        caller_role: Role::Maintain,
        visibility: Visibility::Private,
    })
    .unwrap();
    assert_eq!(applied.len(), 11);
    assert_eq!(applied[0], APPLIED);

    let append = encode(&Message::LayerAppend {
        operation: op("ab"),
        layer: LayerId::parse("cd").unwrap(),
        parent: None,
        checkpoint: id(1),
    })
    .unwrap();
    assert_eq!(append.len(), 1 + 3 + 3 + 33 + 32);
    assert_eq!(append[0], LAYER_APPEND);

    let state = encode(&Message::LayerState {
        layer: layer(),
        base: version(1),
        seq: 1,
        head: id(1),
        state: LayerStatus::Published,
    })
    .unwrap();
    assert_eq!(state.len(), 1 + 1 + 7 + 8 + 8 + 32 + 1);
    assert_eq!(state[0], LAYER_STATE);
}
