//! The node's World log: one SQLite-backed `Repository` and the node's records, from the
//! provider's `Storage`, behind its `Coordination` guard. The repository's transactions give
//! the ordering: a unique head per version, recorded publications, and a Layer's one
//! successful Publish. Every decided `PublishLayer` is also recorded in `node.db`, so a retry
//! replays it even when the Layer has moved on since.

use std::any::Any;

use javelin::{
    Error, LayerHead, LayerId, LayerState, LayerUpdate, ObjectId, OperationId, PublicationReceipt,
    PublishAttribution, PublishOutcome as NativeOutcome, Repository, Result, SparseChanges,
    VersionNumber, WorldVersion,
};
use javelin_protocol::{
    Message, ProtocolError,
    codec::{LayerOutcome, PublishOutcome, code, decode, encode},
    receiver::{WorldLog, refusal},
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    checks,
    review::is_review_layer,
    storage::{Coordination, Storage},
    trace::Traces,
};

/// A decided request's bytes, and the outcome it was decided with.
type Decided = (Vec<u8>, Vec<u8>);

pub struct FileWorldLog {
    repo: Repository,
    /// Decided `PublishLayer` requests and their outcomes, keyed by operation ID, and the
    /// other partitions' rows (`node.db`).
    records: Connection,
    _guard: Box<dyn Any + Send>,
}

impl FileWorldLog {
    /// The self-hosted node on data directory `dir`: creates the repository on first use,
    /// then takes the directory's lock.
    #[cfg(feature = "native")]
    pub fn open(dir: impl AsRef<std::path::Path>) -> Result<Self> {
        let dir = crate::storage::Directory::new(dir);
        Self::on(&dir, &dir)
    }

