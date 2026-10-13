use std::{fs, path::Path, time::Duration};

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

use crate::{Error, Manifest, ObjectId, OperationId, Result, VersionNumber, domain::ObjectKind};

const FORMAT: i64 = 1;
const APPLICATION: i64 = 0x4a41564c;
const DATABASE: &str = "repository.sqlite3";
const SCHEMA: &str = "
CREATE TABLE objects (
    id TEXT PRIMARY KEY CHECK(length(id)=64),
    kind INTEGER NOT NULL CHECK(kind IN (1,2)),
    bytes BLOB NOT NULL
) STRICT;
CREATE TABLE attempts (
    id TEXT PRIMARY KEY,
    expected INTEGER NOT NULL REFERENCES versions(number),
    manifest TEXT NOT NULL REFERENCES objects(id),
    accepted INTEGER NOT NULL CHECK(accepted IN (0,1)),
    result INTEGER NOT NULL REFERENCES versions(number) DEFERRABLE INITIALLY DEFERRED,
    CHECK(accepted=0 OR result=expected+1)
) STRICT;
CREATE TABLE versions (
    number INTEGER PRIMARY KEY CHECK(number>=0),
    manifest TEXT NOT NULL REFERENCES objects(id),
    operation TEXT UNIQUE REFERENCES attempts(id),
    CHECK((number=0 AND operation IS NULL) OR (number>0 AND operation IS NOT NULL))
) STRICT;
CREATE TRIGGER versions_order BEFORE INSERT ON versions
WHEN NEW.number != (SELECT COALESCE(MAX(number)+1,0) FROM versions)
BEGIN SELECT RAISE(ABORT, 'World versions must be contiguous'); END;
PRAGMA application_id=1245795916;
PRAGMA user_version=1;
";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorldVersion {
    pub number: VersionNumber,
    pub manifest: ObjectId,
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
}

/// One handle per caller. Independent processes use independent SQLite connections.
/// The project root must exist. No parent-directory search or working-file ingestion.
pub struct Repository {
    conn: Connection,
}

impl Repository {
    /// Creates empty immutable World 0, or validates and reopens an existing store.
    /// An interrupted empty initialization can be retried; foreign data is never replaced.
    pub fn init(root: impl AsRef<Path>) -> Result<Self> {
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
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            if entry.file_name() != DATABASE && entry.file_name() != "repository.sqlite3-journal" {
                return Err(Error::CorruptRepository(
                    "unexpected files in repository directory; refusing initialization".into(),
                ));
            }
        }
        let path = directory.join(DATABASE);
        validate_database_path(&path)?;
        let mut conn = connect(&path, true)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let format: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let application: i64 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
        if format == 0 && application == 0 {
            let tables: i64 =
                tx.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
            if tables != 0 {
                return Err(Error::CorruptRepository(
                    "unrecognized database; refusing initialization".into(),
                ));
            }
            tx.execute_batch(SCHEMA)?;
            for table in ["objects", "versions", "attempts"] {
                for action in ["UPDATE", "DELETE"] {
                    tx.execute_batch(&format!("CREATE TRIGGER {table}_no_{action} BEFORE {action} ON {table} BEGIN SELECT RAISE(ABORT, 'immutable repository data'); END;"))?;
                }
            }
            let manifest = Manifest::new([])?;
            let id = insert_object(&tx, ObjectKind::Manifest, &manifest.canonical_bytes())?;
            tx.execute(
                "INSERT INTO versions(number,manifest) VALUES(0,?1)",
                [id.to_string()],
            )?;
        } else {
            validate_format(&tx)?;
            history_in(&tx)?;
        }
        #[cfg(test)]
        test_boundary("init_before_commit");
        tx.commit()?;
        sync_directory(&directory)?;
        sync_directory(&root)?;
        #[cfg(test)]
        test_boundary("init_after_commit");
        Ok(Self { conn })
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
        validate_format(&conn)?;
        Ok(Self { conn })
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

