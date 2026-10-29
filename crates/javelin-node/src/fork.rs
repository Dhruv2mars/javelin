//! Forks with inherited history (`docs/online-design.md` section 4.9, U11). A fork copies the
//! source's World log up to version N into its own repository. Each version is replayed with the
//! source's tree, so the fork's versions carry the source's IDs. The objects those versions need
//! are copied into the fork's own store (D14), so the fork never reads the source again.
//!
//! Agent traces follow the reader (D13): a trace the reader may not see is left out and marked,
//! so its bytes never reach the fork. A restricted trace the reader may see stays restricted in
//! the fork. Roles are not copied.
//!
//! Hidden entries follow the reader too. Committed trees are copied whole, so every hidden
//! entry keeps its commitment and the fork's version IDs match the source's. An entry's opening
//! and content are copied only when the reader is in its audience; otherwise the fork holds the
//! commitment alone and its view leaves the entry out.
//!
//! A fork is planned from the source first (`plan`): the versions, the objects the reader may
//! read and the inherited traces, before anything is written. The plan is then applied to the
//! target (`apply`), which refuses it before its first write when the target does not match the
//! source or the plan would cross the target's budget (`budget::Room::fork`). On one node the
//! apply reads the source's store directly (`fork_for`). Across nodes, such as two Durable
//! Objects, the source exports the plan with its bytes (`export`) and the target applies the
//! decoded `Bundle`; a hidden entry the reader may not read leaves the source as its
//! commitment alone.
//!
//! The copy is resumable. Trees and blobs are copied before the versions that name them, each
//! stored tree is complete, and a version is written by one transaction. The origin is written
//! last, so a fork with an origin is finished. A Durable Object applies a bundle in one
//! transaction, so its target is untouched or finished.

use std::collections::{BTreeMap, HashMap, HashSet};

use javelin::{
    AdvanceOutcome, Audience, FileEntry, ObjectId, Opening, OperationId, PublishAttribution,
    Reader, Repository, SALT_BYTES, SealedKind, VersionNumber,
    tree::{Chunk, MAX_CHUNK, Node},
};
use javelin_collaboration::Role;
use javelin_protocol::{
    Message, ProtocolError,
    codec::{PublishOutcome, code, decode},
    receiver::WorldLog,
};
use rusqlite::{OptionalExtension, params};

use crate::{budget::ForkWrite, trace::TraceVisibility, world_log::FileWorldLog};

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
/// source, which decides whether restricted traces and hidden entries may be copied.
pub fn fork(
    source: &FileWorldLog,
    source_name: &str,
    target: &mut FileWorldLog,
    upto: VersionNumber,
    reader: Role,
) -> Result<Forked, ProtocolError> {
    fork_for(source, source_name, target, upto, Some(reader))
}

/// Like `fork`, for a forker who may hold no role in the source: `None` is an outsider, who
/// copies public content only, at or below the source's public frontier. Both repositories
/// are on this node, so the copy reads the source's store as it writes.
pub fn fork_for(
    source: &FileWorldLog,
    source_name: &str,
    target: &mut FileWorldLog,
    upto: VersionNumber,
    role: Option<Role>,
) -> Result<Forked, ProtocolError> {
    if let Some(forked) = finished(target, source_name, upto)? {
        return Ok(forked);
    }
    let plan = plan(source, source_name, upto, role)?;
    apply(&plan, source.repo(), target)
}

/// The fork `target` holds: `Some` when it was forked from `source_name` at `upto`, `None`
/// when it was never forked. A fork from anywhere else is refused.
pub fn finished(
    target: &FileWorldLog,
    source_name: &str,
    upto: VersionNumber,
) -> Result<Option<Forked>, ProtocolError> {
    let Some(origin) = origin(target)? else {
        return Ok(None);
    };
    if origin.source == source_name && origin.number == upto {
        return Ok(Some(Forked {
            omitted: omitted_traces(target)?,
            origin,
        }));
    }
    Err(refused(&format!(
        "repository is already forked from {} at {}",
        origin.source, origin.number
    )))
}

