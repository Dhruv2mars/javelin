//! Attachment to a hosted repository: the node and repository this local store follows,
//! and the head it last reported. `.javelin/attach.json` is written last, after the head's
//! tree, blobs, World and rolling base are local, so its presence means the store holds
//! that head. The network half (fetching from a node) lives in `javelin-cli`, which can
//! depend on the protocol crate. See `docs/online-design.md` section 4.1 (U4).

use std::{fs, io::Write, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    AdvanceOutcome, Error, ObjectId, OperationId, Repository, Result,
    tree::{self, Node},
    workspace::BaseReport,
};

const ATTACHMENT: &str = "attach.json";

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

/// The operation that records an attach to the chain `head` belongs to.
fn attach_operation(head: &RemoteHead) -> Result<OperationId> {
    OperationId::parse(&format!("attach-{}", head.id))
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

    /// Refuses an attach this store cannot take, whatever its tree. Only World 0 takes an
    /// attach, and so does an interrupted attach of this same head, which left World 1 under
    /// `attach-<chain ID>` with the head's tree. Other history is never merged: its World
    /// numbers are not the remote's, even when the trees match.
    pub fn check_attach(&self, head: &RemoteHead) -> Result<()> {
        let local = self.head()?;
        let resumed =
            local.tree == head.tree && local.operation.as_ref() == Some(&attach_operation(head)?);
        if local.number.get() != 0 && !resumed {
            return Err(Error::InvalidInput(
                "this repository already has World history; attach needs an empty \
                 repository at World 0"
                    .into(),
            ));
        }
        Ok(())
    }

    /// Makes the local head hold `head.tree` and materializes the rolling base at it.
    ///
    /// A store at World 0 gets one World advance to the remote tree, under the operation
    /// `attach-<chain ID>`, so a retry after a crash finds the tree already present and
    /// does nothing. Refused unless [`Repository::check_attach`] allows it.
    pub fn attach_head(&mut self, head: &RemoteHead) -> Result<BaseReport> {
        self.check_attach(head)?;
        let local = self.head()?;
        if local.number.get() == 0 && local.tree != head.tree {
            let operation = attach_operation(head)?;
            match self.advance_to_tree(&operation, local.number, head.tree)? {
                AdvanceOutcome::Accepted { .. } => {}
                AdvanceOutcome::Stale { .. } => {
                    return Err(Error::InvalidInput(
                        "the local World moved during attach; retry".into(),
                    ));
                }
            }
        }
        self.warm_base()
    }

    fn attachment_path(&self) -> Result<PathBuf> {
        self.conn
            .path()
            .and_then(|db| std::path::Path::new(db).parent())
            .map(|dir| dir.join(ATTACHMENT))
            .ok_or_else(|| Error::CorruptRepository("repository database has no path".into()))
    }
}