    pub fn put_manifest(&mut self, manifest: &Manifest) -> Result<ObjectId> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for entry in manifest.entries().values() {
            read_object(&tx, entry.content, ObjectKind::Blob)?;
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

    /// Reads the complete ordered sequence and validates all reachable content.
    pub fn history(&self) -> Result<Vec<WorldVersion>> {
        let tx = self.conn.unchecked_transaction()?;
        let history = history_in(&tx)?;
        tx.commit()?;
        Ok(history)
    }

    pub fn version(&self, number: VersionNumber) -> Result<WorldVersion> {
        self.history()?
            .into_iter()
            .find(|v| v.number == number)
            .ok_or(Error::MissingVersion(number))
    }

    /// Publishes metadata only. The caller will later supply Layer, permission and
    /// policy validation. Keep the same operation ID and input after any uncertain error.
    /// Accepted and stale outcomes are durable and returned unchanged on retry.
    pub fn compare_and_advance(
        &mut self,
        operation: &OperationId,
        expected: VersionNumber,
        manifest: ObjectId,
    ) -> Result<AdvanceOutcome> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let history = history_in(&tx)?;
        let head = history
            .last()
            .ok_or_else(|| Error::CorruptRepository("missing initial World".into()))?;
        if let Some(attempt) = attempt_in(&tx, operation)? {
            if attempt.expected != expected || attempt.manifest != manifest {
                return Err(Error::OperationIdReused(operation.clone()));
            }
            manifest_in(&tx, manifest)?;
            let outcome = attempt.outcome(&history)?;
            tx.commit()?;
            return Ok(outcome);
        }
        if expected > head.number {
            return Err(Error::MissingVersion(expected));
        }
        // Revalidate the entire candidate graph inside the publication transaction.
        manifest_in(&tx, manifest)?;
        let accepted = expected == head.number;
        let result = if accepted {
            VersionNumber::new(
                head.number
                    .get()
                    .checked_add(1)
                    .ok_or_else(|| Error::InvalidInput("World version overflow".into()))?,
            )?
        } else {
            head.number
        };
        tx.execute(
            "INSERT INTO attempts(id,expected,manifest,accepted,result) VALUES(?1,?2,?3,?4,?5)",
            params![
                operation.as_str(),
                expected.sql(),
                manifest.to_string(),
                accepted,
                result.sql()
            ],
        )?;
        let outcome = if accepted {
            tx.execute(
                "INSERT INTO versions(number,manifest,operation) VALUES(?1,?2,?3)",
                params![result.sql(), manifest.to_string(), operation.as_str()],
            )?;
            AdvanceOutcome::Accepted {
                version: WorldVersion {
                    number: result,
                    manifest,
                    operation: Some(operation.clone()),
                },
            }
        } else {
            AdvanceOutcome::Stale { actual: result }
        };
        #[cfg(test)]
        test_boundary("before_commit");
        tx.commit()?;
        #[cfg(test)]
        test_boundary("after_commit");
        Ok(outcome)
    }

    /// Checks physical SQLite integrity plus every stored object's native graph and
    /// every accepted/stale operation outcome, within one consistent read snapshot.
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
        let history = history_in(&tx)?;
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
            }
        }
        let operations = tx
            .prepare("SELECT id FROM attempts ORDER BY id")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for id in &operations {
            let operation = decode_operation(id)?;
            let attempt = attempt_in(&tx, &operation)?
                .ok_or_else(|| Error::CorruptRepository("missing attempt".into()))?;
            manifest_in(&tx, attempt.manifest)?;
            attempt.outcome(&history)?;
        }
        let report = IntegrityReport {
            versions: history.len(),
            objects: objects.len(),
            attempts: operations.len(),
        };
        tx.commit()?;
        Ok(report)
    }
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

