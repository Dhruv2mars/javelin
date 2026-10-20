use std::{fs, path::Path, time::Duration};

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use sha2::Digest;

use crate::{
    Error, FileEntry, Manifest, ObjectId, OperationId, RepoPath, Result, SparseChanges,
    VersionNumber,
    domain::{ObjectKind, WorldIndex, normalize, version_id},
    tree::{self, Node, Trees},
};

const FORMAT: i64 = 8;
const APPLICATION: i64 = 0x4a41564c;
const DATABASE: &str = "repository.sqlite3";
// A World version's primary record is its changeset: one `changes` row per changed
// path, NULL kind for deletion. Its derived tree root is stored with the version,
// computed from the parent root plus the changeset (`src/tree.rs`).
const SCHEMA: &str = "
CREATE TABLE objects (
    id TEXT PRIMARY KEY CHECK(length(id)=64),
    kind INTEGER NOT NULL CHECK(kind IN (1,2,3,4)),
    bytes BLOB NOT NULL
) STRICT;
CREATE TABLE attempts (
    id TEXT PRIMARY KEY,
    expected INTEGER NOT NULL REFERENCES versions(number),
    tree TEXT REFERENCES objects(id),
    accepted INTEGER NOT NULL CHECK(accepted IN (0,1)),
    result INTEGER NOT NULL REFERENCES versions(number) DEFERRABLE INITIALLY DEFERRED,
    CHECK(accepted=0 OR result=expected+1),
    CHECK(tree IS NOT NULL OR accepted=1)
) STRICT;
CREATE TABLE versions (
    number INTEGER PRIMARY KEY CHECK(number>=0),
    id TEXT NOT NULL CHECK(length(id)=64),
    tree TEXT NOT NULL REFERENCES objects(id),
    operation TEXT UNIQUE REFERENCES attempts(id),
    CHECK((number=0 AND operation IS NULL) OR (number>0 AND operation IS NOT NULL))
) STRICT;
CREATE TABLE changes (
    version INTEGER NOT NULL REFERENCES versions(number),
    path TEXT NOT NULL,
    kind INTEGER CHECK(kind IN (1,2,3)),
    content TEXT REFERENCES objects(id),
    PRIMARY KEY(version,path),
    CHECK((kind IS NULL)=(content IS NULL))
) STRICT, WITHOUT ROWID;
CREATE TRIGGER versions_order BEFORE INSERT ON versions
WHEN NEW.number != (SELECT COALESCE(MAX(number)+1,0) FROM versions)
BEGIN SELECT RAISE(ABORT, 'World versions must be contiguous'); END;
-- Inserting a version resolves the deferred attempts.result reference; without
-- this index SQLite scans every attempt inside the Publish write transaction.
CREATE INDEX attempts_result ON attempts(result);
PRAGMA application_id=1245795916;
PRAGMA user_version=8;
";

/// One World version. `tree` is the root of its derived snapshot tree; `id` is its
/// hash-chain identity over every root and changeset so far.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorldVersion {
    pub number: VersionNumber,
    pub id: ObjectId,
    pub tree: ObjectId,
    pub operation: Option<OperationId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AdvanceOutcome {
    Accepted { version: WorldVersion },
    Stale { actual: VersionNumber },
}

#[derive(Debug, Serialize)]
pub struct IntegrityReport {
    pub versions: usize,
    pub objects: usize,
    pub attempts: usize,
    pub layers: usize,
}

/// One handle per caller. Independent processes use independent SQLite connections.
/// The project root must exist. No parent-directory search or working-file ingestion.
pub struct Repository {
    pub(crate) conn: Connection,
    /// Recorded in every checkpoint this handle captures or stacks.
    pub(crate) provenance: Option<crate::Provenance>,
}

impl Repository {
    /// Creates empty immutable World 0, or validates and reopens an existing store.
    /// An interrupted empty initialization can be retried; foreign data is never replaced.
    pub fn init(root: impl AsRef<Path>) -> Result<Self> {
        Self::initialize(root, None)
    }

    /// Like [`Repository::init`], and creates `adoption` on World 0 in the same transaction,
    /// so no store exists with World 0 but without the adoption Layer.
    pub(crate) fn init_adopting(root: impl AsRef<Path>, adoption: &crate::LayerId) -> Result<Self> {
        Self::initialize(root, Some(adoption))
    }

