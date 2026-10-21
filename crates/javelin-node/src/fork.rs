//! Forks with inherited history (`docs/online-design.md` section 4.9, U11). A fork copies the
//! source's World log up to version N into its own repository. Each version is replayed with the
//! source's tree, so the fork's versions carry the source's IDs. The objects those versions need
//! are copied into the fork's own store (D14), so the fork never reads the source again.
//!
//! Agent traces follow the reader (D13): a trace the reader may not see is left out and marked,
//! so its bytes never reach the fork. A restricted trace the reader may see stays restricted in
//! the fork. Roles are not copied.
//!
//! The copy is resumable. Trees and blobs are copied before the versions that name them, each
//! stored tree is complete, and a version is written by one transaction. The origin is written
//! last, so a fork with an origin is finished.

use std::collections::BTreeMap;

use javelin::{
    AdvanceOutcome, ObjectId, OperationId, PublishAttribution, Repository, VersionNumber,
    tree::{Chunk, MAX_CHUNK, Node},
};
use javelin_collaboration::Role;
use javelin_protocol::{
    Message, ProtocolError,
    codec::{PublishOutcome, code, decode},
    receiver::WorldLog,
};
use rusqlite::{OptionalExtension, params};

use crate::{trace::TraceVisibility, world_log::FileWorldLog};

/// The fork's record of where it came from, the traces it left out, and the traces it inherited
/// (with the earliest version that named each), so a fork of this fork inherits them too.
pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS fork_origin (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 0),
    source TEXT NOT NULL,
    number INTEGER NOT NULL,
    version TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS fork_omitted_traces (trace TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS fork_traces (trace TEXT PRIMARY KEY, number INTEGER NOT NULL);";

/// `fork_of`: the source repository, the version forked at, and that version's ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Origin {
    pub source: String,
    pub number: VersionNumber,
    pub version: ObjectId,
}

/// A finished fork. `omitted` lists the traces the reader could not see, each marked in the fork.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Forked {
    pub origin: Origin,
    pub omitted: Vec<ObjectId>,
}

/// Copies `source`'s log up to `upto` into `target`, which must hold no versions past 0 or an
/// earlier, unfinished copy of the same fork. `reader` is the role the forker holds in the
/// source, which decides whether restricted traces may be copied.
pub fn fork(
    source: &FileWorldLog,
    source_name: &str,
    target: &mut FileWorldLog,
    upto: VersionNumber,
    reader: Role,
) -> Result<Forked, ProtocolError> {
    if let Some(origin) = origin(target)? {
        if origin.source == source_name && origin.number == upto {
            return Ok(Forked {
                omitted: omitted_traces(target)?,
                origin,
            });
        }
        return Err(refused(&format!(
            "repository is already forked from {} at {}",
            origin.source, origin.number
        )));
    }
    if upto.get() > source.head()?.number.get() {
        return Err(refused(&format!(
            "fork point {upto} is past the source's head"
        )));
    }

    let start = target.head()?;
    let expected = source.repo().version(start.number)?;
    if start.id != expected.id {
        return Err(refused("target does not match the source at its head"));
    }
    for number in start.number.get() + 1..=upto.get() {
        let version = source.repo().version(VersionNumber::new(number)?)?;
        copy_tree(source.repo(), WorldLog::repository(target), version.tree)?;
        let advanced = WorldLog::repository(target).advance_to_tree(
            &OperationId::parse(&format!("fork-{number}"))?,
            VersionNumber::new(number - 1)?,
            version.tree,
        )?;
        let AdvanceOutcome::Accepted { version: copied } = advanced else {
            return Err(refused("target head moved during the fork"));
        };
        if copied.id != version.id {
            return Err(refused("copied version does not match the source"));
        }
    }

    let omitted = copy_traces(source, target, upto, reader)?;
    let version = source.repo().version(upto)?;
    let origin = Origin {
        source: source_name.into(),
        number: upto,
        version: version.id,
    };
    target
        .records()
        .execute(
            "INSERT INTO fork_origin (singleton, source, number, version) VALUES (0, ?1, ?2, ?3)",
            params![
                origin.source,
                origin.number.get() as i64,
                origin.version.to_string()
            ],
        )
        .map_err(storage)?;
    Ok(Forked { origin, omitted })
}

