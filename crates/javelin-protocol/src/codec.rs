//! Version 2 wire format: `u32` big-endian body length, then a body whose first
//! byte is the message tag. Integers are big-endian; IDs are raw 32-byte values.
//! Snapshots travel as content-addressed tree chunks and blobs, root first.
//!
//! Hosting messages (tags 0x03, 0x06, 0x33-0x3a, 0x50-0x56) move Layer, World and
//! Publish Request state. Each variant of [`Message`] documents its fields. Encodings
//! are canonical: an absent optional is a zero flag followed by zeroed bytes, and any
//! other form of absence is malformed, so one message has one encoding.

use std::io::{self, Read, Write};

use javelin::{
    LayerId, MAX_PATH, ObjectId, OperationId, PublishAttribution, RepoPath, VersionNumber,
    tree::Chunk,
};

use crate::{ProtocolError, limits::*};

pub const MAGIC: &[u8; 4] = b"JVLP";
pub const VERSION: u16 = 2;

const HELLO: u8 = 0x01;
const HELLO_OK: u8 = 0x02;
const PERMISSIONS: u8 = 0x03;
const APPLIED: u8 = 0x06;
const WANT: u8 = 0x10;
const MISSING: u8 = 0x11;
const GET: u8 = 0x12;
const OBJECTS: u8 = 0x20;
const STORED: u8 = 0x22;
const PUBLISH: u8 = 0x30;
const OUTCOME: u8 = 0x31;
const LAYER_OUTCOME: u8 = 0x33;
const LAYER_APPEND: u8 = 0x34;
const LAYER_HEAD: u8 = 0x35;
const LAYER_STATE: u8 = 0x36;
const LAYER_STACK: u8 = 0x37;
const PUBLISH_LAYER: u8 = 0x38;
const PUBLISH_OUTCOME: u8 = 0x39;
const PUBLISH_REQUEST: u8 = 0x3a;
const RESOLVE: u8 = 0x40;
const RESOLVED: u8 = 0x41;
const OPEN_REQUEST: u8 = 0x50;
const REVIEW_OUTCOME: u8 = 0x51;
const REVISE: u8 = 0x52;
const APPROVE: u8 = 0x53;
const COMMENT: u8 = 0x54;
const READ_REQUEST: u8 = 0x55;
const REQUEST_STATE: u8 = 0x56;
const ERROR: u8 = 0x7f;

/// Refusal codes that appear in `Error` frames and in hosting outcomes. Two codes
/// are client-side only and never cross the wire: `node_unavailable` (provider or
/// network failure) and `control_plane_unavailable`. They stay distinct from every
/// node refusal, so callers can tell "the node refused" from "the node was not reached".
pub mod code {
    pub const MALFORMED: &str = "malformed";
    pub const LIMIT_EXCEEDED: &str = "limit_exceeded";
    pub const TRUNCATED: &str = "truncated";
    pub const INVALID_INPUT: &str = "invalid_input";
    pub const UNSUPPORTED_VERSION: &str = "unsupported_version";
    pub const UNAUTHORIZED: &str = "unauthorized";
    pub const FORBIDDEN: &str = "forbidden";
    pub const STALE_PERMISSIONS: &str = "stale_permissions";
    pub const OPERATION_ID_REUSED: &str = "operation_id_reused";
    pub const ID_MISMATCH: &str = "id_mismatch";
    pub const MISSING_OBJECT: &str = "missing_object";
    pub const LAYER_STALE: &str = "layer_stale";
    pub const CONFLICT: &str = "conflict";
    pub const NOT_OPEN: &str = "not_open";
    pub const NOT_AUTHOR: &str = "not_author";
    pub const SELF_APPROVAL: &str = "self_approval";
    pub const STALE_REVISION: &str = "stale_revision";
    pub const UNKNOWN_REVISION: &str = "unknown_revision";
    pub const NOT_ENOUGH_APPROVALS: &str = "not_enough_approvals";
    pub const MISSING_AGENT_TRACE: &str = "missing_agent_trace";
    pub const LAYER_PUBLISHED: &str = "layer_published";
    pub const STORAGE_FULL: &str = "storage_full";
    pub const NODE_UNAVAILABLE: &str = "node_unavailable";
    pub const CONTROL_PLANE_UNAVAILABLE: &str = "control_plane_unavailable";
}

/// Object IDs are kind-separated; every reference names the expected kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Kind {
    Blob = 1,
    /// Layer checkpoint: canonical changeset JSON. Bytes are the object.
    Checkpoint = 3,
    Tree = 4,
}

impl Kind {
    /// Largest encoded object of this kind.
    pub fn limit(self) -> usize {
        match self {
            Self::Blob => MAX_BLOB,
            Self::Checkpoint => MAX_CHECKPOINT,
            Self::Tree => MAX_TREE,
        }
    }
    /// The object ID of `bytes` as this kind.
    pub fn id(self, bytes: &[u8]) -> ObjectId {
        match self {
            Self::Blob => ObjectId::for_blob(bytes),
            Self::Checkpoint => ObjectId::for_checkpoint(bytes),
            Self::Tree => ObjectId::for_tree(bytes),
        }
    }
}

