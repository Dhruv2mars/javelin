//! Hosting DTOs for the version 2 messages that move Layer, World and Publish Request
//! state. Each enum documents its wire codes.

use javelin::{ObjectId, RepoPath, VersionNumber};

use crate::ProtocolError;

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
    pub(super) fn code(self) -> u8 {
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
    pub(super) fn code(self) -> u8 {
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
    pub(super) fn code(self) -> u8 {
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
    pub(super) fn code(self) -> u8 {
        match self {
            Self::Open => 0,
            Self::Published => 1,
            Self::Closed => 2,
        }
    }
}

/// Publish Request ID: 1..128 ASCII letters, digits, `-`, `_`, `.` or `:`. This is the
/// collaboration crate's `RequestId` rule. The protocol crate cannot depend on that
/// crate, so keep the two in step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestId(String);

impl RequestId {
    pub fn parse(value: &str) -> Result<Self, ProtocolError> {
        let valid = !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b));
        if !valid {
            return Err(ProtocolError::new(
                "malformed",
                "invalid Publish Request ID",
            ));
        }
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
