//! Hidden entries in committed form.
//!
//! A World version's tree and changeset are committed: an entry outside the public
//! audience is a `FileEntry::Hidden` holding its audience and a salted commitment
//! `SHA-256("javelin-seal-v1\0" || salt || content id || kind)` instead of what it
//! holds. Version IDs and roots are computed over that form, so every reader gets the
//! same IDs and can verify the whole chain, and nobody can confirm a guess of hidden
//! content without the salt. The salt and content ID are an `Opening`, which a
//! repository keeps outside its trees and serves only to the entry's audience.
//!
//! A hidden directory is one entry; its opening names the root of its own committed
//! tree, which can hold nested hidden entries for narrower audiences.
//!
//! `seal` turns a plain change into committed form with fresh salts. `open` turns
//! committed changes back into the changes to this repository's view: the committed
//! tree with every hidden entry it can open replaced by what it hides. Both check the
//! version's policy, so a plain entry the policy hides is refused.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::{
    Audience, Error, FileEntry, ObjectId, Policy, RepoPath, Result, SparseChanges,
    tree::{self, Node, Store, Trees},
};

/// Salt bytes per commitment. See `Opening`.
pub const SALT_BYTES: usize = 32;

/// What a hidden entry holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum SealedKind {
    Regular = 1,
    Executable = 2,
    Symlink = 3,
    /// The content is the root chunk of the directory's committed tree.
    Directory = 4,
}

impl SealedKind {
    pub fn decode(value: i64) -> Result<Self> {
        match value {
            1 => Ok(Self::Regular),
            2 => Ok(Self::Executable),
            3 => Ok(Self::Symlink),
            4 => Ok(Self::Directory),
            _ => Err(Error::InvalidInput("unknown hidden entry kind".into())),
        }
    }
}

/// The salt and content behind one commitment.
///
/// The salt is 32 bytes from the operating system's CSPRNG, fresh for every sealed
/// entry. Hidden files are often small and guessable (a `.env` with one short token),
/// so the commitment must hide content even when its entropy is low: with a uniform
/// 256-bit salt, confirming any guess means finding the salt, about 2^256 hashes per
/// guess, and testing one guess against every commitment in a repository at once does
/// not help. 128 bits would already be out of reach; 32 bytes matches the hash and ID
/// size, leaves margin against multi-target attacks, and costs nothing that matters. A
/// fresh salt also makes equal content at two paths, or a file reverted to old content,
/// commit to unrelated values. Every input has a fixed length, so the encoding is
/// injective, and SHA-256 collision resistance binds the commitment to one opening.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Opening {
    pub salt: [u8; SALT_BYTES],
    pub kind: SealedKind,
    pub content: ObjectId,
}

impl Opening {
    /// A new opening with a fresh salt.
    pub fn new(kind: SealedKind, content: ObjectId) -> Result<Self> {
        let mut salt = [0; SALT_BYTES];
        getrandom::fill(&mut salt)
            .map_err(|e| Error::Unsupported(format!("no system randomness for a salt: {e}")))?;
        Ok(Self {
            salt,
            kind,
            content,
        })
    }

    pub fn commitment(&self) -> ObjectId {
        let mut hash = Sha256::new();
        hash.update(b"javelin-seal-v1\0");
        hash.update(self.salt);
        hash.update(self.content.as_bytes());
        hash.update([self.kind as u8]);
        ObjectId::from_bytes(hash.finalize().into())
    }

    /// The plain file entry this opens to; None for a directory.
    pub fn file(&self) -> Option<FileEntry> {
        match self.kind {
            SealedKind::Regular | SealedKind::Executable => Some(FileEntry::Regular {
                content: self.content,
                executable: self.kind == SealedKind::Executable,
            }),
            SealedKind::Symlink => Some(FileEntry::Symlink {
                target: self.content,
            }),
            SealedKind::Directory => None,
        }
    }

