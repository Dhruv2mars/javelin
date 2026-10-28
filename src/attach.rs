//! Attachment to a hosted repository: the node and repository this local store follows,
//! and the head it last reported. `.javelin/attach.json` is written last, after the head's
//! tree, blobs, World and rolling base are local, so its presence means the store holds
//! that head. The network half (fetching from a node) lives in `javelin-cli`, which can
//! depend on the protocol crate. See `docs/online-design.md` section 4.1 (U4).
//!
//! Local World numbers are not the remote's. Each World that attach or `javelin sync`
//! advances to is recorded under `attach-<number>-<id>` or `sync-<number>-<id>`, naming the
//! remote version it holds, so a Layer's base maps back to the remote World it was made over.

use std::{fs, io::Write, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    AdvanceOutcome, Error, ObjectId, OperationId, Repository, Result, VersionNumber, WorldVersion,
    genesis_version_id,
    tree::{self, Node},
};

const ATTACHMENT: &str = "attach.json";
const ATTACH_PREFIX: &str = "attach-";
/// The operation prefix of a World advance that follows the node to a newer head.
pub const SYNC_PREFIX: &str = "sync-";

/// A head a node reported: its World number, chain ID and root tree. The number is the
/// remote World's, which the local store does not share after attach (see `attach_head`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoteHead {
    pub number: u64,
    pub id: ObjectId,
    pub tree: ObjectId,
}

/// `.javelin/attach.json`: where the store came from and the last head it reported.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    /// Node base URL, such as `http://127.0.0.1:7878`.
    pub node: String,
    /// Repository name on that node.
    pub repository: String,
    pub provider: String,
    pub head: RemoteHead,
}

/// The operation that records an advance to remote `head`: `<prefix><number>-<id>`.
fn remote_operation(prefix: &str, head: &RemoteHead) -> Result<OperationId> {
    OperationId::parse(&format!("{prefix}{}-{}", head.number, head.id))
}

/// The remote head an attach or sync operation names, holding `tree`.
fn remote_of(operation: &OperationId, tree: ObjectId) -> Option<RemoteHead> {
    let rest = [ATTACH_PREFIX, SYNC_PREFIX]
        .iter()
        .find_map(|prefix| operation.as_str().strip_prefix(prefix))?;
    let (number, id) = rest.split_once('-')?;
    Some(RemoteHead {
        number: number.parse().ok()?,
        id: ObjectId::parse(id).ok()?,
        tree,
    })
}

