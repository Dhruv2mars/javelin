use crate::error::{Context, JavelinError, Result};
use crate::model::{EntryKind, Tree, TreeEntry};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;

const MAGIC: &[u8; 4] = b"JVL1";
const HEADER_LEN: u64 = 13;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Blob = 1,
    Tree = 2,
}

impl ObjectKind {
    fn domain(self) -> &'static [u8] {
        match self {
            Self::Blob => b"javelin:blob:v1\0",
            Self::Tree => b"javelin:tree:v1\0",
        }
    }

    fn from_byte(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Blob),
            2 => Ok(Self::Tree),
            _ => Err(JavelinError::corruption(format!(
                "unknown object kind {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ObjectStore {
    objects: PathBuf,
    temp: PathBuf,
}

#[derive(Debug)]
struct PendingObject {
    id: String,
    temp: PathBuf,
    target: PathBuf,
}

#[derive(Debug)]
pub struct ObjectBatch<'a> {
    store: &'a ObjectStore,
    pending: Vec<PendingObject>,
    pending_ids: HashSet<String>,
}

impl ObjectStore {
    pub fn new(metadata: &Path) -> Result<Self> {
        let objects = metadata.join("objects");
        let temp = metadata.join("temp");
        fs::create_dir_all(&objects).jctx("OBJECT_IO", "cannot create object directory")?;
        fs::create_dir_all(&temp).jctx("OBJECT_IO", "cannot create temp directory")?;
        Ok(Self { objects, temp })
    }

    pub fn object_path(&self, id: &str) -> Result<PathBuf> {
        if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(JavelinError::corruption(format!("invalid object ID {id}")));
        }
        Ok(self.objects.join(&id[..2]).join(&id[2..]))
    }

    pub fn batch(&self) -> ObjectBatch<'_> {
        ObjectBatch {
            store: self,
            pending: Vec::new(),
            pending_ids: HashSet::new(),
        }
    }

    pub fn put_blob(&self, bytes: &[u8]) -> Result<String> {
        let mut batch = self.batch();
        let id = batch.put_blob(bytes)?;
        batch.commit()?;
        Ok(id)
    }

    pub fn put_blob_file(&self, path: &Path) -> Result<String> {
        let mut batch = self.batch();
        let id = batch.put_blob_file(path)?;
        batch.commit()?;
        Ok(id)
    }

    pub fn put_tree(&self, tree: &Tree) -> Result<String> {
        let mut batch = self.batch();
        let id = batch.put_tree(tree)?;
        batch.commit()?;
        Ok(id)
    }

    pub fn read_blob(&self, id: &str) -> Result<Vec<u8>> {
        let (kind, bytes) = self.read(id)?;
        if kind != ObjectKind::Blob {
            return Err(JavelinError::corruption(format!(
                "object {id} is not a blob"
            )));
        }
        Ok(bytes)
    }

    pub fn read_tree(&self, id: &str) -> Result<Tree> {
        let (kind, bytes) = self.read(id)?;
        if kind != ObjectKind::Tree {
            return Err(JavelinError::corruption(format!(
                "object {id} is not a tree"
            )));
        }
        decode_tree(&bytes)
    }

    pub fn validate(&self, id: &str) -> Result<(ObjectKind, u64)> {
        let (kind, expected_length, file) = self.open_object(id)?;
        let mut decoder = zstd::stream::read::Decoder::new(file)
            .jctx("CORRUPT_OBJECT", format!("cannot decompress object {id}"))?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(kind.domain());
        let mut length = 0_u64;
        let mut buffer = vec![0_u8; 1024 * 1024];
        loop {
            let read = decoder
                .read(&mut buffer)
                .jctx("CORRUPT_OBJECT", format!("cannot read object {id}"))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            length += read as u64;
        }
        self.check_identity(id, expected_length, length, &hasher)?;
        Ok((kind, length))
    }

    pub fn info(&self, id: &str) -> Result<(ObjectKind, u64)> {
        let (kind, length, _file) = self.open_object(id)?;
        Ok((kind, length))
    }

    pub fn write_blob_to_file(&self, id: &str, path: &Path) -> Result<u64> {
        let output =
            File::create(path).jctx("VIEW_IO", format!("cannot create {}", path.display()))?;
        self.write_blob_to_writer(id, output)
    }

    pub fn write_blob_to_writer(&self, id: &str, mut output: impl Write) -> Result<u64> {
        let (kind, expected_length, file) = self.open_object(id)?;
        if kind != ObjectKind::Blob {
            return Err(JavelinError::corruption(format!(
                "object {id} is not a blob"
            )));
        }
        let mut decoder = zstd::stream::read::Decoder::new(file)
            .jctx("CORRUPT_OBJECT", format!("cannot decompress object {id}"))?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(kind.domain());
        let mut length = 0_u64;
        let mut buffer = vec![0_u8; 1024 * 1024];
        loop {
            let read = decoder
                .read(&mut buffer)
                .jctx("CORRUPT_OBJECT", format!("cannot read object {id}"))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            output
                .write_all(&buffer[..read])
                .jctx("OUTPUT_IO", "cannot write object bytes")?;
            length += read as u64;
        }
        self.check_identity(id, expected_length, length, &hasher)?;
        Ok(length)
    }

    fn read(&self, id: &str) -> Result<(ObjectKind, Vec<u8>)> {
        let (kind, expected_length, file) = self.open_object(id)?;
        let mut decoder = zstd::stream::read::Decoder::new(file)
            .jctx("CORRUPT_OBJECT", format!("cannot decompress object {id}"))?;
        let mut bytes = Vec::new();
        decoder
            .read_to_end(&mut bytes)
            .jctx("CORRUPT_OBJECT", format!("cannot read object {id}"))?;
        if bytes.len() as u64 != expected_length {
            return Err(JavelinError::corruption(format!(
                "object {id} length mismatch"
            )));
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(kind.domain());
        hasher.update(&bytes);
        if hasher.finalize().to_hex().as_str() != id {
            return Err(JavelinError::corruption(format!(
                "object {id} content hash mismatch"
            )));
        }
        Ok((kind, bytes))
    }

    fn open_object(&self, id: &str) -> Result<(ObjectKind, u64, File)> {
        let path = self.object_path(id)?;
        let mut file = File::open(&path).jctx(
            "MISSING_OBJECT",
            format!("missing object {id} at {}", path.display()),
        )?;
        let mut header = [0_u8; HEADER_LEN as usize];
        file.read_exact(&mut header)
            .jctx("CORRUPT_OBJECT", format!("truncated object {id}"))?;
        if &header[..4] != MAGIC {
            return Err(JavelinError::corruption(format!(
                "invalid object header {id}"
            )));
        }
        let kind = ObjectKind::from_byte(header[4])?;
        let expected_length = u64::from_be_bytes(header[5..13].try_into().unwrap());
        Ok((kind, expected_length, file))
    }

    fn check_identity(
        &self,
        id: &str,
        expected_length: u64,
        actual_length: u64,
        hasher: &blake3::Hasher,
    ) -> Result<()> {
        if actual_length != expected_length {
            return Err(JavelinError::corruption(format!(
                "object {id} length mismatch"
            )));
        }
        if hasher.clone().finalize().to_hex().as_str() != id {
            return Err(JavelinError::corruption(format!(
                "object {id} content hash mismatch"
            )));
        }
        Ok(())
    }

    pub fn all_ids(&self) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        if !self.objects.exists() {
            return Ok(ids);
        }
        for shard in fs::read_dir(&self.objects).jctx("OBJECT_IO", "cannot list object shards")? {
            let shard = shard.jctx("OBJECT_IO", "cannot read object shard")?;
            if !shard.path().is_dir() {
                continue;
            }
            let prefix = shard.file_name().to_string_lossy().into_owned();
            for object in fs::read_dir(shard.path()).jctx("OBJECT_IO", "cannot list objects")? {
                let object = object.jctx("OBJECT_IO", "cannot read object entry")?;
                if object.path().is_file() {
                    ids.push(format!(
                        "{}{}",
                        prefix,
                        object.file_name().to_string_lossy()
                    ));
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let path = self.object_path(id)?;
        if path.exists() {
            fs::remove_file(&path).jctx(
                "OBJECT_IO",
                format!("cannot remove unreachable object {id}"),
            )?;
        }
        Ok(())
    }
}

impl ObjectBatch<'_> {
    pub fn put_blob(&mut self, bytes: &[u8]) -> Result<String> {
        self.put_reader(ObjectKind::Blob, Cursor::new(bytes))
    }

    pub fn put_blob_file(&mut self, path: &Path) -> Result<String> {
        let file = File::open(path).jctx("OBJECT_IO", format!("cannot open {}", path.display()))?;
        self.put_reader(ObjectKind::Blob, BufReader::new(file))
    }

    pub fn put_tree(&mut self, tree: &Tree) -> Result<String> {
        let bytes = encode_tree(tree)?;
        self.put_reader(ObjectKind::Tree, Cursor::new(bytes))
    }

    fn put_reader(&mut self, kind: ObjectKind, mut reader: impl Read + Seek) -> Result<String> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(kind.domain());
        let mut length = 0_u64;
        let mut buffer = vec![0_u8; 1024 * 1024];
        loop {
            let read = reader
                .read(&mut buffer)
                .jctx("OBJECT_IO", "cannot read object input")?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            length += read as u64;
        }
        let id = hasher.finalize().to_hex().to_string();
        let target = self.store.object_path(&id)?;
        if target.exists() || self.pending_ids.contains(&id) {
            return Ok(id);
        }
        reader
            .seek(SeekFrom::Start(0))
            .jctx("OBJECT_IO", "cannot rewind object input")?;

        crate::fault::hit("before_object_temp_write");
        let temp = self
            .store
            .temp
            .join(format!("object-{}.tmp", ulid::Ulid::new()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&temp)
            .jctx("OBJECT_IO", "cannot create temporary object")?;
        file.write_all(MAGIC)
            .and_then(|_| file.write_all(&[kind as u8]))
            .and_then(|_| file.write_all(&0_u64.to_be_bytes()))
            .jctx("OBJECT_IO", "cannot write object header")?;
        {
            let mut encoder = zstd::stream::write::Encoder::new(&mut file, 3)
                .jctx("OBJECT_IO", "cannot create zstd encoder")?;
            loop {
                let read = reader
                    .read(&mut buffer)
                    .jctx("OBJECT_IO", "cannot read object input")?;
                if read == 0 {
                    break;
                }
                encoder
                    .write_all(&buffer[..read])
                    .jctx("OBJECT_IO", "cannot compress object")?;
            }
            encoder
                .finish()
                .jctx("OBJECT_IO", "cannot finish object compression")?;
        }
        file.seek(SeekFrom::Start(5))
            .and_then(|_| file.write_all(&length.to_be_bytes()))
            .jctx("OBJECT_IO", "cannot finalize object")?;
        drop(file);

        self.pending_ids.insert(id.clone());
        self.pending.push(PendingObject {
            id: id.clone(),
            temp,
            target,
        });
        Ok(id)
    }

    pub fn commit(self) -> Result<()> {
        let temp_paths = self
            .pending
            .iter()
            .map(|object| object.temp.clone())
            .collect::<Vec<_>>();
        sync_paths_in_parallel(&temp_paths, false)?;
        if !temp_paths.is_empty() {
            crate::fault::hit("after_object_fsync");
        }

        let mut parents = HashSet::new();
        let mut created_shard = false;
        for object in &self.pending {
            if object.target.exists() {
                fs::remove_file(&object.temp)
                    .jctx("OBJECT_IO", "cannot remove raced temp object")?;
                continue;
            }
            let parent = object
                .target
                .parent()
                .ok_or_else(|| JavelinError::corruption("object path has no parent"))?;
            created_shard |= !parent.exists();
            fs::create_dir_all(parent).jctx("OBJECT_IO", "cannot create object shard")?;
            crate::fault::hit("before_object_rename");
            match fs::rename(&object.temp, &object.target) {
                Ok(()) => {
                    parents.insert(parent.to_path_buf());
                }
                Err(error) if object.target.exists() => {
                    fs::remove_file(&object.temp)
                        .jctx("OBJECT_IO", "cannot remove raced temp object")?;
                    let _ = error;
                }
                Err(error) => {
                    return Err(JavelinError::new(7, "OBJECT_IO", "cannot install object")
                        .details(
                            serde_json::json!({"cause": error.to_string(), "id": object.id}),
                        ));
                }
            }
            crate::fault::hit("after_object_rename");
        }

        let mut parent_paths = parents.into_iter().collect::<Vec<_>>();
        parent_paths.sort();
        sync_paths_in_parallel(&parent_paths, true)?;
        if created_shard {
            sync_dir(&self.store.objects)?;
        }
        Ok(())
    }
}

impl Drop for ObjectBatch<'_> {
    fn drop(&mut self) {
        for object in &self.pending {
            let _ = fs::remove_file(&object.temp);
        }
    }
}