/// One object a fork writes, in the order it writes them: a hidden entry's content before its
/// opening, and a tree's contents and children before the tree.
#[derive(Clone, Debug)]
enum Item {
    Blob { id: ObjectId, size: u64 },
    Tree { id: ObjectId, size: u64 },
    Opening(Opening, Audience),
}

/// A trace the fork inherits, with the earliest version that named it: copied when `copy` is
/// set, left out for the reader otherwise.
#[derive(Clone, Debug)]
struct Inherited {
    trace: ObjectId,
    number: VersionNumber,
    copy: Option<Copied>,
}

#[derive(Clone, Copy, Debug)]
struct Copied {
    size: u64,
    restricted: bool,
}

/// What a fork copies for one reader, read from the source before anything is written: each
/// version up to the fork point, the objects those versions need that the reader may read, and
/// the traces they inherit. The target's budget is checked against it before the first write.
#[derive(Clone, Debug)]
pub struct Plan {
    origin: Origin,
    /// Each version's `(id, tree)`, by number from World 0.
    versions: Vec<(ObjectId, ObjectId)>,
    items: Vec<Item>,
    traces: Vec<Inherited>,
    /// The paths the versions after World 0 change, and their bytes: what the target's change
    /// sets and blame store for them.
    paths: u64,
    path_bytes: u64,
}

impl Plan {
    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// What applying the plan to an empty target writes, for the budget.
    pub(crate) fn write(&self) -> ForkWrite {
        let mut write = ForkWrite {
            versions: self.versions.len() as u64 - 1,
            paths: self.paths,
            path_bytes: self.path_bytes,
            ..ForkWrite::default()
        };
        for item in &self.items {
            write.objects += 1;
            if let Item::Blob { size, .. } | Item::Tree { size, .. } = item {
                write.bytes += size;
                write.largest = write.largest.max(*size);
            }
        }
        for copied in self.traces.iter().filter_map(|inherited| inherited.copy) {
            write.objects += 1;
            write.bytes += copied.size;
            write.largest = write.largest.max(copied.size);
        }
        write
    }
}

/// Where a fork reads the bytes of the objects in its plan: the source's own store when both
/// repositories are on one node, or the bundle the source's node exported.
pub trait Objects {
    fn tree(&self, id: ObjectId) -> Result<Vec<u8>, ProtocolError>;
    fn blob(&self, id: ObjectId) -> Result<Vec<u8>, ProtocolError>;
}

impl Objects for Repository {
    fn tree(&self, id: ObjectId) -> Result<Vec<u8>, ProtocolError> {
        Ok(self.tree_bytes_bounded(id, MAX_CHUNK)?)
    }

    fn blob(&self, id: ObjectId) -> Result<Vec<u8>, ProtocolError> {
        Ok(self.read_blob(id)?)
    }
}

/// Reads what forking `source` at `upto` copies for a forker with `role`; `None` is an
/// outsider, held at the source's public frontier. Writes nothing.
pub fn plan(
    source: &FileWorldLog,
    source_name: &str,
    upto: VersionNumber,
    role: Option<Role>,
) -> Result<Plan, ProtocolError> {
    // An outsider forks at or below the public frontier, and a point above it is refused
    // exactly as one past the head, so the refusal says nothing about the versions between.
    if role.is_none() && upto > source.frontier()? {
        return Err(refused(&format!(
            "fork point {upto} is past the source's public frontier"
        )));
    }
    if upto.get() > source.head()?.number.get() {
        return Err(refused(&format!(
            "fork point {upto} is past the source's head"
        )));
    }
    let audiences = crate::auth::reader_for(role);
    let mut walk = Walk {
        from: source.repo(),
        reader: &audiences,
        trees: HashSet::new(),
        blobs: HashSet::new(),
        openings: HashSet::new(),
        items: Vec::new(),
    };
    let (mut versions, mut paths, mut path_bytes) = (Vec::new(), 0, 0);
    for number in 0..=upto.get() {
        let number = VersionNumber::new(number)?;
        let version = source.repo().version(number)?;
        if number.get() > 0 {
            walk.tree(version.tree)?;
            for path in source.repo().changeset(number)?.entries().keys() {
                paths += 1;
                path_bytes += path.as_str().len() as u64;
            }
        }
        versions.push((version.id, version.tree));
    }
    let items = walk.items;
    let traces = plan_traces(source, upto, role.unwrap_or(Role::Read))?;
    Ok(Plan {
        origin: Origin {
            source: source_name.into(),
            number: upto,
            version: versions[upto.get() as usize].0,
        },
        versions,
        items,
        traces,
        paths,
        path_bytes,
    })
}