    fn of_file(entry: &FileEntry) -> Option<(SealedKind, ObjectId)> {
        match entry {
            FileEntry::Regular {
                content,
                executable,
            } => Some((
                if *executable {
                    SealedKind::Executable
                } else {
                    SealedKind::Regular
                },
                *content,
            )),
            FileEntry::Symlink { target } => Some((SealedKind::Symlink, *target)),
            FileEntry::Hidden { .. } => None,
        }
    }
}

/// The openings a repository holds.
pub(crate) trait Openings {
    /// The opening of `commitment` and its audience, checked against the commitment.
    fn opening(&self, commitment: ObjectId) -> Result<Option<(Opening, Audience)>>;
}

/// The policy of the version whose committed root is `root`: the `.javelinpolicy`
/// file, read through `read`. A missing file is the empty policy.
pub(crate) fn policy_at<S: Store>(
    trees: &Trees<S>,
    root: ObjectId,
    read: impl FnOnce(ObjectId) -> Result<Vec<u8>>,
) -> Result<Policy> {
    match trees.lookup(root, &RepoPath(crate::audience::POLICY_PATH.into()))? {
        None => Ok(Policy::default()),
        Some(Node::File(FileEntry::Regular { content, .. })) => Policy::parse(&read(content)?),
        Some(_) => Err(Error::InvalidInput(format!(
            "{} must be a regular file",
            crate::audience::POLICY_PATH
        ))),
    }
}

fn join(prefix: &str, rel: &RepoPath) -> RepoPath {
    RepoPath(format!("{prefix}{}", rel.as_str()))
}

fn relative(prefix: &str, full: &str) -> RepoPath {
    RepoPath(full[prefix.len()..].into())
}

/// The first hidden entry at or below `full`'s component after `prefix`.
fn next_hidden(policy: &Policy, prefix: &str, full: &str) -> Option<(usize, Audience)> {
    policy
        .hidden_along(full)
        .into_iter()
        .find(|(end, _)| *end >= prefix.len())
}

fn nonconforming(path: &str) -> Error {
    Error::InvalidInput(format!(
        "entry {path:?} disagrees with the version's {}",
        crate::audience::POLICY_PATH
    ))
}

/// Seals plain changes. Committed chunks of hidden directories go to `committed`.
pub(crate) struct Sealer<'a, C: Store, V: Store, O: Openings> {
    pub committed: &'a Trees<C>,
    pub view: &'a Trees<V>,
    pub openings: &'a O,
    pub policy: &'a Policy,
    /// Openings created so far, with their audiences.
    pub created: Vec<(Opening, Audience)>,
}