/// The fork's origin, if it has been forked.
pub fn origin(log: &FileWorldLog) -> Result<Option<Origin>, ProtocolError> {
    let row = log
        .records()
        .query_row(
            "SELECT source, number, version FROM fork_origin WHERE singleton = 0",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(storage)?;
    let Some((source, number, version)) = row else {
        return Ok(None);
    };
    Ok(Some(Origin {
        source,
        number: VersionNumber::new(number as u64)?,
        version: ObjectId::parse(&version)?,
    }))
}

/// The traces a fork left out for the reader, in the order they were found.
pub fn omitted_traces(log: &FileWorldLog) -> Result<Vec<ObjectId>, ProtocolError> {
    let mut statement = log
        .records()
        .prepare("SELECT trace FROM fork_omitted_traces ORDER BY rowid")
        .map_err(storage)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(storage)?;
    let mut traces = Vec::new();
    for row in rows {
        traces.push(ObjectId::parse(&row.map_err(storage)?)?);
    }
    Ok(traces)
}

/// Copies every trace that a publication up to `upto` named. Each trace is copied, marked
/// restricted if it was, or omitted and marked when the reader may not see it. A trace the source
/// already omitted has no bytes to copy, so it stays omitted. Every step is idempotent, so a
/// resumed fork repeats them safely.
fn copy_traces(
    source: &FileWorldLog,
    target: &mut FileWorldLog,
    upto: VersionNumber,
    reader: Role,
) -> Result<Vec<ObjectId>, ProtocolError> {
    let mut omitted = Vec::new();
    for (trace, number) in inherited_traces(source, upto)? {
        target
            .records()
            .execute(
                "INSERT OR IGNORE INTO fork_traces (trace, number) VALUES (?1, ?2)",
                params![trace.to_string(), number.get() as i64],
            )
            .map_err(storage)?;
        let visibility = source.traces().visibility(trace)?;
        let absent = source
            .records()
            .query_row(
                "SELECT 1 FROM fork_omitted_traces WHERE trace = ?1",
                [trace.to_string()],
                |_| Ok(()),
            )
            .optional()
            .map_err(storage)?
            .is_some();
        if absent || (visibility == TraceVisibility::Restricted && reader < Role::Maintain) {
            target
                .records()
                .execute(
                    "INSERT OR IGNORE INTO fork_omitted_traces (trace) VALUES (?1)",
                    [trace.to_string()],
                )
                .map_err(storage)?;
            if !omitted.contains(&trace) {
                omitted.push(trace);
            }
            continue;
        }
        copy_blob(source.repo(), WorldLog::repository(target), trace)?;
        if visibility == TraceVisibility::Restricted {
            target.traces().mark_restricted(trace)?;
        }
    }
    Ok(omitted)
}

/// The traces up to `upto`, each with the earliest version that named it. They come from the
/// source's own `PublishLayer` outcomes and from the traces the source inherited when it was
/// forked. A fork's inherited versions have no outcomes of their own, so only the second kind
/// reaches them.
fn inherited_traces(
    source: &FileWorldLog,
    upto: VersionNumber,
) -> Result<Vec<(ObjectId, VersionNumber)>, ProtocolError> {
    let mut earliest = BTreeMap::new();
    let mut statement = source
        .records()
        .prepare("SELECT request, outcome FROM decided")
        .map_err(storage)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .map_err(storage)?;
    for row in rows {
        let (request, outcome) = row.map_err(storage)?;
        let Message::PublishOutcome(PublishOutcome::Published { number, .. }) = decode(&outcome)?
        else {
            continue;
        };
        if number.get() > upto.get() {
            continue;
        }
        let Message::PublishLayer {
            attribution:
                PublishAttribution::Agent {
                    context: Some(trace),
                    ..
                },
            ..
        } = decode(&request)?
        else {
            continue;
        };
        note(&mut earliest, trace, number.get());
    }
    let mut statement = source
        .records()
        .prepare("SELECT trace, number FROM fork_traces")
        .map_err(storage)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(storage)?;
    for row in rows {
        let (trace, number) = row.map_err(storage)?;
        note(&mut earliest, ObjectId::parse(&trace)?, number as u64);
    }
    earliest
        .into_iter()
        .filter(|(_, number)| *number <= upto.get())
        .map(|(trace, number)| Ok((ObjectId::parse(&trace)?, VersionNumber::new(number)?)))
        .collect()
}

/// Keeps the earliest version that named `trace`.
fn note(earliest: &mut BTreeMap<String, u64>, trace: ObjectId, number: u64) {
    let first = earliest.entry(trace.to_string()).or_insert(number);
    *first = (*first).min(number);
}

/// Copies one tree chunk and everything under it. A chunk already in `to` is complete, so the
/// walk stops there, which keeps a resumed copy incremental.
fn copy_tree(from: &Repository, to: &mut Repository, root: ObjectId) -> Result<(), ProtocolError> {
    if to.has_tree(root)? {
        return Ok(());
    }
    let bytes = from.tree_bytes_bounded(root, MAX_CHUNK)?;
    let chunk = Chunk::decode(&bytes)?;
    for (_, node) in chunk.entries() {
        match node {
            Node::File(entry) => copy_blob(from, to, entry.content())?,
            Node::Dir(child) => copy_tree(from, to, child)?,
        }
    }
    for child in chunk.children() {
        copy_tree(from, to, child)?;
    }
    to.put_tree(&bytes)?;
    Ok(())
}

fn copy_blob(from: &Repository, to: &mut Repository, id: ObjectId) -> Result<(), ProtocolError> {
    if !to.has_blob(id)? {
        to.put_blob(&from.read_blob(id)?)?;
    }
    Ok(())
}

fn refused(detail: &str) -> ProtocolError {
    ProtocolError::new(code::INVALID_INPUT, detail)
}

fn storage(error: rusqlite::Error) -> ProtocolError {
    ProtocolError::new("storage", error.to_string())
}
