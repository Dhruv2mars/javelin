//! Agent traces at the node. A trace is a Blob in the repository, named by its content ID, so
//! its bytes are stored and checked like any other object. Its visibility is recorded here. An
//! inherited trace follows the repository's read rule (D25). A restricted trace is also refused
//! to every caller below Maintain. Restriction is one-way and survives a restart, so a trace
//! never gains broader access than it had when it was restricted.
//!
//! `missing_agent_trace` is decided by the repository's Publish, which refuses an agent
//! attribution with neither a context nor the override (`A§Publish`). The node reports that
//! refusal in `world_log.rs`, so it is not repeated here.
//!
//! No wire message carries traces yet: section 3 of `docs/online-design.md` names none. The
//! fetch path is guarded by `refuse_wire_fetch`, because `Get` would otherwise return a
//! restricted trace's bytes to anyone who knows its ID.

use std::path::Path;

use javelin::{ObjectId, Repository};
use javelin_collaboration::Role;
use javelin_identity::{Need, Scope};
use javelin_protocol::{
    Message, ProtocolError,
    codec::{Kind, code},
    limits::MAX_BLOB,
};
use rusqlite::{Connection, OptionalExtension};

use crate::auth::Gate;

/// Restricted traces, by object ID. A trace absent from this table inherits.
const RESTRICTED: &str = "restricted_traces";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceVisibility {
    /// Readable wherever the repository is readable (D25).
    Inherit,
    /// Readable by Maintain only.
    Restricted,
}

pub struct Traces {
    db: Connection,
}

impl Traces {
    /// Opens the trace store in `path`, which is the node's `node.db`.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let db = Connection::open(path)?;
        db.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS {RESTRICTED} (trace TEXT PRIMARY KEY);"
        ))?;
        Ok(Self { db })
    }

    pub fn visibility(&self, trace: ObjectId) -> Result<TraceVisibility, ProtocolError> {
        let restricted = self
            .db
            .query_row(
                "SELECT 1 FROM restricted_traces WHERE trace = ?1",
                [trace.to_string()],
                |_| Ok(()),
            )
            .optional()
            .map_err(storage)?
            .is_some();
        Ok(if restricted {
            TraceVisibility::Restricted
        } else {
            TraceVisibility::Inherit
        })
    }

    /// Restricts a stored trace to Maintain. Needs the Publish scope, which only Maintain
    /// holds. Restricting an already restricted trace changes nothing.
    pub fn restrict(
        &self,
        gate: &Gate,
        repo: &Repository,
        token: Option<&str>,
        now: u64,
        trace: ObjectId,
    ) -> Result<(), ProtocolError> {
        gate.check(
            token,
            now,
            Need {
                scope: Scope::Publish,
                layer: None,
            },
        )?;
        if !repo.has_blob(trace)? {
            return Err(ProtocolError::new(
                code::MISSING_OBJECT,
                format!("trace {trace} is not stored"),
            ));
        }
        self.mark_restricted(trace)
    }

    /// Records a stored trace as restricted, with no gate check. The fork copies a restriction
    /// this way, since the forker's right to see the trace was checked when it was copied.
    pub(crate) fn mark_restricted(&self, trace: ObjectId) -> Result<(), ProtocolError> {
        self.db
            .execute(
                "INSERT OR IGNORE INTO restricted_traces (trace) VALUES (?1)",
                [trace.to_string()],
            )
            .map_err(storage)?;
        Ok(())
    }

    /// Reads a trace for the caller. The gate applies the repository's read rule. A restricted
    /// trace also needs the Maintain role.
    pub fn read(
        &self,
        gate: &Gate,
        repo: &Repository,
        token: Option<&str>,
        now: u64,
        trace: ObjectId,
    ) -> Result<Vec<u8>, ProtocolError> {
        let grant = gate.check(
            token,
            now,
            Need {
                scope: Scope::Read,
                layer: None,
            },
        )?;
        if self.visibility(trace)? == TraceVisibility::Restricted && grant.role < Role::Maintain {
            return Err(restricted());
        }
        Ok(repo.read_blob_bounded(trace, MAX_BLOB)?)
    }

    /// Refuses a fetch that names a restricted trace. The wire carries no caller identity yet,
    /// so every fetch is anonymous, and an anonymous caller never reads a restricted trace.
    pub fn refuse_wire_fetch(&self, message: &Message) -> Result<(), ProtocolError> {
        let Message::Get(items) = message else {
            return Ok(());
        };
        for (kind, trace) in items {
            if *kind == Kind::Blob && self.visibility(*trace)? == TraceVisibility::Restricted {
                return Err(restricted());
            }
        }
        Ok(())
    }
}

fn restricted() -> ProtocolError {
    ProtocolError::new(
        code::FORBIDDEN,
        "restricted traces are readable by Maintain only",
    )
}

fn storage(error: rusqlite::Error) -> ProtocolError {
    ProtocolError::new("storage", error.to_string())
}
