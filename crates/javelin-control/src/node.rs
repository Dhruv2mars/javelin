//! The self-hosted node the control plane provisions in-process. Each repository gets a
//! directory holding a native repository with World 0, its applied permissions and the
//! keys it trusts, in the files `javelin_node::auth` reads when the node serves. The
//! permissions are checked by the node's own gate, so the epoch rules are the node's.

use std::{
    fs::{self},
    io::ErrorKind,
    path::PathBuf,
};

use javelin::{Repository, VersionNumber};
use javelin_collaboration::{AppliedPermissions, RepositoryId};
use javelin_identity::KeySet;
use javelin_node::{
    FileWorldLog,
    auth::{self, Gate},
    fork::Forked,
};

use crate::{
    ControlError,
    control::{ForkPlan, NodeLink},
};

const LOCK_FILE: &str = "node.lock";

/// Self-hosted nodes under one directory: `root/<repository>/`.
pub struct LocalNode {
    root: PathBuf,
}

impl LocalNode {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The node directory of `repository`.
    pub fn repository_dir(&self, repository: &RepositoryId) -> PathBuf {
        self.root.join(repository.as_str())
    }

    /// A gate for `repository` with the persisted permissions applied, trusting `keys`.
    pub fn gate(&self, repository: &RepositoryId, keys: KeySet) -> Result<Gate, ControlError> {
        let mut gate = Gate::new(repository.clone(), keys);
        if let Some(permissions) = self.stored(repository)? {
            gate.apply(permissions).map_err(ControlError::Refused)?;
        }
        Ok(gate)
    }

    /// Creates the repository's directory and takes its lock. Holding the lock is what keeps two
    /// retries of one create from racing on the node's files; the lock lasts until the guard drops.
    fn locked(&self, repository: &RepositoryId) -> Result<fs::File, ControlError> {
        let dir = self.repository_dir(repository);
        fs::create_dir_all(&dir).map_err(node_error)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(LOCK_FILE))
            .map_err(node_error)?;
        lock.lock().map_err(node_error)?;
        Ok(lock)
    }

    fn stored(
        &self,
        repository: &RepositoryId,
    ) -> Result<Option<AppliedPermissions>, ControlError> {
        auth::read_permissions(&self.repository_dir(repository)).map_err(read_error)
    }

    /// Replaces the keys `repository`'s node trusts. A serving node reads them when it starts.
    pub fn trust_keys(&self, repository: &RepositoryId, keys: &KeySet) -> Result<(), ControlError> {
        let _lock = self.locked(repository)?;
        auth::write_keys(&self.repository_dir(repository), keys).map_err(node_error)
    }
}

impl NodeLink for LocalNode {
    fn provision(&self, repository: &RepositoryId) -> Result<(), ControlError> {
        let _lock = self.locked(repository)?;
        let dir = self.repository_dir(repository);
        if Repository::open(&dir).is_ok() {
            return Ok(());
        }
        Repository::init(&dir)
            .map(|_| ())
            .map_err(|error| ControlError::Node(error.to_string()))
    }

    fn head(&self, repository: &RepositoryId) -> Result<VersionNumber, ControlError> {
        let repo = Repository::open(self.repository_dir(repository))
            .map_err(|error| ControlError::Node(error.to_string()))?;
        let head = repo
            .head()
            .map_err(|error| ControlError::Node(error.to_string()))?;
        Ok(head.number)
    }

    /// Opens each repository with its own node lock, which `FileWorldLog` takes. No lock from
    /// `locked` is held here, because the two would conflict on the same file.
    fn fork(&self, plan: &ForkPlan) -> Result<Forked, ControlError> {
        let open = |repository: &RepositoryId| {
            FileWorldLog::open(self.repository_dir(repository))
                .map_err(|error| ControlError::Node(error.to_string()))
        };
        let source = open(&plan.source)?;
        let mut target = open(&plan.target)?;
        let at = VersionNumber::new(plan.at)
            .map_err(|error| ControlError::InvalidInput(error.to_string()))?;
        javelin_node::fork::fork(&source, plan.source.as_str(), &mut target, at, plan.reader)
            .map_err(ControlError::Refused)
    }

    fn apply(&self, permissions: &AppliedPermissions) -> Result<(), ControlError> {
        let _lock = self.locked(&permissions.repository)?;
        let stored = self.stored(&permissions.repository)?;
        let mut gate = Gate::new(permissions.repository.clone(), KeySet::new());
        if let Some(current) = &stored {
            gate.apply(current.clone()).map_err(ControlError::Refused)?;
        }
        gate.apply(permissions.clone())
            .map_err(ControlError::Refused)?;
        if stored.as_ref() == Some(permissions) {
            return Ok(());
        }
        auth::write_permissions(&self.repository_dir(&permissions.repository), permissions)
            .map_err(node_error)
    }

    fn trust(&self, repository: &RepositoryId, keys: &KeySet) -> Result<(), ControlError> {
        self.trust_keys(repository, keys)
    }
}

fn node_error(error: std::io::Error) -> ControlError {
    ControlError::Node(error.to_string())
}

/// A stored file that does not parse is the node's state, corrupt; anything else means the
/// node could not be reached.
fn read_error(error: std::io::Error) -> ControlError {
    if error.kind() == ErrorKind::InvalidData {
        ControlError::Storage(error.to_string())
    } else {
        node_error(error)
    }
}
