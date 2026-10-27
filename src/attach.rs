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
const SYNC_PREFIX: &str = "sync-";

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

/// Where the local World stands against its attachment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttachedWorld {
    /// The local head holds the attached head. An interrupted sync is finished first.
    Current(Attachment),
    /// The local World moved away from the attachment, by a Publish here. Sync refuses it.
    Diverged {
        attachment: Attachment,
        local: WorldVersion,
    },
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
        if let Some(operation) = &version.operation
            && let Some(remote) = remote_of(operation, version.tree)
            && self.publication(operation)?.is_none()
        {
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

    /// Checks the local head against the attachment. A sync that advanced the World and was
    /// interrupted before `attach.json` names a newer remote head than the attachment; that
    /// head was verified before the advance, so the attachment is completed to it here.
    /// Any other local head that does not hold the attached tree was published here.
    pub fn attached_world(&self) -> Result<Option<AttachedWorld>> {
        let Some(attachment) = self.attachment()? else {
            return Ok(None);
        };
        let local = self.head()?;
        if local.tree == attachment.head.tree {
            return Ok(Some(AttachedWorld::Current(attachment)));
        }
        if let Some(remote) = self
            .remote_world(local.number)?
            .filter(|remote| remote.number > attachment.head.number)
        {
            let attachment = Attachment {
                head: remote,
                ..attachment
            };
            self.write_attachment(&attachment)?;
            return Ok(Some(AttachedWorld::Current(attachment)));
        }
        Ok(Some(AttachedWorld::Diverged { attachment, local }))
    }

    /// Moves the local World to `head`, a verified remote head newer than the attached one
    /// whose tree is stored, then records it in `attach.json`. The advance is recorded under
    /// `sync-<number>-<id>`, so a retry after a crash finds it done. The caller checks the
    /// local World first with [`Repository::attached_world`] and warms the rolling base after.
    pub fn sync_head(&mut self, attachment: &Attachment, head: &RemoteHead) -> Result<()> {
        let local = self.head()?;
        if local.tree != head.tree {
            let operation = remote_operation(SYNC_PREFIX, head)?;
            match self.advance_to_tree(&operation, local.number, head.tree)? {
                AdvanceOutcome::Accepted { .. } => {}
                AdvanceOutcome::Stale { .. } => {
                    return Err(Error::InvalidInput(
                        "the local World moved during sync; retry".into(),
                    ));
                }
            }
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
