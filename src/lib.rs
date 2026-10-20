//! Native immutable repository storage, Layers, Stack and whole-Layer Publish.
//! Layers are plain directories cloned from an immutable World base and captured
//! without a daemon (docs/workspace-approach.md).

mod adopt;
mod blame;
pub mod capture;
mod domain;
mod ignore;
mod layer;
mod publish;
mod radar;
mod repository;
mod stack;
pub mod tree;
mod workspace;

pub use adopt::Adoption;
pub use blame::{Actor, BlameLine, LineBlame, Origin, PathBlame, Provenance};
pub use capture::Capture;
pub use domain::{
    FileEntry, MAX_DEPTH, MAX_PATH, Manifest, ObjectId, OperationId, RepoPath, SparseChanges,
    VersionNumber,
};
pub use ignore::IgnoreRules;
pub use layer::{
    CheckpointId, LayerBase, LayerCheckpoint, LayerHead, LayerId, LayerState, LayerUpdate, PathDiff,
};
pub use publish::{PublicationReceipt, PublishAttribution, PublishOutcome, PublishWarning};
pub use radar::{LayerCollision, PathPair, Radar, WorldCollisions};
pub use repository::{AdvanceOutcome, IntegrityReport, Repository, WorldVersion};
pub use workspace::{
    BaseReport, BaseSource, CloneReport, CloneStrategy, LayerWorkspace, WorkspaceDiscard,
    WorkspaceRecovery, WorkspaceRefresh, WorkspaceReport,
};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("repository is not initialized at {0}")]
    NotInitialized(std::path::PathBuf),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("missing object {0}")]
    MissingObject(ObjectId),
    #[error("corrupt object {0}")]
    CorruptObject(ObjectId),
    #[error("object {0} has the wrong kind")]
    WrongObjectKind(ObjectId),
    #[error("object {id} has {size} bytes, exceeding read limit {limit}")]
    ObjectTooLarge {
        id: ObjectId,
        size: usize,
        limit: usize,
    },
    #[error("World version {0} does not exist")]
    MissingVersion(VersionNumber),
    #[error("corrupt repository: {0}")]
    CorruptRepository(String),
    #[error("unsupported repository format {0}")]
    UnsupportedFormat(i64),
    #[error("Layer {layer:?} was published by operation {operation}")]
    LayerPublished {
        layer: LayerId,
        operation: OperationId,
    },
    #[error("Agent publication requires a context Blob or explicit missing-trace override")]
    MissingAgentTrace,
    #[error("operation ID {0} was already used with different input")]
    OperationIdReused(OperationId),
    #[error(
        "storage error: {0}; if publication was attempted, reopen and retry the same operation ID and input"
    )]
    Storage(rusqlite::Error),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("busy: {0}; nothing changed, retry after writers close")]
    Busy(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error(
        "local edits conflict with the head at {}; nothing was changed. Resolve these paths, then refresh again",
        .0.join(", ")
    )]
    RefreshConflict(Vec<String>),
}

/// Lock contention is `busy`: SQLite rolls back the refused transaction, so nothing
/// changed and a retry is safe. A damaged database or undecodable stored text is
/// `corrupt_repository`, since retrying cannot help. Other SQLite failures stay
/// `storage_error`, which tells the caller a Publish may have been attempted.
impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        match error.sqlite_error_code() {
            Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
                Self::Busy(error.to_string())
            }
            Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase) => {
                Self::CorruptRepository(error.to_string())
            }
            // A header that names a format this repository never uses (WAL) is damage.
            _ if error.to_string().contains("unsupported file format") => {
                Self::CorruptRepository(error.to_string())
            }
            // The schema is fixed: a missing column or table means it was altered.
            _ if error.to_string().starts_with("no such column")
                || error.to_string().starts_with("no such table") =>
            {
                Self::CorruptRepository(error.to_string())
            }
            _ if matches!(error, rusqlite::Error::Utf8Error(..)) => {
                Self::CorruptRepository(error.to_string())
            }
            _ => Self::Storage(error),
        }
    }
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotInitialized(_) => "not_initialized",
            Self::InvalidInput(_) => "invalid_input",
            Self::MissingObject(_) => "missing_object",
            Self::CorruptObject(_) => "corrupt_object",
            Self::WrongObjectKind(_) => "wrong_object_kind",
            Self::ObjectTooLarge { .. } => "object_too_large",
            Self::MissingVersion(_) => "missing_version",
            Self::CorruptRepository(_) => "corrupt_repository",
            Self::UnsupportedFormat(_) => "unsupported_format",
            Self::LayerPublished { .. } => "layer_published",
            Self::MissingAgentTrace => "missing_agent_trace",
            Self::OperationIdReused(_) => "operation_id_reused",
            Self::Storage(_) => "storage_error",
            Self::Io(_) => "io_error",
            Self::Busy(_) => "busy",
            Self::Unsupported(_) => "unsupported",
            Self::RefreshConflict(_) => "refresh_conflict",
        }
    }
}