impl<C: Store, V: Store, O: Openings> Sealer<'_, C, V, O> {
    /// The committed changes for `plain`, a change canonical against the parent's view
    /// whose new view root is `view`. `committed` is the parent's committed root, or
    /// None to seal `plain` as a whole snapshot. Work is O(change x depth) plus the
    /// rules matched per path; a hidden directory reseals only its changed paths.
    pub fn seal(
        &mut self,
        committed: Option<ObjectId>,
        view: ObjectId,
        plain: &SparseChanges,
    ) -> Result<SparseChanges> {
        let changes: Vec<_> = plain
            .entries()
            .iter()
            .map(|(p, e)| (p, e.as_ref()))
            .collect();
        SparseChanges::new(self.level("", committed, view, &changes)?)
    }

    fn level(
        &mut self,
        prefix: &str,
        dir: Option<ObjectId>,
        view: ObjectId,
        plain: &[(&RepoPath, Option<&FileEntry>)],
    ) -> Result<BTreeMap<RepoPath, Option<FileEntry>>> {
        let mut out = BTreeMap::new();
        type Group<'a> = (Audience, Vec<(&'a RepoPath, Option<&'a FileEntry>)>);
        let mut groups: BTreeMap<String, Group<'_>> = BTreeMap::new();
        for &(path, entry) in plain {
            if entry.is_some_and(FileEntry::is_hidden) {
                return Err(Error::InvalidInput(
                    "a Layer cannot write a hidden entry".into(),
                ));
            }
            match next_hidden(self.policy, prefix, path.as_str()) {
                None => {
                    out.insert(relative(prefix, path.as_str()), entry.cloned());
                }
                Some((end, audience)) => groups
                    .entry(path.as_str()[..end].to_owned())
                    .or_insert_with(|| (audience, Vec::new()))
                    .1
                    .push((path, entry)),
            }
        }
        for (hidden, (audience, changes)) in groups {
            let name = relative(prefix, &hidden);
            let path = RepoPath(hidden.clone());
            let sealed = match self.view.lookup(view, &path)? {
                None => None,
                Some(Node::File(file)) => {
                    let (kind, content) =
                        Opening::of_file(&file).ok_or_else(|| Error::HiddenPath(path.clone()))?;
                    Some(self.sealed(kind, content, audience)?)
                }
                Some(Node::Dir(_)) => {
                    let before = match dir {
                        Some(dir) => self.committed.lookup(dir, &name)?,
                        None => None,
                    };
                    let sub = match before {
                        Some(Node::File(FileEntry::Hidden { commitment, .. })) => {
                            match self.openings.opening(commitment)? {
                                Some((opening, _)) => (opening.kind == SealedKind::Directory)
                                    .then_some(opening.content),
                                None => return Err(Error::HiddenPath(path)),
                            }
                        }
                        Some(Node::Dir(_)) => return Err(nonconforming(&hidden)),
                        _ => None,
                    };
                    let below: Vec<_> = changes
                        .iter()
                        .filter(|(p, _)| p.as_str().len() > hidden.len())
                        .copied()
                        .collect();
                    let inner = self.level(&format!("{hidden}/"), sub, view, &below)?;
                    let root = self
                        .committed
                        .apply(sub.unwrap_or_else(tree::empty), &SparseChanges::new(inner)?)?;
                    Some(self.sealed(SealedKind::Directory, root, audience)?)
                }
            };
            out.insert(name, sealed);
        }
        Ok(out)
    }

    fn sealed(
        &mut self,
        kind: SealedKind,
        content: ObjectId,
        audience: Audience,
    ) -> Result<FileEntry> {
        let opening = Opening::new(kind, content)?;
        let entry = FileEntry::Hidden {
            commitment: opening.commitment(),
            audience: audience.clone(),
        };
        self.created.push((opening, audience));
        Ok(entry)
    }
}

/// Opens committed changes into view changes.
pub(crate) struct Opener<'a, C: Store, V: Store, O: Openings> {
    pub committed: &'a Trees<C>,
    pub view: &'a Trees<V>,
    pub openings: &'a O,
    pub policy: &'a Policy,
}

enum Opened {
    File(FileEntry),
    Dir(ObjectId),
}