/// Wire DTO for the native `AdvanceOutcome`. Built only from validated wire values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    Accepted {
        number: VersionNumber,
        tree: ObjectId,
        operation: OperationId,
    },
    Stale {
        actual: VersionNumber,
    },
}

impl Outcome {
    /// Adapts a native outcome of advancing to `tree`. Accepted native versions
    /// always carry their operation and root.
    pub fn from_native(outcome: &javelin::AdvanceOutcome) -> Result<Self, ProtocolError> {
        match outcome {
            javelin::AdvanceOutcome::Accepted { version } => Ok(Self::Accepted {
                number: version.number,
                tree: version.tree,
                operation: version.operation.clone().ok_or_else(|| {
                    ProtocolError::new("invalid_input", "accepted World lacks operation")
                })?,
            }),
            javelin::AdvanceOutcome::Stale { actual } => Ok(Self::Stale { actual: *actual }),
        }
    }
}

/// Caller's role at the applied epoch. Wire codes: 1 Read, 2 Review, 3 Maintain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    /// Read, comment, and author Publish Requests.
    Read,
    /// Also approve other authors' revisions.
    Review,
    /// Also Publish.
    Maintain,
}

impl Role {
    fn code(self) -> u8 {
        match self {
            Self::Read => 1,
            Self::Review => 2,
            Self::Maintain => 3,
        }
    }
}

/// Repository visibility. Wire codes: 0 Public, 1 Private.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Visibility {
    Public,
    Private,
}

impl Visibility {
    fn code(self) -> u8 {
        match self {
            Self::Public => 0,
            Self::Private => 1,
        }
    }
}

/// Layer state as the node reports it. Wire codes: 0 Active, 1 Published, 2 Discarded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayerStatus {
    Active,
    Published,
    Discarded,
}

impl LayerStatus {
    fn code(self) -> u8 {
        match self {
            Self::Active => 0,
            Self::Published => 1,
            Self::Discarded => 2,
        }
    }
}

/// Publish Request state. Wire codes: 0 Open, 1 Published, 2 Closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestStatus {
    Open,
    Published,
    Closed,
}

impl RequestStatus {
    fn code(self) -> u8 {
        match self {
            Self::Open => 0,
            Self::Published => 1,
            Self::Closed => 2,
        }
    }
}

/// Publish Request ID: 1..128 ASCII letters, digits, `-`, `_` or `.`, as operation IDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestId(String);

impl RequestId {
    pub fn parse(value: &str) -> Result<Self, ProtocolError> {
        OperationId::parse(value)?;
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Reply to `LayerAppend` and `LayerStack`. A replayed `LayerAppend` returns the
/// original `seq`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LayerOutcome {
    Accepted {
        seq: u64,
        checkpoint: ObjectId,
    },
    /// The Layer head moved; `head` is the checkpoint the node now holds.
    Stale {
        head: ObjectId,
    },
    Refused {
        code: String,
    },
}

/// Reply to `PublishLayer` and `PublishRequest`. A replay of a successful Publish
/// returns `Published` with the original version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishOutcome {
    Published {
        number: VersionNumber,
        id: ObjectId,
        tree: ObjectId,
    },
    /// Paths that intersect the World advance. Nothing is written.
    Conflict {
        paths: Vec<RepoPath>,
    },
    LayerStale {
        head: ObjectId,
    },
    Refused {
        code: String,
    },
}