/// Writes `plan` into `target`, reading each object's bytes from `objects`. Nothing is written
/// until the target is known to match the source at its head and the plan fits the target's
/// budget, so a refused fork changes nothing. Each write is idempotent and the origin is
/// written last, so repeating an interrupted apply finishes it.
pub fn apply(
    plan: &Plan,
    objects: &dyn Objects,
    target: &mut FileWorldLog,
) -> Result<Forked, ProtocolError> {
    let origin = &plan.origin;
    if let Some(forked) = finished(target, &origin.source, origin.number)? {
        return Ok(forked);
    }
    let start = target.head()?;
    let matches = plan
        .versions
        .get(start.number.get() as usize)
        .is_some_and(|(id, _)| *id == start.id);
    if !matches {
        return Err(refused("target does not match the source at its head"));
    }
    let now = target.now();
    crate::budget::Room::now(target, now)?.fork(&plan.write())?;

    for item in &plan.items {
        let to = WorldLog::repository(target);
        match item {
            Item::Blob { id, .. } => put_blob(to, objects, *id)?,
            Item::Tree { id, .. } => {
                if !to.has_tree(*id)? {
                    to.put_tree(&objects.tree(*id)?)?;
                }
            }
            Item::Opening(opening, audience) => {
                to.put_opening(opening, audience)?;
            }
        }
    }
    for number in start.number.get() + 1..=origin.number.get() {
        let (id, tree) = plan.versions[number as usize];
        let advanced = WorldLog::repository(target).advance_to_tree(
            &OperationId::parse(&format!("fork-{number}"))?,
            VersionNumber::new(number - 1)?,
            tree,
        )?;
        let AdvanceOutcome::Accepted { version: copied } = advanced else {
            return Err(refused("target head moved during the fork"));
        };
        if copied.id != id {
            return Err(refused("copied version does not match the source"));
        }
    }
    let omitted = apply_traces(plan, objects, target)?;
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
    // The rows the fork wrote count against today's budget, as a request's do. A failure
    // leaves them uncounted; the fork is already made.
    let _ = crate::budget::meter(target, now);
    Ok(Forked {
        origin: origin.clone(),
        omitted,
    })
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

/// The traces a publication up to `upto` named, each to be copied, marked restricted if it
/// was, or left out when the reader may not see it. A trace the source already omitted has no
/// bytes to copy, so it stays omitted.
fn plan_traces(
    source: &FileWorldLog,
    upto: VersionNumber,
    reader: Role,
) -> Result<Vec<Inherited>, ProtocolError> {
    let mut traces = Vec::new();
    for (trace, number) in inherited_traces(source, upto)? {
        let restricted = source.traces().visibility(trace)? == TraceVisibility::Restricted;
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
        let copy = if absent || (restricted && reader < Role::Maintain) {
            None
        } else {
            Some(Copied {
                size: source.repo().read_blob(trace)?.len() as u64,
                restricted,
            })
        };
        traces.push(Inherited {
            trace,
            number,
            copy,
        });
    }
    Ok(traces)
}

/// Records every inherited trace, copies the ones the plan copies and marks the rest left
/// out. Every step is idempotent, so a resumed fork repeats them safely.
fn apply_traces(
    plan: &Plan,
    objects: &dyn Objects,
    target: &mut FileWorldLog,
) -> Result<Vec<ObjectId>, ProtocolError> {
    let mut omitted = Vec::new();
    for inherited in &plan.traces {
        let trace = inherited.trace;
        target
            .records()
            .execute(
                "INSERT OR IGNORE INTO fork_traces (trace, number) VALUES (?1, ?2)",
                params![trace.to_string(), inherited.number.get() as i64],
            )
            .map_err(storage)?;
        let Some(copied) = inherited.copy else {
            target
                .records()
                .execute(
                    "INSERT OR IGNORE INTO fork_omitted_traces (trace) VALUES (?1)",
                    [trace.to_string()],
                )
                .map_err(storage)?;
            omitted.push(trace);
            continue;
        };
        put_blob(WorldLog::repository(target), objects, trace)?;
        if copied.restricted {
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

/// Lists a fork's objects: each tree once, after everything under it. A hidden entry's
/// content and opening are listed, opening last, only when `reader` may read it.
struct Walk<'a> {
    from: &'a Repository,
    reader: &'a Reader,
    trees: HashSet<ObjectId>,
    blobs: HashSet<ObjectId>,
    openings: HashSet<ObjectId>,
    items: Vec<Item>,
}

impl Walk<'_> {
    fn tree(&mut self, root: ObjectId) -> Result<(), ProtocolError> {
        if !self.trees.insert(root) {
            return Ok(());
        }
        let bytes = self.from.tree_bytes_bounded(root, MAX_CHUNK)?;
        let chunk = Chunk::decode(&bytes)?;
        for (_, node) in chunk.entries() {
            match node {
                Node::File(FileEntry::Hidden { commitment, .. }) => {
                    for (_, opening, audience) in self.from.openings(&[commitment], self.reader)? {
                        match opening.file() {
                            Some(_) => self.blob(opening.content)?,
                            None => self.tree(opening.content)?,
                        }
                        if self.openings.insert(commitment) {
                            self.items.push(Item::Opening(opening, audience));
                        }
                    }
                }
                Node::File(entry) => {
                    if let Some(blob) = entry.content() {
                        self.blob(blob)?;
                    }
                }
                Node::Dir(child) => self.tree(child)?,
            }
        }
        for child in chunk.children() {
            self.tree(child)?;
        }
        self.items.push(Item::Tree {
            id: root,
            size: bytes.len() as u64,
        });
        Ok(())
    }

    fn blob(&mut self, id: ObjectId) -> Result<(), ProtocolError> {
        if self.blobs.insert(id) {
            let size = self.from.read_blob(id)?.len() as u64;
            self.items.push(Item::Blob { id, size });
        }
        Ok(())
    }
}

fn put_blob(to: &mut Repository, objects: &dyn Objects, id: ObjectId) -> Result<(), ProtocolError> {
    if !to.has_blob(id)? {
        to.put_blob(&objects.blob(id)?)?;
    }
    Ok(())
}

fn refused(detail: &str) -> ProtocolError {
    ProtocolError::new(code::INVALID_INPUT, detail)
}

fn storage(error: rusqlite::Error) -> ProtocolError {
    ProtocolError::new("storage", error.to_string())
}

/// The largest bundle a source exports. A Durable Object holds a bundle whole, twice (in wasm
/// and in JavaScript), and a Worker's isolate has 128 MB.
pub const MAX_BUNDLE: usize = 32 << 20;
const BUNDLE_MAGIC: &[u8; 5] = b"JVFK1";
const BLOB: u8 = 1;
const TREE: u8 = 2;
const OPENING: u8 = 3;
const OMITTED: u8 = 0;
const COPIED: u8 = 1;
const RESTRICTED: u8 = 2;

/// The plan for forking `source` at `upto`, with every object's bytes, for a target on
/// another node (one Durable Object per repository). It carries exactly what `fork_for` would
/// copy for `role`, so a hidden entry the reader may not read leaves the source as its
/// commitment alone. Over `MAX_BUNDLE` it is refused with `limit_exceeded`.
///
/// Layout: `JVFK1`; the origin (source name, number, version ID); the path count and bytes;
/// each version's `(id, tree)` from World 0; each object in write order, tagged blob or tree
/// (length and bytes) or opening (salt, kind, content, audience); each trace (ID, number, and
/// omitted, or copied or restricted with length and bytes). Integers are big-endian.
pub fn export(
    source: &FileWorldLog,
    source_name: &str,
    upto: VersionNumber,
    role: Option<Role>,
) -> Result<Vec<u8>, ProtocolError> {
    let plan = plan(source, source_name, upto, role)?;
    let from = source.repo();
    let mut out = Out(BUNDLE_MAGIC.to_vec());
    out.text(&plan.origin.source)?;
    out.u64(plan.origin.number.get());
    out.id(plan.origin.version);
    out.u64(plan.paths);
    out.u64(plan.path_bytes);
    out.u64(plan.versions.len() as u64);
    for (id, tree) in &plan.versions {
        out.id(*id);
        out.id(*tree);
    }
    out.u64(plan.items.len() as u64);
    for item in &plan.items {
        match item {
            Item::Blob { id, .. } => {
                out.0.push(BLOB);
                out.bytes(&from.read_blob(*id)?)?;
            }
            Item::Tree { id, .. } => {
                out.0.push(TREE);
                out.bytes(&from.tree_bytes_bounded(*id, MAX_CHUNK)?)?;
            }
            Item::Opening(opening, audience) => {
                out.0.push(OPENING);
                out.0.extend_from_slice(&opening.salt);
                out.0.push(opening.kind as u8);
                out.id(opening.content);
                out.text(&audience.as_str())?;
            }
        }
    }
    out.u64(plan.traces.len() as u64);
    for inherited in &plan.traces {
        out.id(inherited.trace);
        out.u64(inherited.number.get());
        match inherited.copy {
            None => out.0.push(OMITTED),
            Some(copied) => {
                out.0.push(if copied.restricted {
                    RESTRICTED
                } else {
                    COPIED
                });
                out.bytes(&from.read_blob(inherited.trace)?)?;
            }
        }
    }
    Ok(out.0)
}

/// A bundle a source exported, decoded: the plan to `apply`, and the bytes it reads.
pub struct Bundle {
    plan: Plan,
    trees: HashMap<ObjectId, Vec<u8>>,
    blobs: HashMap<ObjectId, Vec<u8>>,
}

impl Bundle {
    /// Decodes `bytes` as `export` wrote them. Each object's ID is computed from its bytes,
    /// and each trace's must match the trace it is for.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > MAX_BUNDLE {
            return Err(ProtocolError::new(
                code::LIMIT_EXCEEDED,
                "fork bundle is too large",
            ));
        }
        let mut r = In(bytes);
        if r.take(BUNDLE_MAGIC.len())? != BUNDLE_MAGIC {
            return Err(malformed("not a fork bundle"));
        }
        let source = r.text()?;
        let number = VersionNumber::new(r.u64()?)?;
        let version = r.id()?;
        let (paths, path_bytes) = (r.u64()?, r.u64()?);
        let mut versions = Vec::new();
        for _ in 0..r.count()? {
            versions.push((r.id()?, r.id()?));
        }
        if versions.len() as u64 != number.get() + 1 || versions[number.get() as usize].0 != version
        {
            return Err(malformed("fork bundle versions do not end at its origin"));
        }
        let (mut items, mut trees, mut blobs) = (Vec::new(), HashMap::new(), HashMap::new());
        for _ in 0..r.count()? {
            match r.u8()? {
                BLOB => {
                    let bytes = r.bytes()?.to_vec();
                    let id = ObjectId::for_blob(&bytes);
                    items.push(Item::Blob {
                        id,
                        size: bytes.len() as u64,
                    });
                    blobs.insert(id, bytes);
                }
                TREE => {
                    let bytes = r.bytes()?.to_vec();
                    let id = ObjectId::for_tree(&bytes);
                    items.push(Item::Tree {
                        id,
                        size: bytes.len() as u64,
                    });
                    trees.insert(id, bytes);
                }
                OPENING => {
                    let salt = r.take(SALT_BYTES)?.try_into().expect("SALT_BYTES bytes");
                    let kind = SealedKind::decode(i64::from(r.u8()?))?;
                    let opening = Opening {
                        salt,
                        kind,
                        content: r.id()?,
                    };
                    items.push(Item::Opening(opening, Audience::parse(&r.text()?)?));
                }
                _ => return Err(malformed("unknown fork bundle object")),
            }
        }
        let mut traces = Vec::new();
        for _ in 0..r.count()? {
            let trace = r.id()?;
            let number = VersionNumber::new(r.u64()?)?;
            let copy = match r.u8()? {
                OMITTED => None,
                tag @ (COPIED | RESTRICTED) => {
                    let bytes = r.bytes()?.to_vec();
                    if ObjectId::for_blob(&bytes) != trace {
                        return Err(malformed("fork bundle trace does not match its ID"));
                    }
                    let size = bytes.len() as u64;
                    blobs.insert(trace, bytes);
                    Some(Copied {
                        size,
                        restricted: tag == RESTRICTED,
                    })
                }
                _ => return Err(malformed("unknown fork bundle trace")),
            };
            traces.push(Inherited {
                trace,
                number,
                copy,
            });
        }
        if !r.0.is_empty() {
            return Err(malformed("fork bundle has trailing bytes"));
        }
        Ok(Self {
            plan: Plan {
                origin: Origin {
                    source,
                    number,
                    version,
                },
                versions,
                items,
                traces,
                paths,
                path_bytes,
            },
            trees,
            blobs,
        })
    }

    pub fn plan(&self) -> &Plan {
        &self.plan
    }
}

