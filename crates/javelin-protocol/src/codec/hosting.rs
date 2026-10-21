//! Hosting DTOs for the version 2 messages that move Layer, World and Publish Request
//! state. Each enum documents its wire codes.

use javelin::{ObjectId, OperationId, RepoPath, VersionNumber};

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

/// Completed check conclusion. Wire codes follow the GitHub check-run set, minus the two
/// values slice 1 excludes (`action_required`, and `stale`, which only GitHub sets).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Conclusion {
    Success,
    Failure,
    Neutral,
    Cancelled,
    TimedOut,
    Skipped,
}

impl Conclusion {
    pub fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Failure => 1,
            Self::Neutral => 2,
            Self::Cancelled => 3,
            Self::TimedOut => 4,
            Self::Skipped => 5,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Self::Success,
            1 => Self::Failure,
            2 => Self::Neutral,
            3 => Self::Cancelled,
            4 => Self::TimedOut,
            5 => Self::Skipped,
            _ => return None,
        })
    }

    /// The name used by the CLI and in JSON, matching GitHub's spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Neutral => "neutral",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Skipped => "skipped",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        [
            Self::Success,
            Self::Failure,
            Self::Neutral,
            Self::Cancelled,
            Self::TimedOut,
            Self::Skipped,
        ]
        .into_iter()
        .find(|conclusion| conclusion.as_str() == name)
    }
}

/// `CheckReport` (0x60): one completed result for a tree. `repository` is the repository ID
/// the reporter addresses; the node refuses any other. Optional text is absent or nonempty;
/// an empty string is malformed on both encode and decode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckReport {
    pub operation: OperationId,
    pub repository: String,
    pub subject_tree: ObjectId,
    pub subject_checkpoint: Option<ObjectId>,
    pub name: String,
    pub conclusion: Conclusion,
    pub details_url: Option<String>,
    pub summary: Option<String>,
    pub started_at: u64,
    pub completed_at: u64,
}

/// Reply to `CheckReport` (0x61). A replay of the same operation returns `Accepted` with the
/// original `id` and `seq`. Name and subject refusals are outcomes, not protocol errors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckOutcome {
    Accepted { id: ObjectId, seq: u64 },
    Refused { code: String },
}