/// Reply to `OpenRequest`, `Revise`, `Approve` and `Comment`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewOutcome {
    Accepted { revision: u32 },
    Refused { code: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    Hello {
        min: u16,
        max: u16,
    },
    HelloOk {
        version: u16,
    },
    /// Ask which permissions apply to the caller. Reply: `Applied`.
    Permissions,
    /// The applied epoch, the caller's role, and the repository's visibility.
    Applied {
        epoch: u64,
        caller_role: Role,
        visibility: Visibility,
    },
    /// Push: which of these does the receiver lack?
    Want(Vec<(Kind, ObjectId)>),
    Missing(Vec<(Kind, ObjectId)>),
    /// Fetch: send these objects.
    Get(Vec<(Kind, ObjectId)>),
    /// Object bytes, each bounded by its kind's limit. Push sends children before
    /// parents; a fetch reply answers a prefix of its `Get`.
    Objects(Vec<(Kind, ObjectId, Vec<u8>)>),
    Stored {
        count: u32,
    },
    Publish {
        operation: OperationId,
        expected: VersionNumber,
        tree: ObjectId,
    },
    Outcome(Outcome),
    /// Append checkpoint `checkpoint` to a Layer. `parent` must equal the current
    /// head, or the reply is `Stale`.
    LayerAppend {
        operation: OperationId,
        layer: LayerId,
        parent: Option<ObjectId>,
        checkpoint: ObjectId,
    },
    LayerOutcome(LayerOutcome),
    /// Read a Layer's head and state.
    LayerHead {
        layer: LayerId,
    },
    /// Reply to `LayerHead`. `seq` is the head's sequence number.
    LayerState {
        layer: LayerId,
        base: VersionNumber,
        seq: u64,
        head: ObjectId,
        state: LayerStatus,
    },
    /// Stack `source` onto `target`, which must still be at `target_head`.
    LayerStack {
        operation: OperationId,
        target: LayerId,
        target_head: ObjectId,
        source: ObjectId,
    },
    /// Publish a whole Layer as the next World version, over `base`.
    PublishLayer {
        operation: OperationId,
        layer: LayerId,
        expected: ObjectId,
        base: VersionNumber,
        attribution: PublishAttribution,
    },
    PublishOutcome(PublishOutcome),
    /// Publish revision `revision` of a Request, over `base`.
    PublishRequest {
        operation: OperationId,
        request: RequestId,
        revision: u32,
        base: VersionNumber,
    },
    /// Fetch: the version to read, or the head.
    Resolve {
        version: Option<VersionNumber>,
    },
    Resolved {
        number: VersionNumber,
        id: ObjectId,
        tree: ObjectId,
    },
    /// Open a Publish Request whose first revision is `checkpoint` on `layer`.
    OpenRequest {
        operation: OperationId,
        request: RequestId,
        layer: LayerId,
        checkpoint: ObjectId,
        base: VersionNumber,
    },
    ReviewOutcome(ReviewOutcome),
    /// Author adds revision `expected_revision + 1` with `checkpoint`.
    Revise {
        operation: OperationId,
        request: RequestId,
        expected_revision: u32,
        checkpoint: ObjectId,
        base: VersionNumber,
    },
    Approve {
        operation: OperationId,
        request: RequestId,
        revision: u32,
    },
    /// `body` is at most `MAX_COMMENT` bytes of UTF-8. `revision: None` comments on
    /// the request as a whole.
    Comment {
        operation: OperationId,
        request: RequestId,
        revision: Option<u32>,
        body: String,
    },
    ReadRequest {
        request: RequestId,
    },
    /// Reply to `ReadRequest`: the request's state and its latest revision number.
    RequestState {
        request: RequestId,
        state: RequestStatus,
        revision: u32,
    },
    Error {
        code: String,
        detail: String,
    },
}