    /// The node on `storage`: creates the repository on first use, then takes the ordering
    /// point from `coordination` for the node's lifetime.
    pub fn on(storage: &dyn Storage, coordination: &dyn Coordination) -> Result<Self> {
        let repo = storage.repository()?;
        let guard = coordination.acquire()?;
        let records = storage.records()?;
        records.execute_batch(
            "CREATE TABLE IF NOT EXISTS decided (
                operation TEXT PRIMARY KEY,
                request BLOB NOT NULL,
                outcome BLOB NOT NULL
            );",
        )?;
        records.execute_batch(crate::fork::SCHEMA)?;
        records.execute_batch(crate::layer::SCHEMA)?;
        records.execute_batch(crate::lease::SCHEMA)?;
        records.execute_batch(crate::checks::SCHEMA)?;
        records.execute_batch(crate::trace::SCHEMA)?;
        Ok(Self {
            repo,
            records,
            _guard: guard,
        })
    }

    /// Read access to the repository, for checks and inspection.
    pub fn repo(&self) -> &Repository {
        &self.repo
    }

    /// The repository and the node's records, for partitions that share this node's
    /// storage (the review partition in `review.rs`).
    pub(crate) fn parts(&mut self) -> (&mut Repository, &mut Connection) {
        (&mut self.repo, &mut self.records)
    }

    /// The node's `node.db`, shared with the trace and fork stores.
    pub(crate) fn records(&self) -> &Connection {
        &self.records
    }

    /// The trace store, which keeps its visibility beside the decided operations.
    pub fn traces(&self) -> Traces<'_> {
        Traces::new(&self.records)
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

    /// One `PublishLayer`. A decided operation replays its recorded outcome when the
    /// request is identical, and is refused as reused otherwise. Only an undecided
    /// operation reads the Layer, so a retry after a lost reply is answered the same way
    /// however the Layer has changed since.
    fn publish(
        &mut self,
        operation: &OperationId,
        layer: &LayerId,
        expected: ObjectId,
        base: VersionNumber,
        attribution: &PublishAttribution,
    ) -> std::result::Result<PublishOutcome, ProtocolError> {
        let request = encode(&Message::PublishLayer {
            operation: operation.clone(),
            layer: layer.clone(),
            expected,
            base,
            attribution: attribution.clone(),
        })?;
        if let Some((recorded, outcome)) = self.decided(operation)? {
            return if recorded == request {
                replay(&outcome)
            } else {
                Ok(refused(code::OPERATION_ID_REUSED))
            };
        }
        let outcome = match self.repo.publication(operation)? {
            // Published before the record was written, when the node stopped between the
            // two writes. The library's receipt decides, and is recorded from here on.
            Some(receipt) => {
                if !self.receipt_matches(&receipt, layer, expected, base, attribution)? {
                    return Ok(refused(code::OPERATION_ID_REUSED));
                }
                outcome(receipt.outcome)?
            }
            None => self.decide(operation, layer, expected, base, attribution)?,
        };
        self.record(operation, &request, &outcome)?;
        Ok(outcome)
    }

    /// Decides an undecided operation against the Layer as it is now.
    fn decide(
        &mut self,
        operation: &OperationId,
        layer: &LayerId,
        expected: ObjectId,
        base: VersionNumber,
        attribution: &PublishAttribution,
    ) -> std::result::Result<PublishOutcome, ProtocolError> {
        let head = match self.repo.layer_state(layer) {
            Err(Error::InvalidInput(_)) => return Ok(refused(refusal::UNKNOWN_LAYER)),
            state => match state? {
                LayerState::Active { head } if head.checkpoint().object() == expected => head,
                LayerState::Active { head } => {
                    return Ok(PublishOutcome::LayerStale {
                        head: head.checkpoint().object(),
                    });
                }
                LayerState::Published { .. } => return Ok(refused(code::LAYER_PUBLISHED)),
                LayerState::Discarded { .. } => return Ok(refused(refusal::LAYER_DISCARDED)),
            },
        };
        if self.base_of(&head)? != base {
            return Ok(refused(code::INVALID_INPUT));
        }
        // No reporter-revocation check here: PublishLayer has no applied permissions until
        // its path is authenticated. PublishRequest checks them (review.rs).
        let (repo, records) = self.parts();
        if let Some(refusal) = checks::gate(records, repo, head.checkpoint(), None)? {
            return Ok(refused(refusal));
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

    fn base_of(&self, head: &LayerHead) -> Result<VersionNumber> {
        Ok(self
            .repo
            .read_checkpoint(head.checkpoint())?
            .base()
            .version())
    }

    fn receipt_matches(
        &self,
        receipt: &PublicationReceipt,
        layer: &LayerId,
        expected: ObjectId,
        base: VersionNumber,
        attribution: &PublishAttribution,
    ) -> Result<bool> {
        Ok(receipt.expected.layer() == layer
            && receipt.expected.checkpoint().object() == expected
            && receipt.attribution == *attribution
            && self.base_of(&receipt.expected)? == base)
    }

    fn decided(
        &self,
        operation: &OperationId,
    ) -> std::result::Result<Option<Decided>, ProtocolError> {
        self.records
            .query_row(
                "SELECT request, outcome FROM decided WHERE operation = ?1",
                [operation.as_str()],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()
            .map_err(storage)
    }

    fn record(
        &self,
        operation: &OperationId,
        request: &[u8],
        outcome: &PublishOutcome,
    ) -> std::result::Result<(), ProtocolError> {
        let bytes = encode(&Message::PublishOutcome(outcome.clone()))?;
        self.records
            .execute(
                "INSERT INTO decided (operation, request, outcome) VALUES (?1, ?2, ?3)",
                params![operation.as_str(), request, bytes],
            )
            .map_err(storage)?;
        Ok(())
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
        // A revision publishes only through PublishRequest, so its approvals gate it.
        if is_review_layer(layer) {
            return Ok(refused(refusal::LAYER_RESERVED));
        }
        self.publish(operation, layer, expected, base, attribution)
    }

    fn has_changes(&mut self, id: ObjectId) -> std::result::Result<bool, ProtocolError> {
        crate::layer::has_changes(&self.records, id)
    }

    fn changes_visible(
        &mut self,
        id: ObjectId,
        reader: &javelin::Reader,
    ) -> std::result::Result<bool, ProtocolError> {
        crate::layer::changes_visible(&self.repo, &self.records, id, reader)
    }

    fn store_changes(
        &mut self,
        id: ObjectId,
        bytes: &[u8],
    ) -> std::result::Result<(), ProtocolError> {
        crate::layer::store_changes(&self.records, id, bytes)
    }

    fn append_checkpoint(
        &mut self,
        operation: &OperationId,
        layer: &LayerId,
        parent: Option<ObjectId>,
        changes: ObjectId,
        writer: &javelin::Reader,
    ) -> std::result::Result<LayerOutcome, ProtocolError> {
        if is_review_layer(layer) {
            return Ok(LayerOutcome::Refused {
                code: refusal::LAYER_RESERVED.into(),
            });
        }
        crate::layer::append(
            &mut self.repo,
            &self.records,
            operation,
            layer,
            parent,
            changes,
            Some(writer),
        )
    }

    fn layer_head(&mut self, layer: &LayerId) -> std::result::Result<Message, ProtocolError> {
        crate::layer::layer_head(&self.repo, layer)
    }
}

/// Maps the repository's outcome to the wire. A Layer that is no longer Active has
/// no head to be stale against, so it is refused as published or discarded.
pub(crate) fn outcome(
    outcome: NativeOutcome,
) -> std::result::Result<PublishOutcome, ProtocolError> {
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

fn replay(bytes: &[u8]) -> std::result::Result<PublishOutcome, ProtocolError> {
    match decode(bytes)? {
        Message::PublishOutcome(outcome) => Ok(outcome),
        _ => Err(storage_detail("recorded outcome is not a publish outcome")),
    }
}

pub(crate) fn refused(code: &str) -> PublishOutcome {
    PublishOutcome::Refused { code: code.into() }
}

fn storage(error: rusqlite::Error) -> ProtocolError {
    storage_detail(&error.to_string())
}

fn storage_detail(detail: &str) -> ProtocolError {
    ProtocolError::new("storage", detail)
}