impl<C: Store, V: Store, O: Openings> Opener<'_, C, V, O> {
    /// The view root after committed changes `changes` (from committed root `from`, whose
    /// view is `view`). Every changed entry, and every entry of an opened directory that
    /// changed, must agree with the policy: plain where it is public, hidden with its
    /// audience exactly where the policy starts a hidden entry. Pass the empty roots to
    /// open a whole snapshot.
    pub fn open(
        &self,
        from: ObjectId,
        view: ObjectId,
        changes: &SparseChanges,
    ) -> Result<ObjectId> {
        let mut out = BTreeMap::new();
        self.level("", Some(from), Some(view), changes, &mut out)?;
        self.view.apply(view, &SparseChanges::new(out)?)
    }

    fn level(
        &self,
        prefix: &str,
        cdir: Option<ObjectId>,
        vdir: Option<ObjectId>,
        changes: &SparseChanges,
        out: &mut BTreeMap<RepoPath, Option<FileEntry>>,
    ) -> Result<()> {
        for (rel, entry) in changes.entries() {
            // A hidden entry that replaced a plain directory carries deletions of the
            // directory's files; opening the entry accounts for them.
            if rel.ancestors().any(|a| {
                matches!(
                    changes.entries().get(&a),
                    Some(Some(FileEntry::Hidden { .. }))
                )
            }) {
                continue;
            }
            let full = join(prefix, rel);
            let first = next_hidden(self.policy, prefix, full.as_str());
            match entry {
                Some(FileEntry::Hidden { audience, .. })
                    if first.as_ref() != Some(&(full.as_str().len(), audience.clone())) =>
                {
                    return Err(nonconforming(full.as_str()));
                }
                Some(FileEntry::Hidden { .. }) | None => {}
                Some(_) if first.is_some() => return Err(nonconforming(full.as_str())),
                Some(_) => {}
            }
            let new = match entry {
                None => None,
                Some(FileEntry::Hidden {
                    commitment,
                    audience,
                }) => match self.openings.opening(*commitment)? {
                    Some((opening, held)) => {
                        if held != *audience {
                            return Err(Error::CorruptRepository(format!(
                                "opening of {commitment} has another audience"
                            )));
                        }
                        Some(match opening.file() {
                            Some(file) => Opened::File(file),
                            None => Opened::Dir(opening.content),
                        })
                    }
                    None => Some(Opened::File(FileEntry::Hidden {
                        commitment: *commitment,
                        audience: audience.clone(),
                    })),
                },
                Some(file) => Some(Opened::File(file.clone())),
            };
            let before = match vdir {
                Some(dir) => self.view.lookup(dir, rel)?,
                None => None,
            };
            match new {
                Some(Opened::Dir(sub)) => {
                    let old = match cdir {
                        Some(dir) => match self.committed.lookup(dir, rel)? {
                            Some(Node::Dir(id)) => Some(id),
                            Some(Node::File(FileEntry::Hidden { commitment, .. })) => self
                                .openings
                                .opening(commitment)?
                                .filter(|(o, _)| o.kind == SealedKind::Directory)
                                .map(|(o, _)| o.content),
                            _ => None,
                        },
                        None => None,
                    };
                    let below = format!("{}/", full.as_str());
                    match (before, old) {
                        (Some(Node::Dir(vd)), Some(old)) => {
                            let inner = self.committed.diff(old, sub)?;
                            self.level(&below, Some(old), Some(vd), &inner, out)?;
                        }
                        (before, _) => {
                            self.clear(&full, before, out)?;
                            let inner = self.committed.diff(tree::empty(), sub)?;
                            self.level(&below, None, None, &inner, out)?;
                        }
                    }
                }
                Some(Opened::File(file)) => {
                    if let Some(Node::Dir(_)) = before {
                        self.clear(&full, before, out)?;
                    }
                    out.insert(full, Some(file));
                }
                None => self.clear(&full, before, out)?,
            }
        }
        Ok(())
    }

    /// Deletes what the view held at `path`: the file, or every file below the directory.
    fn clear(
        &self,
        path: &RepoPath,
        before: Option<Node>,
        out: &mut BTreeMap<RepoPath, Option<FileEntry>>,
    ) -> Result<()> {
        match before {
            None => {}
            Some(Node::File(_)) => {
                out.insert(path.clone(), None);
            }
            Some(Node::Dir(dir)) => {
                let prefix = format!("{}/", path.as_str());
                for file in self.view.entries(dir)?.into_keys() {
                    out.insert(join(&prefix, &file), None);
                }
            }
        }
        Ok(())
    }
}

/// Objects first reachable from `root` that `known` does not already list: chunks, and
/// the plain files' blobs when `files` is set. Hidden entries are not followed; each
/// has its own audience. A known chunk's closure is known, so work is O(new objects).
pub(crate) fn reachable<S: Store>(
    trees: &Trees<S>,
    root: ObjectId,
    files: bool,
    known: &mut impl FnMut(ObjectId) -> Result<bool>,
) -> Result<Vec<ObjectId>> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if known(id)? {
            continue;
        }
        out.push(id);
        let chunk = trees.load(id)?;
        stack.extend(chunk.children());
        for (_, node) in chunk.entries() {
            match node {
                Node::Dir(dir) => stack.push(dir),
                Node::File(file) if files => {
                    if let Some(blob) = file.content()
                        && !known(blob)?
                    {
                        out.push(blob);
                    }
                }
                Node::File(_) => {}
            }
        }
    }
    Ok(out)
}