/// Encodes one body after checking every count, length and the total against the
/// version 2 limits, so an oversize message is refused before its buffer exists.
pub fn encode(message: &Message) -> Result<Vec<u8>, ProtocolError> {
    let len = encoded_len(message)?;
    let mut out = Vec::with_capacity(len);
    match message {
        Message::Hello { min, max } => {
            out.push(HELLO);
            out.extend_from_slice(MAGIC);
            out.extend_from_slice(&min.to_be_bytes());
            out.extend_from_slice(&max.to_be_bytes());
        }
        Message::HelloOk { version } => {
            out.push(HELLO_OK);
            out.extend_from_slice(&version.to_be_bytes());
        }
        Message::Permissions => out.push(PERMISSIONS),
        Message::Applied {
            epoch,
            caller_role,
            visibility,
        } => {
            out.push(APPLIED);
            out.extend_from_slice(&epoch.to_be_bytes());
            out.push(caller_role.code());
            out.push(visibility.code());
        }
        Message::Want(items) | Message::Missing(items) | Message::Get(items) => {
            out.push(match message {
                Message::Want(_) => WANT,
                Message::Missing(_) => MISSING,
                _ => GET,
            });
            put_u32(&mut out, items.len());
            for (kind, id) in items {
                out.push(*kind as u8);
                put_id(&mut out, id);
            }
        }
        Message::Objects(objects) => {
            out.push(OBJECTS);
            put_u32(&mut out, objects.len());
            for (kind, id, bytes) in objects {
                out.push(*kind as u8);
                put_id(&mut out, id);
                put_u32(&mut out, bytes.len());
                out.extend_from_slice(bytes);
            }
        }
        Message::Stored { count } => {
            out.push(STORED);
            out.extend_from_slice(&count.to_be_bytes());
        }
        Message::Publish {
            operation,
            expected,
            tree,
        } => {
            out.push(PUBLISH);
            put_short(&mut out, operation.as_str());
            out.extend_from_slice(&expected.get().to_be_bytes());
            put_id(&mut out, tree);
        }
        Message::Outcome(Outcome::Accepted {
            number,
            tree,
            operation,
        }) => {
            out.extend_from_slice(&[OUTCOME, 0]);
            out.extend_from_slice(&number.get().to_be_bytes());
            put_id(&mut out, tree);
            put_short(&mut out, operation.as_str());
        }
        Message::Outcome(Outcome::Stale { actual }) => {
            out.extend_from_slice(&[OUTCOME, 1]);
            out.extend_from_slice(&actual.get().to_be_bytes());
        }
        Message::LayerAppend {
            operation,
            layer,
            parent,
            checkpoint,
        } => {
            out.push(LAYER_APPEND);
            put_short(&mut out, operation.as_str());
            put_short(&mut out, layer.as_str());
            put_optional_id(&mut out, parent.as_ref());
            put_id(&mut out, checkpoint);
        }
        Message::LayerOutcome(outcome) => {
            out.push(LAYER_OUTCOME);
            match outcome {
                LayerOutcome::Accepted { seq, checkpoint } => {
                    out.push(0);
                    out.extend_from_slice(&seq.to_be_bytes());
                    put_id(&mut out, checkpoint);
                }
                LayerOutcome::Stale { head } => {
                    out.push(1);
                    put_id(&mut out, head);
                }
                LayerOutcome::Refused { code } => {
                    out.push(2);
                    put_short(&mut out, code);
                }
            }
        }
        Message::LayerHead { layer } => {
            out.push(LAYER_HEAD);
            put_short(&mut out, layer.as_str());
        }
        Message::LayerState {
            layer,
            base,
            seq,
            head,
            state,
        } => {
            out.push(LAYER_STATE);
            put_short(&mut out, layer.as_str());
            out.extend_from_slice(&base.get().to_be_bytes());
            out.extend_from_slice(&seq.to_be_bytes());
            put_id(&mut out, head);
            out.push(state.code());
        }
        Message::LayerStack {
            operation,
            target,
            target_head,
            source,
        } => {
            out.push(LAYER_STACK);
            put_short(&mut out, operation.as_str());
            put_short(&mut out, target.as_str());
            put_id(&mut out, target_head);
            put_id(&mut out, source);
        }
        Message::PublishLayer {
            operation,
            layer,
            expected,
            base,
            attribution,
        } => {
            out.push(PUBLISH_LAYER);
            put_short(&mut out, operation.as_str());
            put_short(&mut out, layer.as_str());
            put_id(&mut out, expected);
            out.extend_from_slice(&base.get().to_be_bytes());
            put_attribution(&mut out, attribution);
        }
        Message::PublishOutcome(outcome) => {
            out.push(PUBLISH_OUTCOME);
            match outcome {
                PublishOutcome::Published { number, id, tree } => {
                    out.push(0);
                    out.extend_from_slice(&number.get().to_be_bytes());
                    put_id(&mut out, id);
                    put_id(&mut out, tree);
                }
                PublishOutcome::Conflict { paths } => {
                    out.push(1);
                    put_u32(&mut out, paths.len());
                    for path in paths {
                        put_long(&mut out, path.as_str());
                    }
                }
                PublishOutcome::LayerStale { head } => {
                    out.push(2);
                    put_id(&mut out, head);
                }
                PublishOutcome::Refused { code } => {
                    out.push(3);
                    put_short(&mut out, code);
                }
            }
        }
        Message::PublishRequest {
            operation,
            request,
            revision,
            base,
        } => {
            out.push(PUBLISH_REQUEST);
            put_short(&mut out, operation.as_str());
            put_short(&mut out, request.as_str());
            out.extend_from_slice(&revision.to_be_bytes());
            out.extend_from_slice(&base.get().to_be_bytes());
        }
        Message::Resolve { version } => {
            out.push(RESOLVE);
            out.push(u8::from(version.is_some()));
            out.extend_from_slice(&version.map_or(0, VersionNumber::get).to_be_bytes());
        }
        Message::Resolved { number, id, tree } => {
            out.push(RESOLVED);
            out.extend_from_slice(&number.get().to_be_bytes());
            put_id(&mut out, id);
            put_id(&mut out, tree);
        }
        Message::OpenRequest {
            operation,
            request,
            layer,
            checkpoint,
            base,
        } => {
            out.push(OPEN_REQUEST);
            put_short(&mut out, operation.as_str());
            put_short(&mut out, request.as_str());
            put_short(&mut out, layer.as_str());
            put_id(&mut out, checkpoint);
            out.extend_from_slice(&base.get().to_be_bytes());
        }
        Message::ReviewOutcome(outcome) => {
            out.push(REVIEW_OUTCOME);
            match outcome {
                ReviewOutcome::Accepted { revision } => {
                    out.push(0);
                    out.extend_from_slice(&revision.to_be_bytes());
                }
                ReviewOutcome::Refused { code } => {
                    out.push(1);
                    put_short(&mut out, code);
                }
            }
        }
        Message::Revise {
            operation,
            request,
            expected_revision,
            checkpoint,
            base,
        } => {
            out.push(REVISE);
            put_short(&mut out, operation.as_str());
            put_short(&mut out, request.as_str());
            out.extend_from_slice(&expected_revision.to_be_bytes());
            put_id(&mut out, checkpoint);
            out.extend_from_slice(&base.get().to_be_bytes());
        }
        Message::Approve {
            operation,
            request,
            revision,
        } => {
            out.push(APPROVE);
            put_short(&mut out, operation.as_str());
            put_short(&mut out, request.as_str());
            out.extend_from_slice(&revision.to_be_bytes());
        }
        Message::Comment {
            operation,
            request,
            revision,
            body,
        } => {
            out.push(COMMENT);
            put_short(&mut out, operation.as_str());
            put_short(&mut out, request.as_str());
            put_optional_u32(&mut out, *revision);
            put_u32(&mut out, body.len());
            out.extend_from_slice(body.as_bytes());
        }
        Message::ReadRequest { request } => {
            out.push(READ_REQUEST);
            put_short(&mut out, request.as_str());
        }
        Message::RequestState {
            request,
            state,
            revision,
        } => {
            out.push(REQUEST_STATE);
            put_short(&mut out, request.as_str());
            out.push(state.code());
            out.extend_from_slice(&revision.to_be_bytes());
        }
        Message::Error { code, detail } => {
            out.push(ERROR);
            put_short(&mut out, code);
            let detail = truncate_utf8(detail, MAX_DETAIL);
            out.extend_from_slice(&(detail.len() as u16).to_be_bytes());
            out.extend_from_slice(detail.as_bytes());
        }
    }
    // `Error` reserves its detail bound, so its exact length may be shorter.
    debug_assert!(out.len() <= len, "encoded length exceeds its bound");
    Ok(out)
}

