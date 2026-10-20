//! The self-hosted World log: one SQLite-backed `Repository` in a data directory, behind
//! an exclusive process lock. The repository's transactions give the ordering: a unique
//! head per version, recorded operations, and a Layer's one successful Publish.

use std::{
    fs::{self, File, OpenOptions, TryLockError},
    path::Path,
};

use javelin::{
    Error, LayerHead, LayerId, LayerState, LayerUpdate, ObjectId, OperationId, PublicationReceipt,
    PublishAttribution, PublishOutcome as NativeOutcome, Repository, Result, SparseChanges,
    VersionNumber, WorldVersion,
};
use javelin_protocol::{
    ProtocolError,
    codec::{PublishOutcome, code},
    receiver::{WorldLog, refusal},
};

/// Held for the node's lifetime. A second node on the same directory is refused.
const LOCK: &str = "node.lock";

pub struct FileWorldLog {
    repo: Repository,
    _lock: File,
}

impl FileWorldLog {
    /// Creates the repository on first use, then takes the directory's lock. The
    /// lock file sits beside `.javelin`, so the repository's directory stays as it is.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        fs::create_dir_all(dir)?;
        let repo = Repository::init(dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(LOCK))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(Error::Busy(format!(
                    "node data directory {} is held by another node",
                    dir.display()
                )));
            }
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
        Ok(Self { repo, _lock: lock })
    }

    /// Read access to the repository, for checks and inspection.
    pub fn repo(&self) -> &Repository {
        &self.repo
    }

    pub fn head(&self) -> Result<WorldVersion> {
        self.repo.head()
    }

    pub fn publication(&self, operation: &OperationId) -> Result<Option<PublicationReceipt>> {
        self.repo.publication(operation)
    }

    pub fn layer_state(&self, layer: &LayerId) -> Result<LayerState> {
        self.repo.layer_state(layer)
    }

    /// Creates an empty Layer over World `base`. Retrying the same ID and base returns
    /// the same head.
    pub fn create_layer(&mut self, layer: &LayerId, base: VersionNumber) -> Result<LayerHead> {
        let base = self.repo.layer_base(base)?;
        self.repo.create_layer(layer, &base)
    }

    /// Records `changes` as the Layer's next checkpoint. This seeds the node's Layer
    /// state until `LayerAppend` carries checkpoints over the wire.
    pub fn append(
        &mut self,
        operation: &OperationId,
        layer: &LayerId,
        changes: &SparseChanges,
    ) -> Result<LayerHead> {
        let head = match self.repo.layer_state(layer)? {
            LayerState::Active { head } => head,
            other => {
                return Err(Error::InvalidInput(format!(
                    "Layer {} is not active: {other:?}",
                    layer.as_str()
                )));
            }
        };
        match self.repo.capture_layer(operation, &head, changes)? {
            LayerUpdate::Applied {
                state: LayerState::Active { head },
            } => Ok(head),
            other => Err(Error::InvalidInput(format!(
                "append did not apply: {other:?}"
            ))),
        }
    }

    /// One `PublishLayer`. A recorded operation replays its outcome before the Layer is
    /// read, so a retry after a lost reply succeeds even once the Layer is Published.
    fn publish(
        &mut self,
        operation: &OperationId,
        layer: &LayerId,
        expected: ObjectId,
        base: VersionNumber,
        attribution: &PublishAttribution,
    ) -> std::result::Result<PublishOutcome, ProtocolError> {
        let head = match self.repo.publication(operation)? {
            // A recorded operation keeps the head it was prepared on. Only the same
            // input replays; anything else reuses the ID.
            Some(receipt) => {
                let same = receipt.expected.layer() == layer
                    && receipt.expected.checkpoint().object() == expected
                    && receipt.attribution == *attribution;
                if !same {
                    return Ok(refused(code::OPERATION_ID_REUSED));
                }
                receipt.expected
            }
            None => match self.repo.layer_state(layer) {
                Err(Error::InvalidInput(_)) => return Ok(refused(refusal::UNKNOWN_LAYER)),
                state => match state? {
                    LayerState::Active { head } if head.checkpoint().object() == expected => head,
                    LayerState::Active { head } => {
                        return Ok(PublishOutcome::LayerStale {
                            head: head.checkpoint().object(),
                        });
                    }
                    LayerState::Published { .. } => return Ok(refused(code::LAYER_PUBLISHED)),
                    LayerState::Discarded { .. } => {
                        return Ok(refused(refusal::LAYER_DISCARDED));
                    }
                },
            },
        };
        if self
            .repo
            .read_checkpoint(head.checkpoint())?
            .base()
            .version()
            != base
        {
            return Ok(refused(code::INVALID_INPUT));
        }
        match self
            .repo
            .publish_layer(operation, &head, attribution, |_, _| Ok(()))
        {
            Ok(receipt) => outcome(receipt.outcome),
            Err(error) => match error.code() {
                "operation_id_reused"
                | "missing_agent_trace"
                | "layer_published"
                | "missing_object"
                | "invalid_input" => Ok(refused(error.code())),
                _ => Err(error.into()),
            },
        }
    }
}

impl WorldLog for FileWorldLog {
    fn repository(&mut self) -> &mut Repository {
        &mut self.repo
    }

    fn publish_layer(
        &mut self,
        operation: &OperationId,
        layer: &LayerId,
        expected: ObjectId,
        base: VersionNumber,
        attribution: &PublishAttribution,
    ) -> std::result::Result<PublishOutcome, ProtocolError> {
        self.publish(operation, layer, expected, base, attribution)
    }
}

/// Maps the repository's outcome to the wire. A Layer that is no longer Active has
/// no head to be stale against, so it is refused as published or discarded.
fn outcome(outcome: NativeOutcome) -> std::result::Result<PublishOutcome, ProtocolError> {
    Ok(match outcome {
        NativeOutcome::Published { version } => PublishOutcome::Published {
            number: version.number,
            id: version.id,
            tree: version.tree,
        },
        NativeOutcome::Conflict { paths } => PublishOutcome::Conflict { paths },
        NativeOutcome::LayerStale { actual } => match actual {
            LayerState::Active { head } => PublishOutcome::LayerStale {
                head: head.checkpoint().object(),
            },
            LayerState::Published { .. } => refused(code::LAYER_PUBLISHED),
            LayerState::Discarded { .. } => refused(refusal::LAYER_DISCARDED),
        },
    })
}

fn refused(code: &str) -> PublishOutcome {
    PublishOutcome::Refused { code: code.into() }
}