fn sync_paths_in_parallel(paths: &[PathBuf], directories: bool) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let workers = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(paths.len());
    let chunk_size = paths.len().div_ceil(workers);
    thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for chunk in paths.chunks(chunk_size) {
            handles.push(scope.spawn(move || -> Result<()> {
                for path in chunk {
                    if directories {
                        sync_dir(path)?;
                    } else {
                        File::open(path)
                            .and_then(|file| file.sync_all())
                            .jctx("OBJECT_IO", format!("cannot sync {}", path.display()))?;
                    }
                }
                Ok(())
            }));
        }
        for handle in handles {
            handle
                .join()
                .map_err(|_| JavelinError::corruption("object sync worker panicked"))??;
        }
        Ok(())
    })
}

#[cfg(unix)]
fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .jctx("OBJECT_IO", format!("cannot sync {}", path.display()))
}

#[cfg(not(unix))]
fn sync_dir(_path: &Path) -> Result<()> {
    // Rust's standard library cannot open Windows directories for `sync_all`.
    // The object file itself is synced before its atomic rename.
    Ok(())
}

pub fn encode_tree(tree: &Tree) -> Result<Vec<u8>> {
    let mut entries = tree.entries.clone();
    entries.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    let mut previous: Option<&str> = None;
    let mut out = Vec::new();
    out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for entry in &entries {
        if previous == Some(entry.path.as_str()) {
            return Err(JavelinError::corruption(format!(
                "duplicate tree path {}",
                entry.path
            )));
        }
        previous = Some(&entry.path);
        let path = entry.path.as_bytes();
        out.extend_from_slice(&(path.len() as u32).to_be_bytes());
        out.extend_from_slice(path);
        out.push(match entry.kind {
            EntryKind::File => 1,
            EntryKind::Directory => 2,
            EntryKind::Symlink => 3,
        });
        out.push(u8::from(entry.executable));
        let object = entry.object_id.as_deref().unwrap_or("").as_bytes();
        out.extend_from_slice(&(object.len() as u16).to_be_bytes());
        out.extend_from_slice(object);
    }
    Ok(out)
}