/// Upper bound on the encoded body of `message`, checked against the limits.
fn encoded_len(message: &Message) -> Result<usize, ProtocolError> {
    let len = match message {
        Message::Hello { .. } => 9,
        Message::HelloOk { .. } => 3,
        Message::Permissions => 1,
        Message::Applied { .. } => 11,
        Message::Want(items) | Message::Missing(items) | Message::Get(items) => {
            if items.len() > MAX_WANT {
                return Err(limit("count exceeds limit"));
            }
            5 + items.len() * 33
        }
        Message::Objects(objects) => {
            if objects.len() > MAX_OBJECTS {
                return Err(limit("count exceeds limit"));
            }
            let mut len = 5usize;
            for (kind, _, bytes) in objects {
                if bytes.len() > kind.limit() {
                    return Err(limit("object exceeds size limit"));
                }
                len = len.saturating_add(OBJECT_OVERHEAD + bytes.len());
            }
            len
        }
        Message::Stored { .. } => 5,
        Message::Publish { operation, .. } => 41 + short(operation.as_str())?,
        Message::Outcome(Outcome::Accepted { operation, .. }) => 42 + short(operation.as_str())?,
        Message::Outcome(Outcome::Stale { .. }) => 10,
        Message::LayerAppend {
            operation, layer, ..
        } => 1 + short(operation.as_str())? + short(layer.as_str())? + 33 + 32,
        Message::LayerOutcome(outcome) => match outcome {
            LayerOutcome::Accepted { .. } => 42,
            LayerOutcome::Stale { .. } => 34,
            LayerOutcome::Refused { code } => 2 + short(code)?,
        },
        Message::LayerHead { layer } => 1 + short(layer.as_str())?,
        Message::LayerState { layer, .. } => 1 + short(layer.as_str())? + 8 + 8 + 32 + 1,
        Message::LayerStack {
            operation, target, ..
        } => 1 + short(operation.as_str())? + short(target.as_str())? + 64,
        Message::PublishLayer {
            operation,
            layer,
            attribution,
            ..
        } => {
            let attribution = match attribution {
                PublishAttribution::Human => 1,
                PublishAttribution::Agent { .. } => 1 + 33 + 1,
            };
            1 + short(operation.as_str())? + short(layer.as_str())? + 32 + 8 + attribution
        }
        Message::PublishOutcome(outcome) => match outcome {
            PublishOutcome::Published { .. } => 74,
            PublishOutcome::Conflict { paths } => {
                if paths.len() > MAX_WANT {
                    return Err(limit("count exceeds limit"));
                }
                let mut len = 6usize;
                for path in paths {
                    len = len.saturating_add(2 + path.as_str().len());
                }
                len
            }
            PublishOutcome::LayerStale { .. } => 34,
            PublishOutcome::Refused { code } => 2 + short(code)?,
        },
        Message::PublishRequest {
            operation, request, ..
        } => 1 + short(operation.as_str())? + short(request.as_str())? + 4 + 8,
        Message::Resolve { .. } => 10,
        Message::Resolved { .. } => 73,
        Message::OpenRequest {
            operation,
            request,
            layer,
            ..
        } => {
            1 + short(operation.as_str())?
                + short(request.as_str())?
                + short(layer.as_str())?
                + 32
                + 8
        }
        Message::ReviewOutcome(outcome) => match outcome {
            ReviewOutcome::Accepted { .. } => 6,
            ReviewOutcome::Refused { code } => 2 + short(code)?,
        },
        Message::Revise {
            operation, request, ..
        } => 1 + short(operation.as_str())? + short(request.as_str())? + 4 + 32 + 8,
        Message::Approve {
            operation, request, ..
        } => 1 + short(operation.as_str())? + short(request.as_str())? + 4,
        Message::Comment {
            operation,
            request,
            body,
            ..
        } => {
            if body.len() > MAX_COMMENT {
                return Err(limit("comment exceeds limit"));
            }
            1 + short(operation.as_str())? + short(request.as_str())? + 5 + 4 + body.len()
        }
        Message::ReadRequest { request } => 1 + short(request.as_str())?,
        Message::RequestState { request, .. } => 1 + short(request.as_str())? + 1 + 4,
        Message::Error { code, .. } => {
            if code.len() > u8::MAX as usize {
                return Err(malformed("error code too long"));
            }
            4 + code.len() + MAX_DETAIL
        }
    };
    if len > MAX_FRAME {
        return Err(limit("frame exceeds size limit"));
    }
    Ok(len)
}