fn validate_format(conn: &Connection) -> Result<()> {
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

fn insert_object(conn: &Connection, kind: ObjectKind, bytes: &[u8]) -> Result<ObjectId> {
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

fn read_object(conn: &Connection, id: ObjectId, expected_kind: ObjectKind) -> Result<Vec<u8>> {
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

fn manifest_in(conn: &Connection, id: ObjectId) -> Result<Manifest> {
    let bytes = read_object(conn, id, ObjectKind::Manifest)?;
    let manifest = Manifest::decode(&bytes).map_err(|_| Error::CorruptObject(id))?;
    for entry in manifest.entries().values() {
        read_object(conn, entry.content, ObjectKind::Blob)?;
    }
    Ok(manifest)
}

fn decode_id(value: &str) -> Result<ObjectId> {
    ObjectId::parse(value).map_err(|_| Error::CorruptRepository("invalid stored object ID".into()))
}

fn decode_operation(value: &str) -> Result<OperationId> {
    OperationId::parse(value)
        .map_err(|_| Error::CorruptRepository("invalid stored operation ID".into()))
}

fn history_in(conn: &Connection) -> Result<Vec<WorldVersion>> {
    let rows = conn
        .prepare("SELECT number,manifest,operation FROM versions ORDER BY number")?
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if rows.is_empty() {
        return Err(Error::CorruptRepository("missing initial World".into()));
    }
    let mut history = Vec::new();
    for (index, (number, manifest, operation)) in rows.into_iter().enumerate() {
        let number = VersionNumber::decode(number)?;
        if number.get() != index as u64 {
            return Err(Error::CorruptRepository(
                "World history is not contiguous".into(),
            ));
        }
        let manifest = decode_id(&manifest)?;
        let content = manifest_in(conn, manifest)?;
        let operation = operation.as_deref().map(decode_operation).transpose()?;
        if index == 0 {
            if operation.is_some() || !content.entries().is_empty() {
                return Err(Error::CorruptRepository("invalid initial World".into()));
            }
        } else {
            let op = operation
                .as_ref()
                .ok_or_else(|| Error::CorruptRepository("missing version operation".into()))?;
            let attempt = attempt_in(conn, op)?
                .ok_or_else(|| Error::CorruptRepository("missing accepted attempt".into()))?;
            if !attempt.accepted
                || attempt.result != number
                || attempt.expected.get() != number.get() - 1
                || attempt.manifest != manifest
            {
                return Err(Error::CorruptRepository(
                    "version and operation disagree".into(),
                ));
            }
        }
        history.push(WorldVersion {
            number,
            manifest,
            operation,
        });
    }
    Ok(history)
}

struct Attempt {
    operation: OperationId,
    expected: VersionNumber,
    manifest: ObjectId,
    accepted: bool,
    result: VersionNumber,
}

impl Attempt {
    fn outcome(&self, history: &[WorldVersion]) -> Result<AdvanceOutcome> {
        let actual = history
            .iter()
            .find(|v| v.number == self.result)
            .ok_or_else(|| {
                Error::CorruptRepository("attempt refers to missing result version".into())
            })?;
        if self.accepted {
            if self.result.get() != self.expected.get() + 1
                || actual.manifest != self.manifest
                || actual.operation.as_ref() != Some(&self.operation)
            {
                return Err(Error::CorruptRepository(
                    "accepted attempt disagrees with World".into(),
                ));
            }
            Ok(AdvanceOutcome::Accepted {
                version: actual.clone(),
            })
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
        .query_row(
            "SELECT expected,manifest,accepted,result FROM attempts WHERE id=?1",
            [operation.as_str()],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?;
    row.map(|(expected, manifest, accepted, result)| {
        if accepted != 0 && accepted != 1 {
            return Err(Error::CorruptRepository("invalid attempt outcome".into()));
        }
        Ok(Attempt {
            operation: operation.clone(),
            expected: VersionNumber::decode(expected)?,
            manifest: decode_id(&manifest)?,
            accepted: accepted == 1,
            result: VersionNumber::decode(result)?,
        })
    })
    .transpose()
}

#[cfg(test)]
fn test_boundary(phase: &str) {
    use std::io::{Read, Write};
    if std::env::var("JAVELIN_TEST_BOUNDARY").as_deref() == Ok(phase) {
        println!("BOUNDARY {phase}");
        std::io::stdout().flush().unwrap();
        let mut byte = [0];
        std::io::stdin().read_exact(&mut byte).unwrap();
    }
}

