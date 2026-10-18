use crate::{Result, corpus::Blob, platform};
use javelin::{ObjectId, Repository};
use rusqlite::{Connection, params};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Real,
    Tuned,
    Files,
}
#[derive(Clone)]
pub struct Store {
    pub root: PathBuf,
    pub kind: Kind,
    pub durable: bool,
    pub page: usize,
    pub incremental: bool,
}

const SCHEMA: &str = "CREATE TABLE objects (
 id TEXT PRIMARY KEY CHECK(length(id)=64),
 kind INTEGER NOT NULL CHECK(kind IN (1,2,3)), bytes BLOB NOT NULL) STRICT;
 CREATE TRIGGER objects_no_UPDATE BEFORE UPDATE ON objects BEGIN SELECT RAISE(ABORT,'immutable'); END;
 CREATE TRIGGER objects_no_DELETE BEFORE DELETE ON objects BEGIN SELECT RAISE(ABORT,'immutable'); END;";

impl Store {
    pub fn db(&self) -> PathBuf {
        if self.kind == Kind::Real {
            self.root.join(".javelin/repository.sqlite3")
        } else {
            self.root.join("objects.sqlite3")
        }
    }
    pub fn object(&self, id: ObjectId) -> PathBuf {
        let id = id.to_string();
        self.root.join("objects").join(&id[..2]).join(&id[2..])
    }
    pub fn conn(&self) -> Result<Connection> {
        let c = Connection::open(self.db())?;
        c.execute_batch(&format!("PRAGMA mmap_size=2147483648; PRAGMA cache_size=-65536; PRAGMA fullfsync=OFF; PRAGMA synchronous={};", if self.durable {"FULL"} else {"OFF"}))?;
        Ok(c)
    }
    pub fn init(&self) -> Result<()> {
        fs::create_dir(&self.root)?;
        match self.kind {
            Kind::Real => {
                Repository::init(&self.root)?;
            }
            Kind::Tuned => {
                let c = Connection::open(self.db())?;
                c.execute_batch(&format!(
                    "PRAGMA page_size={}; PRAGMA auto_vacuum=INCREMENTAL; PRAGMA journal_mode=WAL;",
                    self.page
                ))?;
                c.execute_batch(SCHEMA)?;
            }
            Kind::Files => {
                fs::create_dir(self.root.join("objects"))?;
                fs::create_dir(self.root.join("tmp"))?;
                // Precreate all fanouts; initialize and durably publish store topology.
                for i in 0..256 {
                    fs::create_dir(self.root.join("objects").join(format!("{i:02x}")))?;
                }
                if self.durable {
                    File::open(self.root.join("objects"))?.sync_all()?;
                    File::open(&self.root)?.sync_all()?;
                    File::open(self.root.parent().unwrap())?.sync_all()?;
                }
            }
        }
        Ok(())
    }
    /// Hashing is inside the measured region, even though read IDs are precomputed.
    /// Limit is checked after each committed object for the real-store 100k budget.
    pub fn ingest(&self, blobs: &[Blob], limit: Option<f64>) -> Result<bool> {
        let t = Instant::now();
        match self.kind {
            Kind::Real => {
                let mut repo = Repository::open(&self.root)?;
                for b in blobs {
                    assert_eq!(repo.put_blob(&b.bytes)?, b.id);
                    if limit.is_some_and(|s| t.elapsed().as_secs_f64() > s) {
                        return Ok(false);
                    }
                }
            }
            Kind::Tuned => {
                let mut c = self.conn()?;
                for batch in blobs.chunks(1000) {
                    let tx =
                        c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                    {
                        let mut insert = tx.prepare("INSERT INTO objects(id,kind,bytes) VALUES(?1,1,?2) ON CONFLICT(id) DO NOTHING")?;
                        for b in batch {
                            let id = ObjectId::for_blob(&b.bytes);
                            insert.execute(params![id.to_string(), b.bytes])?;
                        }
                    }
                    tx.commit()?;
                }
                // Include draining WAL in ingest, so deferred checkpoint work is not hidden.
                c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
            }
            Kind::Files => {
                for batch in blobs.chunks(1000) {
                    let mut dirs = BTreeSet::new();
                    for (i, b) in batch.iter().enumerate() {
                        let target = self.object(ObjectId::for_blob(&b.bytes));
                        if target.try_exists()? {
                            continue;
                        }
                        let temp = self
                            .root
                            .join("tmp")
                            .join(format!("{}-{i}", std::process::id()));
                        let mut f = OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&temp)?;
                        f.write_all(&b.bytes)?;
                        f.set_permissions(fs::Permissions::from_mode(0o444))?;
                        if self.durable {
                            f.sync_all()?;
                        }
                        fs::rename(&temp, &target)?;
                        dirs.insert(target.parent().unwrap().to_path_buf());
                    }
                    if self.durable {
                        for d in dirs {
                            File::open(d)?.sync_all()?;
                        }
                        File::open(self.root.join("tmp"))?.sync_all()?;
                    }
                }
            }
        }
        Ok(true)
    }
    pub fn reader(&self) -> Result<Reader> {
        Ok(match self.kind {
            Kind::Real => Reader::Real(Repository::open(&self.root)?),
            Kind::Tuned => Reader::Tuned(self.conn()?, self.incremental),
            Kind::Files => Reader::Files(self.clone()),
        })
    }
    pub fn materialize(&self, blobs: &[Blob], dst: &Path, clones: bool) -> Result<()> {
        let reader = self.reader()?;
        fs::create_dir(dst)?;
        let mut last = PathBuf::new();
        for b in blobs {
            let to = dst.join(&b.path);
            let parent = to.parent().unwrap();
            if parent != last {
                fs::create_dir_all(parent)?;
                last = parent.to_path_buf();
            }
            if self.kind == Kind::Files {
                if clones {
                    platform::clone_file(&self.object(b.id), &to)?;
                } else {
                    platform::copy_bytes(&self.object(b.id), &to)?;
                }
            } else {
                fs::write(to, reader.read(b.id)?)?;
            }
        }
        Ok(())
    }
    pub fn evict(&self, ids: &[ObjectId]) -> Result<()> {
        match self.kind {
            Kind::Tuned => {
                let mut c = self.conn()?;
                let tx = c.transaction()?;
                tx.execute_batch("DROP TRIGGER objects_no_DELETE")?;
                {
                    let mut delete = tx.prepare("DELETE FROM objects WHERE id=?1")?;
                    for id in ids {
                        delete.execute([id.to_string()])?;
                    }
                }
                tx.execute_batch("CREATE TRIGGER objects_no_DELETE BEFORE DELETE ON objects BEGIN SELECT RAISE(ABORT,'immutable'); END;")?;
                tx.commit()?;
                c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
            }
            Kind::Files => {
                let mut dirs = BTreeSet::new();
                for id in ids {
                    let p = self.object(*id);
                    fs::remove_file(&p)?;
                    dirs.insert(p.parent().unwrap().to_path_buf());
                }
                if self.durable {
                    for d in dirs {
                        File::open(d)?.sync_all()?;
                    }
                }
            }
            Kind::Real => return Err("real immutable store is not evicted".into()),
        }
        Ok(())
    }
}

pub enum Reader {
    Real(Repository),
    Tuned(Connection, bool),
    Files(Store),
}
impl Reader {
    pub fn read(&self, id: ObjectId) -> Result<Vec<u8>> {
        Ok(match self {
            Self::Real(r) => r.read_blob(id)?,
            Self::Tuned(c, incremental) => sqlite_read(c, id, *incremental)?,
            Self::Files(s) => fs::read(s.object(id))?,
        })
    }
}

pub fn sqlite_read(c: &Connection, id: ObjectId, incremental: bool) -> Result<Vec<u8>> {
    if incremental {
        let rowid: i64 = c
            .prepare_cached("SELECT rowid FROM objects WHERE id=?1 AND kind=1")?
            .query_row([id.to_string()], |r| r.get(0))?;
        let mut blob = c.blob_open("main", "objects", "bytes", rowid, true)?;
        let mut bytes = Vec::with_capacity(blob.len());
        blob.read_to_end(&mut bytes)?;
        Ok(bytes)
    } else {
        Ok(
            c.prepare_cached("SELECT bytes FROM objects WHERE id=?1 AND kind=1")?
                .query_row([id.to_string()], |r| r.get(0))?,
        )
    }
}