/// Encoded bytes of one `Objects` item besides its payload: kind, ID and length.
pub const OBJECT_OVERHEAD: usize = 37;

/// Decodes one complete frame body. Every count and length is checked against its
/// fixed limit, and every field against the remaining bytes, before it is copied.
/// Tree chunks must be canonically encoded. Unknown tags and kinds are malformed.
pub fn decode(body: &[u8]) -> Result<Message, ProtocolError> {
    if body.len() > MAX_FRAME {
        return Err(limit("frame exceeds size limit"));
    }
    let mut r = Reader(body);
    let message = match r.u8()? {
        HELLO => {
            if r.take(4)? != MAGIC {
                return Err(malformed("bad protocol magic"));
            }
            let (min, max) = (r.u16()?, r.u16()?);
            if min > max {
                return Err(malformed("empty version range"));
            }
            Message::Hello { min, max }
        }
        HELLO_OK => Message::HelloOk { version: r.u16()? },
        PERMISSIONS => Message::Permissions,
        APPLIED => Message::Applied {
            epoch: r.u64()?,
            caller_role: r.role()?,
            visibility: r.visibility()?,
        },
        tag @ (WANT | MISSING | GET) => {
            let n = r.count(MAX_WANT)?;
            let mut items = Vec::new();
            for _ in 0..n {
                items.push((r.kind()?, r.id()?));
            }
            match tag {
                WANT => Message::Want(items),
                MISSING => Message::Missing(items),
                _ => Message::Get(items),
            }
        }
        OBJECTS => {
            let n = r.count(MAX_OBJECTS)?;
            let mut objects = Vec::new();
            for _ in 0..n {
                let kind = r.kind()?;
                let id = r.id()?;
                let len = r.u32()? as usize;
                if len > kind.limit() {
                    return Err(limit("object exceeds size limit"));
                }
                let bytes = r.take(len)?;
                if kind == Kind::Tree {
                    Chunk::decode(bytes).map_err(|e| malformed(&e.to_string()))?;
                }
                objects.push((kind, id, bytes.to_vec()));
            }
            Message::Objects(objects)
        }
        STORED => Message::Stored { count: r.u32()? },
        PUBLISH => Message::Publish {
            operation: r.operation()?,
            expected: r.version()?,
            tree: r.id()?,
        },
        OUTCOME => match r.u8()? {
            0 => Message::Outcome(Outcome::Accepted {
                number: r.version()?,
                tree: r.id()?,
                operation: r.operation()?,
            }),
            1 => Message::Outcome(Outcome::Stale {
                actual: r.version()?,
            }),
            _ => return Err(malformed("unknown outcome")),
        },
        LAYER_APPEND => Message::LayerAppend {
            operation: r.operation()?,
            layer: r.layer()?,
            parent: r.optional_id()?,
            checkpoint: r.id()?,
        },
        LAYER_OUTCOME => Message::LayerOutcome(match r.u8()? {
            0 => LayerOutcome::Accepted {
                seq: r.u64()?,
                checkpoint: r.id()?,
            },
            1 => LayerOutcome::Stale { head: r.id()? },
            2 => LayerOutcome::Refused {
                code: r.short_str()?.to_owned(),
            },
            _ => return Err(malformed("unknown layer outcome")),
        }),
        LAYER_HEAD => Message::LayerHead { layer: r.layer()? },
        LAYER_STATE => Message::LayerState {
            layer: r.layer()?,
            base: r.version()?,
            seq: r.u64()?,
            head: r.id()?,
            state: r.layer_status()?,
        },
        LAYER_STACK => Message::LayerStack {
            operation: r.operation()?,
            target: r.layer()?,
            target_head: r.id()?,
            source: r.id()?,
        },
        PUBLISH_LAYER => Message::PublishLayer {
            operation: r.operation()?,
            layer: r.layer()?,
            expected: r.id()?,
            base: r.version()?,
            attribution: r.attribution()?,
        },
        PUBLISH_OUTCOME => Message::PublishOutcome(match r.u8()? {
            0 => PublishOutcome::Published {
                number: r.version()?,
                id: r.id()?,
                tree: r.id()?,
            },
            1 => {
                let n = r.count(MAX_WANT)?;
                let mut paths = Vec::new();
                for _ in 0..n {
                    paths.push(r.path()?);
                }
                PublishOutcome::Conflict { paths }
            }
            2 => PublishOutcome::LayerStale { head: r.id()? },
            3 => PublishOutcome::Refused {
                code: r.short_str()?.to_owned(),
            },
            _ => return Err(malformed("unknown publish outcome")),
        }),
        PUBLISH_REQUEST => Message::PublishRequest {
            operation: r.operation()?,
            request: r.request()?,
            revision: r.u32()?,
            base: r.version()?,
        },
        RESOLVE => {
            let flag = r.u8()?;
            let version = r.version()?;
            Message::Resolve {
                version: match flag {
                    0 if version.get() == 0 => None,
                    1 => Some(version),
                    _ => return Err(malformed("invalid version selector")),
                },
            }
        }
        RESOLVED => Message::Resolved {
            number: r.version()?,
            id: r.id()?,
            tree: r.id()?,
        },
        OPEN_REQUEST => Message::OpenRequest {
            operation: r.operation()?,
            request: r.request()?,
            layer: r.layer()?,
            checkpoint: r.id()?,
            base: r.version()?,
        },
        REVIEW_OUTCOME => Message::ReviewOutcome(match r.u8()? {
            0 => ReviewOutcome::Accepted { revision: r.u32()? },
            1 => ReviewOutcome::Refused {
                code: r.short_str()?.to_owned(),
            },
            _ => return Err(malformed("unknown review outcome")),
        }),
        REVISE => Message::Revise {
            operation: r.operation()?,
            request: r.request()?,
            expected_revision: r.u32()?,
            checkpoint: r.id()?,
            base: r.version()?,
        },
        APPROVE => Message::Approve {
            operation: r.operation()?,
            request: r.request()?,
            revision: r.u32()?,
        },
        COMMENT => Message::Comment {
            operation: r.operation()?,
            request: r.request()?,
            revision: r.optional_u32()?,
            body: r.text32(MAX_COMMENT)?,
        },
        READ_REQUEST => Message::ReadRequest {
            request: r.request()?,
        },
        REQUEST_STATE => Message::RequestState {
            request: r.request()?,
            state: r.request_status()?,
            revision: r.u32()?,
        },
        ERROR => {
            let code = r.short_str()?.to_owned();
            let len = r.u16()? as usize;
            if len > MAX_DETAIL {
                return Err(limit("error detail exceeds limit"));
            }
            let detail = std::str::from_utf8(r.take(len)?)
                .map_err(|_| malformed("error detail is not UTF-8"))?
                .to_owned();
            Message::Error { code, detail }
        }
        _ => return Err(malformed("unknown message tag")),
    };
    if !r.0.is_empty() {
        return Err(malformed("trailing bytes after message"));
    }
    Ok(message)
}