impl Objects for Bundle {
    fn tree(&self, id: ObjectId) -> Result<Vec<u8>, ProtocolError> {
        self.trees
            .get(&id)
            .cloned()
            .ok_or_else(|| malformed("fork bundle is missing a tree"))
    }

    fn blob(&self, id: ObjectId) -> Result<Vec<u8>, ProtocolError> {
        self.blobs
            .get(&id)
            .cloned()
            .ok_or_else(|| malformed("fork bundle is missing a blob"))
    }
}

struct Out(Vec<u8>);

impl Out {
    fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    fn id(&mut self, id: ObjectId) {
        self.0.extend_from_slice(id.as_bytes());
    }

    /// Length-prefixed bytes. A bundle that grows past `MAX_BUNDLE` is refused here.
    fn bytes(&mut self, bytes: &[u8]) -> Result<(), ProtocolError> {
        self.u64(bytes.len() as u64);
        self.0.extend_from_slice(bytes);
        if self.0.len() > MAX_BUNDLE {
            return Err(ProtocolError::new(
                code::LIMIT_EXCEEDED,
                format!("the fork is larger than the {MAX_BUNDLE} bytes one node can hand another"),
            ));
        }
        Ok(())
    }

    fn text(&mut self, text: &str) -> Result<(), ProtocolError> {
        self.bytes(text.as_bytes())
    }
}

struct In<'a>(&'a [u8]);

impl<'a> In<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        if self.0.len() < n {
            return Err(malformed("fork bundle is truncated"));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.take(1)?[0])
    }

    fn u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }

    /// A count of entries that each take at least one byte, so it never exceeds the bytes left.
    fn count(&mut self) -> Result<usize, ProtocolError> {
        let count = self.u64()?;
        if count > self.0.len() as u64 {
            return Err(malformed("fork bundle is truncated"));
        }
        Ok(count as usize)
    }

    fn id(&mut self) -> Result<ObjectId, ProtocolError> {
        Ok(ObjectId::from_bytes(
            self.take(32)?.try_into().expect("32 bytes"),
        ))
    }

    fn bytes(&mut self) -> Result<&'a [u8], ProtocolError> {
        let len = self.u64()?;
        if len > self.0.len() as u64 {
            return Err(malformed("fork bundle is truncated"));
        }
        self.take(len as usize)
    }

    fn text(&mut self) -> Result<String, ProtocolError> {
        String::from_utf8(self.bytes()?.to_vec())
            .map_err(|_| malformed("fork bundle text is not UTF-8"))
    }
}

fn malformed(detail: &str) -> ProtocolError {
    ProtocolError::new(code::MALFORMED, detail)
}