impl Repository {
    /// The attachment recorded in `.javelin/attach.json`, if this store has one.
    pub fn attachment(&self) -> Result<Option<Attachment>> {
        match fs::read(self.attachment_path()?) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| Error::CorruptRepository(format!("attach.json: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Writes `.javelin/attach.json` atomically: a temporary file, synced, then renamed.
    pub fn write_attachment(&self, attachment: &Attachment) -> Result<()> {
        let path = self.attachment_path()?;
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(attachment).map_err(std::io::Error::other)?;
        let mut file = fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &path)?;
        crate::repository::sync_directory(path.parent().unwrap_or(&path))
    }

    /// Stores the tree rooted at `root` from `store`, children before parents, as the
    /// native store requires. Blobs must already be stored. Chunks already here are skipped.
    pub fn import_tree(&mut self, store: &impl tree::Store, root: ObjectId) -> Result<()> {
        if self.has_tree(root)? {
            return Ok(());
        }
        let bytes = store.load(root)?;
        let chunk = tree::Chunk::decode(&bytes)?;
        let below: Vec<ObjectId> = chunk
            .children()
            .chain(chunk.entries().filter_map(|(_, node)| match node {
                Node::Dir(id) => Some(id),
                Node::File(_) => None,
            }))
            .collect();
        for id in below {
            self.import_tree(store, id)?;
        }
        self.put_tree(&bytes)?;
        Ok(())
    }

    /// The operation that advances this store to `head`, a newer head of its attachment:
    /// `sync-<remote number>-<chain ID>`, so an advance whose `attach.json` was never
    /// written can be checked against the node ([`Repository::interrupted_sync`]).
    pub fn sync_operation(head: &RemoteHead) -> Result<OperationId> {
        remote_operation(SYNC_PREFIX, head)
    }

    /// The remote head an attach or sync recorded for local World `version`: its operation
    /// names one and it has no publication receipt, since Publish always records one.
    fn recorded_remote(&self, version: &WorldVersion) -> Result<Option<RemoteHead>> {
        let Some(operation) = &version.operation else {
            return Ok(None);
        };
        let Some(remote) = remote_of(operation, version.tree) else {
            return Ok(None);
        };
        Ok(self.publication(operation)?.is_none().then_some(remote))
    }

    /// The remote head a sync advanced the local head to, when that is what made the local
    /// head: a `sync-` World with no publication receipt. The tree is the local head's.
    /// `None` for any other head. The caller checks the head against the node before
    /// recording it in `attach.json`.
    pub fn interrupted_sync(&self) -> Result<Option<RemoteHead>> {
        let local = self.head()?;
        let synced = local
            .operation
            .as_ref()
            .is_some_and(|op| op.as_str().starts_with(SYNC_PREFIX));
        if !synced {
            return Ok(None);
        }
        self.recorded_remote(&local)
    }

    /// Whether the local head is still a World this store took from the node for
    /// `attachment`'s head: the same tree, made by an attach or sync (World 0 when the
    /// head's tree is empty). Anything else, such as a local Publish, is local divergence
    /// that following the node would overwrite.
    pub fn follows_attachment(&self, attachment: &Attachment) -> Result<bool> {
        let local = self.head()?;
        if local.tree != attachment.head.tree {
            return Ok(false);
        }
        if local.number.get() == 0 && local.operation.is_none() {
            return Ok(true);
        }
        Ok(self.recorded_remote(&local)?.is_some())
    }

    /// Whether an attach was interrupted before `attach.json` was written. Attach advances
    /// without a publication receipt, while adopt and publish always record one. So an
    /// `attach-` head with no receipt and no attachment is an attach's own World, whatever
    /// operation name a user chose.
    fn interrupted_attach(&self) -> Result<bool> {
        let local = self.head()?;
        let Some(operation) = local
            .operation
            .as_ref()
            .filter(|op| op.as_str().starts_with(ATTACH_PREFIX))
        else {
            return Ok(false);
        };
        Ok(self.attachment()?.is_none() && self.publication(operation)?.is_none())
    }

    /// Refuses an attach this store cannot take, whatever its tree. Only World 0 takes an
    /// attach, and so does a store whose attach was interrupted before `attach.json`: the
    /// remote may have moved on since, and the attach resumes from whatever head it reports
    /// now. Other history is never merged: its World numbers are not the remote's, even
    /// when the trees match.
    pub fn check_attach(&self) -> Result<()> {
        let local = self.head()?;
        if local.number.get() != 0 && !self.interrupted_attach()? {
            return Err(Error::InvalidInput(
                "this repository already has World history; attach needs an empty \
                 repository at World 0"
                    .into(),
            ));
        }
        Ok(())
    }

    /// Makes the local head hold `head.tree`, without materializing the rolling base.
    ///
    /// The store gets one World advance to the remote tree, under the operation
    /// `attach-<chain ID>`, so a retry after a crash finds the tree already present and
    /// does nothing. An interrupted attach advances from its own World the same way. Refused
    /// unless [`Repository::check_attach`] allows it. The caller warms the rolling base with
    /// [`Repository::warm_base`].
    pub fn attach_head(&mut self, head: &RemoteHead) -> Result<()> {
        self.check_attach()?;
        let local = self.head()?;
        if local.tree != head.tree {
            let operation = remote_operation(ATTACH_PREFIX, head)?;
            match self.advance_to_tree(&operation, local.number, head.tree)? {
                AdvanceOutcome::Accepted { .. } => {}
                AdvanceOutcome::Stale { .. } => {
                    return Err(Error::InvalidInput(
                        "the local World moved during attach; retry".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// The remote World a local World holds, as attach and sync recorded it: `None` for a
    /// World made here, or before the attachment. World 0 is the remote's World 0, since both
    /// are the fixed empty genesis.
    pub fn remote_world(&self, local: VersionNumber) -> Result<Option<RemoteHead>> {
        let Some(attachment) = self.attachment()? else {
            return Ok(None);
        };
        let version = self.version(local)?;
        if let Some(remote) = self.recorded_remote(&version)? {
            return Ok(Some(remote));
        }
        if local.get() == 0 {
            return Ok(Some(RemoteHead {
                number: 0,
                id: genesis_version_id(),
                tree: version.tree,
            }));
        }
        if self.head()?.number == local && version.tree == attachment.head.tree {
            return Ok(Some(attachment.head));
        }
        Ok(None)
    }

    /// The local World that holds remote World `remote`, as attach and sync recorded it, or
    /// `None` when this store does not hold it. Attached stores keep one local World per
    /// attach or sync, so this reads that short history.
    pub fn local_world(&self, remote: u64) -> Result<Option<VersionNumber>> {
        for version in self.history()?.into_iter().rev() {
            if self
                .remote_world(version.number)?
                .is_some_and(|head| head.number == remote)
            {
                return Ok(Some(version.number));
            }
        }
        Ok(None)
    }

    /// Moves the local World from `local`, a head [`Repository::follows_attachment`] accepted,
    /// to `head`, a verified remote head newer than the attached one whose tree is stored,
    /// then records it in `attach.json`. The advance is a compare-and-swap on `local`, so a
    /// Publish here since the check is never replaced: the sync is refused instead. It is
    /// recorded under `sync-<number>-<id>`, so a retry after a crash finds it done. The
    /// caller warms the rolling base after.
    pub fn sync_head(
        &mut self,
        attachment: &Attachment,
        local: &WorldVersion,
        head: &RemoteHead,
    ) -> Result<()> {
        if local.tree == head.tree {
            if self.head()?.number != local.number {
                return Err(Error::InvalidInput(
                    "the local World moved during sync; nothing was changed".into(),
                ));
            }
        } else {
            let operation = remote_operation(SYNC_PREFIX, head)?;
            match self.advance_to_tree(&operation, local.number, head.tree)? {
                AdvanceOutcome::Accepted { .. } => {}
                AdvanceOutcome::Stale { .. } => {
                    return Err(Error::InvalidInput(
                        "the local World moved during sync; nothing was changed".into(),
                    ));
                }
            }
            // A stop here leaves a sync World that `attach.json` does not name yet;
            // [`Repository::interrupted_sync`] finds it.
            crate::repository::failpoint("sync_after_advance");
        }
        self.write_attachment(&Attachment {
            head: head.clone(),
            ..attachment.clone()
        })
    }

    fn attachment_path(&self) -> Result<PathBuf> {
        self.conn
            .path()
            .and_then(|db| std::path::Path::new(db).parent())
            .map(|dir| dir.join(ATTACHMENT))
            .ok_or_else(|| Error::CorruptRepository("repository database has no path".into()))
    }
}