/// Reads one frame. `Ok(None)` is a clean end of stream at a frame boundary.
pub fn read_frame(r: &mut impl Read) -> Result<Option<Message>, ProtocolError> {
    let mut header = [0; 4];
    let mut filled = 0;
    while filled < 4 {
        match r.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(truncated()),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let len = u32::from_be_bytes(header) as usize;
    if len == 0 {
        return Err(malformed("empty frame"));
    }
    if len > MAX_FRAME {
        return Err(limit("frame exceeds size limit"));
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            truncated()
        } else {
            e.into()
        }
    })?;
    decode(&body).map(Some)
}

/// Writes one frame. Oversize messages are refused before encoding.
pub fn write_frame(w: &mut impl Write, message: &Message) -> Result<(), ProtocolError> {
    let body = encode(message)?;
    w.write_all(&(body.len() as u32).to_be_bytes())?;
    w.write_all(&body)?;
    w.flush()?;
    Ok(())
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        if n > self.0.len() {
            return Err(malformed("message ends inside a field"));
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, ProtocolError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn version(&mut self) -> Result<VersionNumber, ProtocolError> {
        let value = self.u64()?;
        Ok(VersionNumber::new(value)?)
    }
    fn id(&mut self) -> Result<ObjectId, ProtocolError> {
        Ok(ObjectId::from_bytes(self.take(32)?.try_into().unwrap()))
    }
    fn kind(&mut self) -> Result<Kind, ProtocolError> {
        match self.u8()? {
            1 => Ok(Kind::Blob),
            3 => Ok(Kind::Checkpoint),
            4 => Ok(Kind::Tree),
            _ => Err(malformed("unknown object kind")),
        }
    }
    /// Checks a count against its fixed limit. Collections grow only as elements are
    /// actually decoded, so memory stays proportional to the received frame.
    fn count(&mut self, max: usize) -> Result<usize, ProtocolError> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(limit("count exceeds limit"));
        }
        Ok(n)
    }
    fn short_str(&mut self) -> Result<&'a str, ProtocolError> {
        let len = self.u8()? as usize;
        std::str::from_utf8(self.take(len)?).map_err(|_| malformed("text is not UTF-8"))
    }
    fn operation(&mut self) -> Result<OperationId, ProtocolError> {
        Ok(OperationId::parse(self.short_str()?)?)
    }
    fn layer(&mut self) -> Result<LayerId, ProtocolError> {
        LayerId::parse(self.short_str()?).map_err(|_| malformed("invalid Layer ID"))
    }
    fn request(&mut self) -> Result<RequestId, ProtocolError> {
        RequestId::parse(self.short_str()?).map_err(|_| malformed("invalid request ID"))
    }
    /// One canonical flag byte: 0 or 1, nothing else.
    fn flag(&mut self) -> Result<bool, ProtocolError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(malformed("invalid flag")),
        }
    }
    fn optional_id(&mut self) -> Result<Option<ObjectId>, ProtocolError> {
        let present = self.flag()?;
        let id = self.id()?;
        match present {
            true => Ok(Some(id)),
            false if id == ABSENT_ID => Ok(None),
            false => Err(malformed("absent ID must be zeroed")),
        }
    }
    fn optional_u32(&mut self) -> Result<Option<u32>, ProtocolError> {
        let present = self.flag()?;
        let value = self.u32()?;
        match present {
            true => Ok(Some(value)),
            false if value == 0 => Ok(None),
            false => Err(malformed("absent number must be zero")),
        }
    }
    fn role(&mut self) -> Result<Role, ProtocolError> {
        match self.u8()? {
            1 => Ok(Role::Read),
            2 => Ok(Role::Review),
            3 => Ok(Role::Maintain),
            _ => Err(malformed("unknown role")),
        }
    }
    fn visibility(&mut self) -> Result<Visibility, ProtocolError> {
        match self.u8()? {
            0 => Ok(Visibility::Public),
            1 => Ok(Visibility::Private),
            _ => Err(malformed("unknown visibility")),
        }
    }
    fn layer_status(&mut self) -> Result<LayerStatus, ProtocolError> {
        match self.u8()? {
            0 => Ok(LayerStatus::Active),
            1 => Ok(LayerStatus::Published),
            2 => Ok(LayerStatus::Discarded),
            _ => Err(malformed("unknown Layer state")),
        }
    }
    fn request_status(&mut self) -> Result<RequestStatus, ProtocolError> {
        match self.u8()? {
            0 => Ok(RequestStatus::Open),
            1 => Ok(RequestStatus::Published),
            2 => Ok(RequestStatus::Closed),
            _ => Err(malformed("unknown request state")),
        }
    }
    fn attribution(&mut self) -> Result<PublishAttribution, ProtocolError> {
        match self.u8()? {
            0 => Ok(PublishAttribution::Human),
            1 => Ok(PublishAttribution::Agent {
                context: self.optional_id()?,
                allow_missing_trace: self.flag()?,
            }),
            _ => Err(malformed("unknown attribution")),
        }
    }
    /// A repository path: `u16` length, then UTF-8 that parses as a `RepoPath`.
    fn path(&mut self) -> Result<RepoPath, ProtocolError> {
        let len = self.u16()? as usize;
        if len > MAX_PATH {
            return Err(limit("path exceeds limit"));
        }
        let text =
            std::str::from_utf8(self.take(len)?).map_err(|_| malformed("path is not UTF-8"))?;
        RepoPath::parse(text).map_err(|_| malformed("invalid repository path"))
    }
    /// A comment body: `u32` length, bounded by `max`, then UTF-8.
    fn text32(&mut self, max: usize) -> Result<String, ProtocolError> {
        let len = self.u32()? as usize;
        if len > max {
            return Err(limit("comment exceeds limit"));
        }
        std::str::from_utf8(self.take(len)?)
            .map(str::to_owned)
            .map_err(|_| malformed("comment is not UTF-8"))
    }
}