    fn initialize(root: impl AsRef<Path>, adoption: Option<&crate::LayerId>) -> Result<Self> {
        let root = fs::canonicalize(root)?;
        if !root.is_dir() {
            return Err(Error::InvalidInput(
                "project root must be a directory".into(),
            ));
        }
        let directory = root.join(".javelin");
        match fs::create_dir(&directory) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        validate_directory(&directory)?;
        let path = directory.join(DATABASE);
        validate_database_path(&path)?;
        let mut conn = connect(&path, true)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let format: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let application: i64 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
        if format == 0 && application == 0 {
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                if entry.file_name() != DATABASE
                    && entry.file_name() != "repository.sqlite3-journal"
                    && entry.file_name() != ".DS_Store"
                {
                    return Err(Error::CorruptRepository(
                        "unexpected files in uninitialized repository directory".into(),
                    ));
                }
            }
            let tables: i64 =
                tx.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
            if tables != 0 {
                return Err(Error::CorruptRepository(
                    "unrecognized database; refusing initialization".into(),
                ));
            }
            tx.execute_batch(SCHEMA)?;
            tx.execute_batch(crate::layer::SCHEMA)?;
            tx.execute_batch(crate::publish::SCHEMA)?;
            tx.execute_batch(crate::stack::SCHEMA)?;
            tx.execute_batch(crate::radar::SCHEMA)?;
            tx.execute_batch(crate::blame::SCHEMA)?;
            for table in [
                "objects",
                "versions",
                "changes",
                "attempts",
                "repository_identity",
                "layer_heads",
                "layer_attempts",
                "publications",
                "stacks",
                "blame",
            ] {
                for action in ["UPDATE", "DELETE"] {
                    tx.execute_batch(&format!("CREATE TRIGGER {table}_no_{action} BEFORE {action} ON {table} BEGIN SELECT RAISE(ABORT, 'immutable repository data'); END;"))?;
                }
            }
            let empty = insert_object(&tx, ObjectKind::Tree, &[0])?;
            tx.execute(
                "INSERT INTO versions(number,id,tree) VALUES(0,?1,?2)",
                params![
                    version_id(None, empty, &SparseChanges::default()).to_string(),
                    empty.to_string()
                ],
            )?;
            if let Some(layer) = adoption {
                crate::layer::create_on_version_in(&tx, layer, VersionNumber::new(0)?)?;
            }
        } else {
            validate_state_with_checkpoints(&mut crate::layer::CheckpointReader::new(&tx))?;
        }
        #[cfg(test)]
        test_boundary("init_before_commit");
        tx.commit()?;
        sync_directory(&directory)?;
        sync_directory(&root)?;
        #[cfg(test)]
        test_boundary("init_after_commit");
        Ok(Self {
            conn,
            provenance: None,
        })
    }

    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = fs::canonicalize(root)?;
        let directory = root.join(".javelin");
        let path = directory.join(DATABASE);
        if !path.try_exists()? {
            return Err(Error::NotInitialized(root));
        }
        validate_directory(&directory)?;
        validate_database_path(&path)?;
        let conn = connect(&path, false)?;
        let tx = conn.unchecked_transaction()?;
        let format: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let application: i64 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
        if format == 0
            && application == 0
            && tx.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
                r.get::<_, i64>(0)
            })? == 0
        {
            return Err(Error::NotInitialized(root));
        }
        validate_format(&tx)?;
        tx.commit()?;
        Ok(Self {
            conn,
            provenance: None,
        })
    }

    /// Declares who and what this handle acts for. Later captures and Stacks
    /// record it in their checkpoints; None leaves them unattributed.
    pub fn set_provenance(&mut self, provenance: Option<crate::Provenance>) {
        self.provenance = provenance;
    }

    pub fn put_blob(&mut self, bytes: &[u8]) -> Result<ObjectId> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = insert_object(&tx, ObjectKind::Blob, bytes)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn read_blob(&self, id: ObjectId) -> Result<Vec<u8>> {
        read_object(&self.conn, id, ObjectKind::Blob)
    }

    /// Streams and verifies the stored blob's kind and hash with fixed working memory.
    /// Missing objects return false; damaged or wrong-kind objects return errors.
    /// Work is linear in stored bytes. This does not validate any content graph.
    pub fn has_blob(&self, id: ObjectId) -> Result<bool> {
        self.has_object(id, ObjectKind::Blob)
    }

    /// Verifies stored manifest kind and hash only, using fixed working memory.
    /// Does not check canonical encoding, references or symlink targets. A true
    /// result is object-byte availability, not permission or readiness to publish.
    pub fn has_manifest(&self, id: ObjectId) -> Result<bool> {
        self.has_object(id, ObjectKind::Manifest)
    }

    fn has_object(&self, id: ObjectId, expected_kind: ObjectKind) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let found = match verify_object_streamed(&tx, id, expected_kind) {
            Ok(()) => true,
            Err(Error::MissingObject(missing)) if missing == id => false,
            Err(e) => return Err(e),
        };
        tx.commit()?;
        Ok(found)
    }

    /// Verifies blob kind and hash. Refuses over-limit bytes before allocating
    /// the output; an oversized refusal does not certify content integrity.
    pub fn read_blob_bounded(&self, id: ObjectId, max_bytes: usize) -> Result<Vec<u8>> {
        let tx = self.conn.unchecked_transaction()?;
        let bytes = read_object_bounded(&tx, id, ObjectKind::Blob, max_bytes)?;
        tx.commit()?;
        Ok(bytes)
    }

    /// Verifies manifest kind, hash and its own canonical encoding, without
    /// reading referenced blobs. The limit caps encoded bytes, not parser heap.
    /// Full graph validation remains in read_manifest and publication.
    pub fn manifest_bytes_bounded(&self, id: ObjectId, max_bytes: usize) -> Result<Vec<u8>> {
        let tx = self.conn.unchecked_transaction()?;
        let bytes = read_object_bounded(&tx, id, ObjectKind::Manifest, max_bytes)?;
        Manifest::decode(&bytes).map_err(|_| Error::CorruptObject(id))?;
        tx.commit()?;
        Ok(bytes)
    }

    pub fn put_manifest(&mut self, manifest: &Manifest) -> Result<ObjectId> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for entry in manifest.entries().values() {
            validate_entry(&tx, entry)?;
        }
        let id = insert_object(&tx, ObjectKind::Manifest, &manifest.canonical_bytes())?;
        tx.commit()?;
        Ok(id)
    }

    pub fn manifest_bytes(&self, id: ObjectId) -> Result<Vec<u8>> {
        let tx = self.conn.unchecked_transaction()?;
        manifest_in(&tx, id)?;
        let bytes = read_object(&tx, id, ObjectKind::Manifest)?;
        tx.commit()?;
        Ok(bytes)
    }

    pub fn read_manifest(&self, id: ObjectId) -> Result<Manifest> {
        let tx = self.conn.unchecked_transaction()?;
        let manifest = manifest_in(&tx, id)?;
        tx.commit()?;
        Ok(manifest)
    }

    /// The complete snapshot of a World version, read from its derived tree.
    /// Work is O(snapshot), independent of history length.
    pub fn manifest_at(&self, version: VersionNumber) -> Result<Manifest> {
        let tx = self.conn.unchecked_transaction()?;
        let root = version_in(&tx, version)?.tree;
        let manifest = Manifest::new(Trees::new(Sql::read(&tx)).entries(root)?)?;
        tx.commit()?;
        Ok(manifest)
    }

    /// The exact changeset a World version applied to its parent.
    pub fn changeset(&self, version: VersionNumber) -> Result<SparseChanges> {
        let tx = self.conn.unchecked_transaction()?;
        version_in(&tx, version)?;
        let changes = changeset_in(&tx, version)?;
        tx.commit()?;
        Ok(changes)
    }

    /// Reads the complete ordered sequence and validates all reachable content.
    pub fn history(&self) -> Result<Vec<WorldVersion>> {
        let tx = self.conn.unchecked_transaction()?;
        let history =
            validate_state_with_checkpoints(&mut crate::layer::CheckpointReader::new(&tx))?;
        tx.commit()?;
        Ok(history)
    }

    /// The current World version, read from its single row.
    pub fn head(&self) -> Result<WorldVersion> {
        let tx = self.conn.unchecked_transaction()?;
        validate_format(&tx)?;
        let head = head_in(&tx)?;
        tx.commit()?;
        Ok(head)
    }

    pub fn version(&self, number: VersionNumber) -> Result<WorldVersion> {
        let tx = self.conn.unchecked_transaction()?;
        validate_format(&tx)?;
        let version = version_in(&tx, number)?;
        tx.commit()?;
        Ok(version)
    }

    /// Publishes metadata only. The caller will later supply Layer, permission and
    /// policy validation. Keep the same operation ID and input after any uncertain error.
    /// Accepted and stale outcomes are durable and returned unchanged on retry.
    /// The manifest's tree is built in O(manifest); its changeset is the tree diff
    /// against the head, which skips shared subtrees.
    pub fn compare_and_advance(
        &mut self,
        operation: &OperationId,
        expected: VersionNumber,
        manifest: ObjectId,
    ) -> Result<AdvanceOutcome> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_format(&tx)?;
        if crate::publish::operation_exists_in(&tx, operation)? {
            return Err(Error::OperationIdReused(operation.clone()));
        }
        // Revalidate the entire candidate graph inside the publication transaction.
        let candidate = manifest_in(&tx, manifest)?;
        let root = Trees::new(Sql::write(&tx)).build(candidate.entries())?;
        let outcome = advance_in(&tx, operation, expected, root, false)?;
        #[cfg(test)]
        test_boundary("before_commit");
        tx.commit()?;
        #[cfg(test)]
        test_boundary("after_commit");
        Ok(outcome)
    }

    /// `compare_and_advance` to a received tree. The tree must be stored, its
    /// changed file contents valid, and it must be canonical: the root derived from
    /// `expected`'s tree plus the tree diff must equal `tree`. Work is O(change x
    /// depth); unchanged subtrees are never read.
    pub fn advance_to_tree(
        &mut self,
        operation: &OperationId,
        expected: VersionNumber,
        tree: ObjectId,
    ) -> Result<AdvanceOutcome> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_format(&tx)?;
        let outcome = advance_in(&tx, operation, expected, tree, true)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// Validates a snapshot's contents and stores its tree; returns the root. This
    /// is how a client turns a full snapshot into a tree to push. O(snapshot).
    pub fn put_snapshot(&mut self, manifest: &Manifest) -> Result<ObjectId> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for entry in manifest.entries().values() {
            validate_entry(&tx, entry)?;
        }
        let root = Trees::new(Sql::write(&tx)).build(manifest.entries())?;
        tx.commit()?;
        Ok(root)
    }

    pub fn has_tree(&self, id: ObjectId) -> Result<bool> {
        self.has_object(id, ObjectKind::Tree)
    }

    /// Verified, canonical bytes of one tree chunk, refused over `max_bytes`.
    pub fn tree_bytes_bounded(&self, id: ObjectId, max_bytes: usize) -> Result<Vec<u8>> {
        let tx = self.conn.unchecked_transaction()?;
        let bytes = read_object_bounded(&tx, id, ObjectKind::Tree, max_bytes)?;
        tree::Chunk::decode(&bytes).map_err(|_| Error::CorruptObject(id))?;
        tx.commit()?;
        Ok(bytes)
    }

    /// Stores one received tree chunk. Its encoding must be canonical, and every
    /// child chunk, subdirectory and file content it names must already be stored
    /// with the right kind, so a stored tree always has its whole closure and a
    /// receiver that has a chunk needs nothing below it.
    pub fn put_tree(&mut self, bytes: &[u8]) -> Result<ObjectId> {
        let chunk = tree::Chunk::decode(bytes)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let refs = chunk
            .children()
            .map(|id| (id, ObjectKind::Tree))
            .chain(chunk.entries().map(|(_, node)| match node {
                Node::File(entry) => (entry.content(), ObjectKind::Blob),
                Node::Dir(id) => (id, ObjectKind::Tree),
            }));
        for (id, expected) in refs {
            let kind: Option<i64> = tx
                .prepare_cached("SELECT kind FROM objects WHERE id=?1")?
                .query_row([id.to_string()], |r| r.get(0))
                .optional()?;
            match kind.map(ObjectKind::decode).transpose()? {
                None => return Err(Error::MissingObject(id)),
                Some(kind) if kind != expected => return Err(Error::WrongObjectKind(id)),
                _ => {}
            }
        }
        let id = insert_object(&tx, ObjectKind::Tree, bytes)?;
        tx.commit()?;
        Ok(id)
    }

    /// Checks physical SQLite integrity plus every stored object's native graph,
    /// the World hash chain and every changeset, and every accepted/stale operation
    /// outcome, within one consistent read snapshot. Commands validate only what
    /// they touch; this is the full-graph check.
    pub fn check(&self) -> Result<IntegrityReport> {
        let tx = self.conn.unchecked_transaction()?;
        validate_format(&tx)?;
        let integrity: String = tx.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if integrity != "ok" {
            return Err(Error::CorruptRepository(integrity));
        }
        let foreign_key_failure = tx.prepare("PRAGMA foreign_key_check")?.exists([])?;
        if foreign_key_failure {
            return Err(Error::CorruptRepository(
                "invalid foreign-key references".into(),
            ));
        }
        let mut checkpoints = crate::layer::CheckpointReader::new(&tx);
        let history = validate_state_with_checkpoints(&mut checkpoints)?;
        let objects = tx
            .prepare("SELECT id,kind FROM objects ORDER BY id")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (id, kind) in &objects {
            let id = decode_id(id)?;
            match ObjectKind::decode(*kind)? {
                ObjectKind::Blob => {
                    read_object(&tx, id, ObjectKind::Blob)?;
                }
                ObjectKind::Manifest => {
                    manifest_in(&tx, id)?;
                }
                ObjectKind::Checkpoint => {
                    checkpoints.read(id)?;
                }
                ObjectKind::Tree => {
                    tree::Chunk::decode(&read_object(&tx, id, ObjectKind::Tree)?)
                        .map_err(|_| Error::CorruptObject(id))?;
                }
            }
        }
        let attempts: i64 = tx.query_row("SELECT count(*) FROM attempts", [], |r| r.get(0))?;
        let layers = crate::layer::check_layers(&mut checkpoints)?;
        let report = IntegrityReport {
            layers,
            versions: history.len(),
            objects: objects.len(),
            attempts: usize::try_from(attempts).map_err(|_| {
                Error::CorruptRepository("operation count exceeds memory range".into())
            })?,
        };
        tx.commit()?;
        Ok(report)
    }
}

