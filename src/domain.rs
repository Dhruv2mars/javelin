use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ObjectId([u8; 32]);

impl ObjectId {
    pub fn parse(value: &str) -> Result<Self> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::InvalidInput(
                "object ID must be 64 lowercase hex digits".into(),
            ));
        }
        let mut bytes = [0; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16)
                .map_err(|_| Error::InvalidInput("invalid object ID".into()))?;
        }
        Ok(Self(bytes))
    }

    pub(crate) fn hash(kind: ObjectKind, bytes: &[u8]) -> Self {
        let mut hash = Sha256::new();
        hash.update(b"javelin-object-v1\0");
        hash.update([kind as u8]);
        hash.update(bytes);
        Self(hash.finalize().into())
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Serialize for ObjectId {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ObjectKind {
    Blob = 1,
    Manifest = 2,
}

impl ObjectKind {
    pub(crate) fn decode(kind: i64) -> Result<Self> {
        match kind {
            1 => Ok(Self::Blob),
            2 => Ok(Self::Manifest),
            _ => Err(Error::CorruptRepository("unknown object kind".into())),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct VersionNumber(u64);

impl VersionNumber {
    pub fn new(value: u64) -> Result<Self> {
        if value > i64::MAX as u64 {
            return Err(Error::InvalidInput(
                "World version exceeds storage range".into(),
            ));
        }
        Ok(Self(value))
    }
    pub fn get(self) -> u64 {
        self.0
    }
    pub(crate) fn sql(self) -> i64 {
        self.0 as i64
    }
    pub(crate) fn decode(value: i64) -> Result<Self> {
        if value < 0 {
            return Err(Error::CorruptRepository("negative World version".into()));
        }
        Ok(Self(value as u64))
    }
}

impl fmt::Display for VersionNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RepoPath(String);

impl RepoPath {
    pub fn parse(value: &str) -> Result<Self> {
        if value.is_empty()
            || value.contains(['\\', ':', '\0'])
            || value.split('/').any(|p| {
                p.is_empty() || p == "." || p == ".." || p.eq_ignore_ascii_case(".javelin")
            })
        {
            return Err(Error::InvalidInput(format!(
                "invalid repository-relative path {value:?}"
            )));
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct OperationId(String);

impl OperationId {
    pub fn parse(value: &str) -> Result<Self> {
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            return Err(Error::InvalidInput(
                "operation ID must contain 1..128 ASCII letters, digits, '-', '_' or '.'".into(),
            ));
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OperationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FileEntry {
    pub content: ObjectId,
    pub executable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Manifest(BTreeMap<RepoPath, FileEntry>);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EncodedEntry {
    content: String,
    executable: bool,
}

impl Manifest {
    pub fn new(entries: impl IntoIterator<Item = (RepoPath, FileEntry)>) -> Result<Self> {
        let mut map = BTreeMap::new();
        for (path, entry) in entries {
            if map.insert(path, entry).is_some() {
                return Err(Error::InvalidInput("duplicate manifest path".into()));
            }
        }
        for path in map.keys() {
            for (index, _) in path.as_str().match_indices('/') {
                if map.contains_key(&RepoPath(path.as_str()[..index].into())) {
                    return Err(Error::InvalidInput(
                        "file and descendant paths collide".into(),
                    ));
                }
            }
        }
        Ok(Self(map))
    }
    pub fn entries(&self) -> &BTreeMap<RepoPath, FileEntry> {
        &self.0
    }

    pub fn canonical_bytes(&self) -> Vec<u8> {
        let entries: BTreeMap<_, _> = self
            .0
            .iter()
            .map(|(path, entry)| {
                (
                    path.as_str(),
                    EncodedEntry {
                        content: entry.content.to_string(),
                        executable: entry.executable,
                    },
                )
            })
            .collect();
        // Serializing a string-keyed map with string and bool fields cannot fail.
        serde_json::to_vec(&entries).expect("manifest encoding")
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let entries: BTreeMap<String, EncodedEntry> = serde_json::from_slice(bytes)
            .map_err(|e| Error::CorruptRepository(format!("invalid manifest encoding: {e}")))?;
        let entries = entries
            .into_iter()
            .map(|(path, entry)| {
                Ok((
                    RepoPath::parse(&path)?,
                    FileEntry {
                        content: ObjectId::parse(&entry.content)?,
                        executable: entry.executable,
                    },
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let manifest = Self::new(entries)?;
        if manifest.canonical_bytes() != bytes {
            return Err(Error::CorruptRepository(
                "noncanonical manifest encoding".into(),
            ));
        }
        Ok(manifest)
    }
}