/// The all-zero ID that stands for absence in an optional ID field.
const ABSENT_ID: ObjectId = ObjectId::from_bytes([0; 32]);

fn put_u32(out: &mut Vec<u8>, n: usize) {
    out.extend_from_slice(&(n as u32).to_be_bytes());
}

/// One-byte length, then text. Callers check the length first with `short`.
fn put_short(out: &mut Vec<u8>, text: &str) {
    out.push(text.len() as u8);
    out.extend_from_slice(text.as_bytes());
}

/// `u16` length, then text. Used for paths.
fn put_long(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u16).to_be_bytes());
    out.extend_from_slice(text.as_bytes());
}

fn put_id(out: &mut Vec<u8>, id: &ObjectId) {
    out.extend_from_slice(id.as_bytes());
}

fn put_optional_id(out: &mut Vec<u8>, id: Option<&ObjectId>) {
    out.push(u8::from(id.is_some()));
    put_id(out, id.unwrap_or(&ABSENT_ID));
}

fn put_optional_u32(out: &mut Vec<u8>, value: Option<u32>) {
    out.push(u8::from(value.is_some()));
    out.extend_from_slice(&value.unwrap_or(0).to_be_bytes());
}

fn put_attribution(out: &mut Vec<u8>, attribution: &PublishAttribution) {
    match attribution {
        PublishAttribution::Human => out.push(0),
        PublishAttribution::Agent {
            context,
            allow_missing_trace,
        } => {
            out.push(1);
            put_optional_id(out, context.as_ref());
            out.push(u8::from(*allow_missing_trace));
        }
    }
}

/// Encoded size of a one-byte-length text field, refusing text that does not fit.
fn short(text: &str) -> Result<usize, ProtocolError> {
    if text.len() > u8::MAX as usize {
        return Err(malformed("text exceeds 255 bytes"));
    }
    Ok(1 + text.len())
}

fn truncate_utf8(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn malformed(detail: &str) -> ProtocolError {
    ProtocolError::new(code::MALFORMED, detail)
}

fn limit(detail: &str) -> ProtocolError {
    ProtocolError::new(code::LIMIT_EXCEEDED, detail)
}

fn truncated() -> ProtocolError {
    ProtocolError::new(code::TRUNCATED, "stream ended inside a frame")
}