/// Full state validation for explicit integrity reads: World chain, Layers, Stacks,
/// Publications. Per-command paths never call this.
fn validate_state_with_checkpoints(
    checkpoints: &mut crate::layer::CheckpointReader<'_>,
) -> Result<Vec<WorldVersion>> {
    let conn = checkpoints.conn;
    validate_format(conn)?;
    let history = history_in(conn)?;
    crate::layer::check_layers(checkpoints)?;
    crate::stack::check_stacks(conn)?;
    crate::publish::check_publications(conn)?;
    crate::radar::check_radar(checkpoints)?;
    crate::blame::check_blame(checkpoints)?;
    Ok(history)
}

fn validate_directory(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(Error::InvalidInput(
            "repository directory must be a real directory".into(),
        ));
    }
    Ok(())
}

fn validate_database_path(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => Err(Error::InvalidInput(
            "repository database must be a regular file".into(),
        )),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn connect(path: &Path, create: bool) -> Result<Connection> {
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    if create {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=EXTRA; PRAGMA fullfsync=ON;")?;
    let journal: String = conn.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
    if journal != "delete" {
        return Err(Error::CorruptRepository(
            "repository requires DELETE journal mode".into(),
        ));
    }
    Ok(conn)
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path; // SQLite sync remains enabled; parent directory durability needs platform verification.
    Ok(())
}

pub(crate) fn validate_format(conn: &Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version != FORMAT {
        return Err(Error::UnsupportedFormat(version));
    }
    let application: i64 = conn.pragma_query_value(None, "application_id", |r| r.get(0))?;
    if application != APPLICATION {
        return Err(Error::CorruptRepository(
            "unrecognized database identity".into(),
        ));
    }
    Ok(())
}

pub(crate) fn insert_object(conn: &Connection, kind: ObjectKind, bytes: &[u8]) -> Result<ObjectId> {
    let id = ObjectId::hash(kind, bytes);
    conn.execute(
        "INSERT INTO objects(id,kind,bytes) VALUES(?1,?2,?3) ON CONFLICT(id) DO NOTHING",
        params![id.to_string(), kind as i64, bytes],
    )?;
    if read_object(conn, id, kind)? != bytes {
        return Err(Error::CorruptObject(id));
    }
    Ok(id)
}

pub(crate) fn read_object(
    conn: &Connection,
    id: ObjectId,
    expected_kind: ObjectKind,
) -> Result<Vec<u8>> {
    let (kind, bytes): (i64, Vec<u8>) = conn
        .query_row(
            "SELECT kind,bytes FROM objects WHERE id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(Error::MissingObject(id))?;
    let kind = ObjectKind::decode(kind)?;
    if ObjectId::hash(kind, &bytes) != id {
        return Err(Error::CorruptObject(id));
    }
    if kind != expected_kind {
        return Err(Error::WrongObjectKind(id));
    }
    Ok(bytes)
}

fn object_blob(conn: &Connection, id: ObjectId) -> Result<(ObjectKind, rusqlite::blob::Blob<'_>)> {
    let (rowid, kind): (i64, i64) = conn
        .query_row(
            "SELECT rowid,kind FROM objects WHERE id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(Error::MissingObject(id))?;
    let kind = ObjectKind::decode(kind)?;
    let blob = conn.blob_open(rusqlite::MAIN_DB, c"objects", c"bytes", rowid, true)?;
    Ok((kind, blob))
}

fn verify_object_streamed(
    conn: &Connection,
    id: ObjectId,
    expected_kind: ObjectKind,
) -> Result<()> {
    let (kind, blob) = object_blob(conn, id)?;
    let mut hash = ObjectId::hasher(kind);
    let mut buffer = [0; 64 * 1024];
    let mut offset = 0;
    while offset < blob.len() {
        let len = buffer.len().min(blob.len() - offset);
        blob.read_at_exact(&mut buffer[..len], offset)?;
        hash.update(&buffer[..len]);
        offset += len;
    }
    if ObjectId::from_bytes(hash.finalize().into()) != id {
        return Err(Error::CorruptObject(id));
    }
    if kind != expected_kind {
        return Err(Error::WrongObjectKind(id));
    }
    Ok(())
}

fn read_object_bounded(
    conn: &Connection,
    id: ObjectId,
    expected_kind: ObjectKind,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let (kind, blob) = object_blob(conn, id)?;
    let size = blob.len();
    if size > max_bytes {
        return Err(Error::ObjectTooLarge {
            id,
            size,
            limit: max_bytes,
        });
    }
    let mut bytes = vec![0; size];
    blob.read_at_exact(&mut bytes, 0)?;
    if ObjectId::hash(kind, &bytes) != id {
        return Err(Error::CorruptObject(id));
    }
    if kind != expected_kind {
        return Err(Error::WrongObjectKind(id));
    }
    Ok(bytes)
}

pub(crate) fn manifest_in(conn: &Connection, id: ObjectId) -> Result<Manifest> {
    let bytes = read_object(conn, id, ObjectKind::Manifest)?;
    let manifest = Manifest::decode(&bytes).map_err(|_| Error::CorruptObject(id))?;
    for entry in manifest.entries().values() {
        validate_entry(conn, entry)?;
    }
    Ok(manifest)
}

pub(crate) fn validate_entry(conn: &Connection, entry: &crate::FileEntry) -> Result<()> {
    let bytes = read_object(conn, entry.content(), ObjectKind::Blob)?;
    if matches!(entry, crate::FileEntry::Symlink { .. }) && (bytes.is_empty() || bytes.contains(&0))
    {
        return Err(Error::InvalidInput(
            "symlink target must be nonempty and contain no NUL".into(),
        ));
    }
    Ok(())
}

pub(crate) fn decode_id(value: &str) -> Result<ObjectId> {
    ObjectId::parse(value).map_err(|_| Error::CorruptRepository("invalid stored object ID".into()))
}

fn decode_operation(value: &str) -> Result<OperationId> {
    OperationId::parse(value)
        .map_err(|_| Error::CorruptRepository("invalid stored operation ID".into()))
}

pub(crate) fn next_number(number: VersionNumber) -> Result<VersionNumber> {
    VersionNumber::new(
        number
            .get()
            .checked_add(1)
            .ok_or_else(|| Error::InvalidInput("World version overflow".into()))?,
    )
}

type VersionRow = (i64, String, String, Option<String>);

fn version_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<VersionRow> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
}

fn decode_version((number, id, tree, operation): VersionRow) -> Result<WorldVersion> {
    let number = VersionNumber::decode(number)?;
    let operation = operation.as_deref().map(decode_operation).transpose()?;
    if (number.get() == 0) != operation.is_none() {
        return Err(Error::CorruptRepository("invalid version operation".into()));
    }
    Ok(WorldVersion {
        number,
        id: decode_id(&id)?,
        tree: decode_id(&tree)?,
        operation,
    })
}

/// One version row and its accepted-attempt binding, without walking history.
/// Its hash-chain link is verified by `history_in`.
pub(crate) fn version_in(conn: &Connection, number: VersionNumber) -> Result<WorldVersion> {
    let version = conn
        .prepare_cached("SELECT number,id,tree,operation FROM versions WHERE number=?1")?
        .query_row([number.sql()], version_row)
        .optional()?
        .map(decode_version)
        .transpose()?
        .ok_or(Error::MissingVersion(number))?;
    bind_attempt(conn, &version)?;
    Ok(version)
}

/// The latest World, found by primary key. Contiguity is enforced on insert.
pub(crate) fn head_in(conn: &Connection) -> Result<WorldVersion> {
    let version = conn
        .prepare_cached(
            "SELECT number,id,tree,operation FROM versions ORDER BY number DESC LIMIT 1",
        )?
        .query_row([], version_row)
        .optional()?
        .map(decode_version)
        .transpose()?
        .ok_or_else(|| Error::CorruptRepository("missing initial World".into()))?;
    bind_attempt(conn, &version)?;
    Ok(version)
}

fn bind_attempt(conn: &Connection, version: &WorldVersion) -> Result<()> {
    if let Some(op) = &version.operation {
        let attempt = attempt_in(conn, op)?
            .ok_or_else(|| Error::CorruptRepository("missing accepted attempt".into()))?;
        if !attempt.accepted
            || attempt.result != version.number
            || attempt.expected.get() != version.number.get() - 1
        {
            return Err(Error::CorruptRepository(
                "version and operation disagree".into(),
            ));
        }
    }
    Ok(())
}

pub(crate) fn encode_entry(entry: &Option<FileEntry>) -> (Option<i64>, Option<String>) {
    match entry {
        None => (None, None),
        Some(FileEntry::Regular {
            content,
            executable,
        }) => (Some(1 + i64::from(*executable)), Some(content.to_string())),
        Some(FileEntry::Symlink { target }) => (Some(3), Some(target.to_string())),
    }
}

pub(crate) fn decode_entry(
    kind: Option<i64>,
    content: Option<String>,
) -> Result<Option<FileEntry>> {
    let content = content.as_deref().map(decode_id).transpose()?;
    match (kind, content) {
        (None, None) => Ok(None),
        (Some(kind @ (1 | 2)), Some(content)) => Ok(Some(FileEntry::Regular {
            content,
            executable: kind == 2,
        })),
        (Some(3), Some(target)) => Ok(Some(FileEntry::Symlink { target })),
        _ => Err(Error::CorruptRepository("invalid World change".into())),
    }
}

pub(crate) fn decode_path(path: &str) -> Result<RepoPath> {
    RepoPath::parse(path).map_err(|_| Error::CorruptRepository("invalid World change path".into()))
}

pub(crate) fn changeset_in(conn: &Connection, version: VersionNumber) -> Result<SparseChanges> {
    let rows = conn
        .prepare_cached("SELECT path,kind,content FROM changes WHERE version=?1 ORDER BY path")?
        .query_map([version.sql()], |r| {
            Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    SparseChanges::new(
        rows.into_iter()
            .map(|(path, kind, content)| Ok((decode_path(&path)?, decode_entry(kind, content)?)))
            .collect::<Result<Vec<_>>>()?,
    )
}

/// Appends version `head + 1` with its changeset rows and derived root, whose
/// chunks are already stored. O(change).
pub(crate) fn append_version_in(
    conn: &Connection,
    head: &WorldVersion,
    operation: &OperationId,
    changes: &SparseChanges,
    tree: ObjectId,
) -> Result<WorldVersion> {
    let version = WorldVersion {
        number: next_number(head.number)?,
        id: version_id(Some(head.id), tree, changes),
        tree,
        operation: Some(operation.clone()),
    };
    conn.prepare_cached("INSERT INTO versions(number,id,tree,operation) VALUES(?1,?2,?3,?4)")?
        .execute(params![
            version.number.sql(),
            version.id.to_string(),
            tree.to_string(),
            operation.as_str()
        ])?;
    let mut insert =
        conn.prepare_cached("INSERT INTO changes(path,version,kind,content) VALUES(?1,?2,?3,?4)")?;
    for (path, entry) in changes.entries() {
        let (kind, content) = encode_entry(entry);
        insert.execute(params![path.as_str(), version.number.sql(), kind, content])?;
    }
    Ok(version)
}

/// Where derived tree chunks go: written in the current transaction, buffered for
/// a later write transaction (Publish prepares outside the lock), or only checked
/// for presence (`check` recomputes every root without writing).
enum Sink {
    Write,
    Buffer(std::cell::RefCell<Vec<(ObjectId, Vec<u8>)>>),
    Verify,
}

/// Tree objects in the `objects` table.
pub(crate) struct Sql<'a> {
    conn: &'a Connection,
    sink: Sink,
}

impl<'a> Sql<'a> {
    pub(crate) fn write(conn: &'a Connection) -> Self {
        Self {
            conn,
            sink: Sink::Write,
        }
    }
    pub(crate) fn buffer(conn: &'a Connection) -> Self {
        Self {
            conn,
            sink: Sink::Buffer(Default::default()),
        }
    }
    /// Reads only; a derived chunk must already be stored.
    pub(crate) fn read(conn: &'a Connection) -> Self {
        Self {
            conn,
            sink: Sink::Verify,
        }
    }
    /// Chunks recorded by a buffering store, in creation order.
    pub(crate) fn buffered(self) -> Vec<(ObjectId, Vec<u8>)> {
        match self.sink {
            Sink::Buffer(chunks) => chunks.into_inner(),
            _ => Vec::new(),
        }
    }
}

impl tree::Store for Sql<'_> {
    fn load(&self, id: ObjectId) -> Result<Vec<u8>> {
        read_object(self.conn, id, ObjectKind::Tree)
    }
    fn save(&self, id: ObjectId, bytes: &[u8]) -> Result<()> {
        match &self.sink {
            Sink::Write => insert_object(self.conn, ObjectKind::Tree, bytes).map(drop),
            Sink::Buffer(chunks) => {
                chunks.borrow_mut().push((id, bytes.to_vec()));
                Ok(())
            }
            // Reading verifies the stored chunk's hash and kind.
            Sink::Verify => match read_object(self.conn, id, ObjectKind::Tree) {
                Ok(stored) if stored == bytes => Ok(()),
                Ok(_) | Err(Error::MissingObject(_)) => Err(Error::CorruptRepository(format!(
                    "derived tree chunk {id} is not stored"
                ))),
                Err(e) => Err(e),
            },
        }
    }
}

/// The stored root of a World version, by primary key.
pub(crate) fn tree_in(conn: &Connection, version: VersionNumber) -> Result<ObjectId> {
    let tree: String = conn
        .prepare_cached("SELECT tree FROM versions WHERE number=?1")?
        .query_row([version.sql()], |r| r.get(0))
        .optional()?
        .ok_or(Error::MissingVersion(version))?;
    decode_id(&tree)
}

/// Point reads over World snapshots through their derived trees: O(depth) chunk
/// reads per lookup, independent of history length.
pub(crate) struct Index<'a> {
    pub(crate) trees: Trees<Sql<'a>>,
    roots: std::cell::RefCell<std::collections::HashMap<VersionNumber, ObjectId>>,
}

impl<'a> Index<'a> {
    pub(crate) fn new(conn: &'a Connection) -> Self {
        Self::over(Sql::read(conn))
    }
    pub(crate) fn over(store: Sql<'a>) -> Self {
        Self {
            trees: Trees::new(store),
            roots: Default::default(),
        }
    }
    pub(crate) fn root(&self, version: VersionNumber) -> Result<ObjectId> {
        if let Some(root) = self.roots.borrow().get(&version) {
            return Ok(*root);
        }
        let root = tree_in(self.trees.store().conn, version)?;
        self.roots.borrow_mut().insert(version, root);
        Ok(root)
    }
    /// Uses `root` for `version` instead of the stored root.
    fn pin(&self, version: VersionNumber, root: ObjectId) {
        self.roots.borrow_mut().insert(version, root);
    }
    pub(crate) fn node(&self, path: &RepoPath, version: VersionNumber) -> Result<Option<Node>> {
        self.trees.lookup(self.root(version)?, path)
    }
}

impl WorldIndex for Index<'_> {
    fn entry_at(&self, path: &RepoPath, version: VersionNumber) -> Result<Option<FileEntry>> {
        Ok(match self.node(path, version)? {
            Some(Node::File(entry)) => Some(entry),
            _ => None,
        })
    }

    fn is_dir(&self, path: &RepoPath, version: VersionNumber) -> Result<bool> {
        Ok(matches!(self.node(path, version)?, Some(Node::Dir(_))))
    }

    fn files_under(&self, dir: &RepoPath, version: VersionNumber) -> Result<Vec<RepoPath>> {
        self.trees.files_under(self.root(version)?, dir)
    }

    fn check_entry(&self, entry: &FileEntry) -> Result<()> {
        validate_entry(self.trees.store().conn, entry)
    }
}

/// Records a World attempt for candidate root `root`. A `received` root is
/// verified first: changed contents must be valid and the root canonical.
fn advance_in(
    tx: &Connection,
    operation: &OperationId,
    expected: VersionNumber,
    root: ObjectId,
    received: bool,
) -> Result<AdvanceOutcome> {
    let head = head_in(tx)?;
    if crate::publish::operation_exists_in(tx, operation)? {
        return Err(Error::OperationIdReused(operation.clone()));
    }
    if let Some(attempt) = attempt_in(tx, operation)? {
        if attempt.expected != expected || attempt.tree != Some(root) {
            return Err(Error::OperationIdReused(operation.clone()));
        }
        return attempt.outcome(tx);
    }
    if expected > head.number {
        return Err(Error::MissingVersion(expected));
    }
    let trees = Trees::new(Sql::write(tx));
    let changes = if received {
        let base = tree_in(tx, expected)?;
        let changes = trees.diff(base, root)?;
        for entry in changes.entries().values().flatten() {
            validate_entry(tx, entry)?;
        }
        if trees.apply(base, &changes)? != root {
            return Err(Error::InvalidInput("tree is not in canonical form".into()));
        }
        Some(changes)
    } else {
        None
    };
    let accepted = expected == head.number;
    let result = if accepted {
        next_number(head.number)?
    } else {
        head.number
    };
    tx.execute(
        "INSERT INTO attempts(id,expected,tree,accepted,result) VALUES(?1,?2,?3,?4,?5)",
        params![
            operation.as_str(),
            expected.sql(),
            root.to_string(),
            accepted,
            result.sql()
        ],
    )?;
    if !accepted {
        return Ok(AdvanceOutcome::Stale { actual: result });
    }
    let changes = match changes {
        Some(changes) => changes,
        None => trees.diff(head.tree, root)?,
    };
    Ok(AdvanceOutcome::Accepted {
        version: append_version_in(tx, &head, operation, &changes, root)?,
    })
}

/// Verifies every version row, its changeset against the parent tree, its derived
/// root recomputed from the parent root plus the changeset (every chunk must be
/// stored), and its hash-chain link; then every World attempt. Work is O(total
/// change x depth) plus each stale candidate tree; no full snapshot is held.
pub(crate) fn history_in(conn: &Connection) -> Result<Vec<WorldVersion>> {
    let rows = conn
        .prepare("SELECT number,id,tree,operation FROM versions ORDER BY number")?
        .query_map([], version_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if rows.is_empty() {
        return Err(Error::CorruptRepository("missing initial World".into()));
    }
    let orphans = conn
        .prepare("SELECT 1 FROM changes WHERE version NOT IN (SELECT number FROM versions)")?
        .exists([])?;
    if orphans {
        return Err(Error::CorruptRepository("orphan World change".into()));
    }
    let index = Index::new(conn);
    let mut history: Vec<WorldVersion> = Vec::new();
    for (index_number, row) in rows.into_iter().enumerate() {
        let version = decode_version(row)?;
        if version.number.get() != index_number as u64 {
            return Err(Error::CorruptRepository(
                "World history is not contiguous".into(),
            ));
        }
        let changes = changeset_in(conn, version.number)?;
        let root = match history.last() {
            None if changes.entries().is_empty() => index.trees.apply(tree::empty(), &changes)?,
            None => return Err(Error::CorruptRepository("invalid initial World".into())),
            Some(parent) => {
                match normalize(&index, parent.number, &changes) {
                    Ok(normalized) if normalized == changes => {}
                    Ok(_) | Err(Error::InvalidInput(_)) => {
                        return Err(Error::CorruptRepository(
                            "World changeset is not canonical against its parent".into(),
                        ));
                    }
                    Err(e) => return Err(e),
                }
                index.trees.apply(parent.tree, &changes)?
            }
        };
        if root != version.tree {
            return Err(Error::CorruptRepository(
                "World tree disagrees with its derivation".into(),
            ));
        }
        index.pin(version.number, root);
        index.trees.forget();
        if version.id != version_id(history.last().map(|v| v.id), root, &changes) {
            return Err(Error::CorruptRepository(
                "World version identity disagrees with its changeset".into(),
            ));
        }
        if let Some(op) = &version.operation {
            let attempt = attempt_in(conn, op)?
                .ok_or_else(|| Error::CorruptRepository("missing accepted attempt".into()))?;
            if !attempt.accepted
                || attempt.result != version.number
                || attempt.expected.get() != version.number.get() - 1
            {
                return Err(Error::CorruptRepository(
                    "version and operation disagree".into(),
                ));
            }
            if attempt.tree.is_some_and(|tree| tree != root) {
                return Err(Error::CorruptRepository(
                    "accepted tree disagrees with derived World".into(),
                ));
            }
        } else if index_number != 0 {
            return Err(Error::CorruptRepository("invalid initial World".into()));
        }
        history.push(version);
    }
    let operations = conn
        .prepare("SELECT id FROM attempts ORDER BY id")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for id in operations {
        let operation = decode_operation(&id)?;
        let attempt = attempt_in(conn, &operation)?
            .ok_or_else(|| Error::CorruptRepository("missing attempt".into()))?;
        match attempt.tree {
            // Accepted candidates equal a verified World root.
            Some(_) if attempt.accepted => {}
            Some(tree) => {
                for entry in index.trees.entries(tree)?.values() {
                    validate_entry(conn, entry)?;
                }
            }
            // Layer publications record their candidate through the publication row.
            None if crate::publish::operation_exists_in(conn, &operation)? => {}
            None => {
                return Err(Error::CorruptRepository(
                    "World attempt has no candidate".into(),
                ));
            }
        }
        attempt.outcome(conn)?;
    }
    Ok(history)
}

struct Attempt {
    operation: OperationId,
    expected: VersionNumber,
    tree: Option<ObjectId>,
    accepted: bool,
    result: VersionNumber,
}

impl Attempt {
    fn outcome(&self, conn: &Connection) -> Result<AdvanceOutcome> {
        let actual = version_in(conn, self.result).map_err(|_| {
            Error::CorruptRepository("attempt refers to missing result version".into())
        })?;
        if self.accepted {
            if self.result.get() != self.expected.get() + 1
                || actual.operation.as_ref() != Some(&self.operation)
            {
                return Err(Error::CorruptRepository(
                    "accepted attempt disagrees with World".into(),
                ));
            }
            Ok(AdvanceOutcome::Accepted { version: actual })
        } else {
            if self.result <= self.expected {
                return Err(Error::CorruptRepository("invalid stale outcome".into()));
            }
            Ok(AdvanceOutcome::Stale {
                actual: self.result,
            })
        }
    }
}

fn attempt_in(conn: &Connection, operation: &OperationId) -> Result<Option<Attempt>> {
    let row = conn
        .prepare_cached("SELECT expected,tree,accepted,result FROM attempts WHERE id=?1")?
        .query_row([operation.as_str()], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .optional()?;
    row.map(|(expected, tree, accepted, result)| {
        if accepted != 0 && accepted != 1 {
            return Err(Error::CorruptRepository("invalid attempt outcome".into()));
        }
        Ok(Attempt {
            operation: operation.clone(),
            expected: VersionNumber::decode(expected)?,
            tree: tree.as_deref().map(decode_id).transpose()?,
            accepted: accepted == 1,
            result: VersionNumber::decode(result)?,
        })
    })
    .transpose()
}

#[cfg(test)]
thread_local! {
    /// Runs once after a Publish commits, before its reclaim, so a test can take the
    /// database lock at exactly that point.
    pub(crate) static BEFORE_RECLAIM: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn test_boundary(phase: &str) {
    use std::io::{Read, Write};
    if std::env::var("JAVELIN_TEST_BOUNDARY").as_deref() == Ok(phase) {
        println!("BOUNDARY {phase}");
        std::io::stdout().flush().unwrap();
        let mut byte = [0];
        std::io::stdin().read_exact(&mut byte).unwrap();
    }
}

/// Simulated power loss at a named point, for process-level recovery tests.
/// Debug builds only: `JAVELIN_FAILPOINT=<name>` exits with status 86 at that point,
/// without unwinding, dropping connections or finishing the operation. Release
/// builds compile this to nothing (docs/robustness.md).
#[cfg(debug_assertions)]
pub(crate) fn failpoint(name: &str) {
    const EXIT_STATUS: i32 = 86;
    if std::env::var_os("JAVELIN_FAILPOINT").is_some_and(|set| set == name) {
        std::process::exit(EXIT_STATUS);
    }
}

#[cfg(not(debug_assertions))]
pub(crate) fn failpoint(_: &str) {}

#[cfg(test)]
#[path = "../tests/support/process.rs"]
mod process_tests;