pub fn decode_tree(bytes: &[u8]) -> Result<Tree> {
    let mut cursor = Cursor::new(bytes);
    let count = read_u32(&mut cursor)?;
    let mut entries = Vec::with_capacity(count as usize);
    let mut previous = None::<String>;
    for _ in 0..count {
        let path_length = read_u32(&mut cursor)? as usize;
        let mut path_bytes = vec![0_u8; path_length];
        cursor
            .read_exact(&mut path_bytes)
            .jctx("CORRUPT_TREE", "truncated tree path")?;
        let path = String::from_utf8(path_bytes)
            .map_err(|_| JavelinError::corruption("tree contains non-UTF-8 path"))?;
        crate::paths::validate_relative(&path)?;
        if previous.as_ref().is_some_and(|value| value >= &path) {
            return Err(JavelinError::corruption(
                "tree paths are not strictly sorted",
            ));
        }
        previous = Some(path.clone());
        let mut kind = [0_u8; 1];
        let mut executable = [0_u8; 1];
        cursor
            .read_exact(&mut kind)
            .and_then(|_| cursor.read_exact(&mut executable))
            .jctx("CORRUPT_TREE", "truncated tree entry")?;
        let object_length = read_u16(&mut cursor)? as usize;
        let mut object_bytes = vec![0_u8; object_length];
        cursor
            .read_exact(&mut object_bytes)
            .jctx("CORRUPT_TREE", "truncated tree object ID")?;
        let object = String::from_utf8(object_bytes)
            .map_err(|_| JavelinError::corruption("invalid tree object ID"))?;
        entries.push(TreeEntry {
            path,
            kind: match kind[0] {
                1 => EntryKind::File,
                2 => EntryKind::Directory,
                3 => EntryKind::Symlink,
                value => {
                    return Err(JavelinError::corruption(format!(
                        "unknown tree entry kind {value}"
                    )));
                }
            },
            object_id: (!object.is_empty()).then_some(object),
            executable: executable[0] != 0,
        });
    }
    if cursor.position() != bytes.len() as u64 {
        return Err(JavelinError::corruption("tree has trailing bytes"));
    }
    Ok(Tree { entries })
}

fn read_u32(reader: &mut impl Read) -> Result<u32> {
    let mut bytes = [0_u8; 4];
    reader
        .read_exact(&mut bytes)
        .jctx("CORRUPT_TREE", "truncated tree integer")?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_u16(reader: &mut impl Read) -> Result<u16> {
    let mut bytes = [0_u8; 2];
    reader
        .read_exact(&mut bytes)
        .jctx("CORRUPT_TREE", "truncated tree integer")?;
    Ok(u16::from_be_bytes(bytes))
}
