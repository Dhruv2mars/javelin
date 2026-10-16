//! Native immutable repository storage and ordered World publication metadata.
//! Sparse Layer state is available. Linux binds it to ordinary paths through sparse
//! OverlayFS views; autosave, other platforms and full Publish are later work.

mod domain;
mod layer;
#[cfg(unix)]
mod linux_layer;
mod repository;

pub use domain::{FileEntry, Manifest, ObjectId, OperationId, RepoPath, VersionNumber};
pub use layer::{
    CheckpointId, LayerBase, LayerCheckpoint, LayerHead, LayerId, LayerState, LayerUpdate,
    PathDiff, SparseChanges,
};
#[cfg(unix)]
pub use linux_layer::{
    CaptureOutcome, LayerMountHelper, LinuxLayerRuntime, RecoveryReport, ViewStatus,
};
pub use repository::{AdvanceOutcome, IntegrityReport, Repository, WorldVersion};

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
    #[error("World version {0} does not exist")]
    MissingVersion(VersionNumber),
    #[error("corrupt repository: {0}")]
    CorruptRepository(String),
    #[error("unsupported repository format {0}")]
    UnsupportedFormat(i64),
    #[error("operation ID {0} was already used with different input")]
    OperationIdReused(OperationId),
    #[error(
        "storage error: {0}; if publication was attempted, reopen and retry the same operation ID and input"
    )]
    Storage(#[from] rusqlite::Error),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("busy: {0}; nothing changed, retry after writers close")]
    Busy(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotInitialized(_) => "not_initialized",
            Self::InvalidInput(_) => "invalid_input",
            Self::MissingObject(_) => "missing_object",
            Self::CorruptObject(_) => "corrupt_object",
            Self::WrongObjectKind(_) => "wrong_object_kind",
            Self::MissingVersion(_) => "missing_version",
            Self::CorruptRepository(_) => "corrupt_repository",
            Self::UnsupportedFormat(_) => "unsupported_format",
            Self::OperationIdReused(_) => "operation_id_reused",
            Self::Storage(_) => "storage_error",
            Self::Io(_) => "io_error",
            Self::Busy(_) => "busy",
            Self::Unsupported(_) => "unsupported",
        }
    }
}
