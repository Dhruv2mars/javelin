//! The self-hosted node the control plane provisions in-process. Each repository gets a
//! directory holding a native repository with World 0 and its applied permissions. The
//! permissions are checked by the node's own gate, so the epoch rules are the node's.

use std::{
    collections::BTreeMap,
    fs::{self},
    io::ErrorKind,
    path::PathBuf,
};

use javelin::Repository;
use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_identity::KeySet;
use javelin_node::auth::Gate;
use serde::{Deserialize, Serialize};

use crate::{ControlError, control::NodeLink, secret};

const PERMISSIONS_FILE: &str = "permissions.json";
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
        let path = self.repository_dir(repository).join(PERMISSIONS_FILE);
        match fs::read(&path) {
            Ok(bytes) => {
                let stored: Stored = serde_json::from_slice(&bytes).map_err(|error| {
                    ControlError::Storage(format!("permissions are corrupt: {error}"))
                })?;
                stored.into_permissions().map(Some)
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(node_error(error)),
        }
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
        let dir = self.repository_dir(&permissions.repository);
        let bytes = serde_json::to_vec_pretty(&Stored::from(permissions))
            .map_err(|error| ControlError::Storage(error.to_string()))?;
        secret::write_private(&dir.join(PERMISSIONS_FILE), &bytes).map_err(node_error)
    }
}

fn node_error(error: std::io::Error) -> ControlError {
    ControlError::Node(error.to_string())
}

#[derive(Serialize, Deserialize)]
struct Stored {
    repository: String,
    epoch: u64,
    public: bool,
    roles: BTreeMap<String, String>,
    required_approvals: u32,
}

impl From<&AppliedPermissions> for Stored {
    fn from(permissions: &AppliedPermissions) -> Self {
        Self {
            repository: permissions.repository.as_str().into(),
            epoch: permissions.epoch,
            public: permissions.visibility == Visibility::Public,
            roles: permissions
                .roles
                .iter()
                .map(|(principal, role)| (principal.as_str().into(), role_name(*role).into()))
                .collect(),
            required_approvals: permissions.required_approvals,
        }
    }
}

impl Stored {
    fn into_permissions(self) -> Result<AppliedPermissions, ControlError> {
        let corrupt =
            |what: &str| ControlError::Storage(format!("permissions are corrupt: {what}"));
        let repository =
            RepositoryId::parse(&self.repository).map_err(|_| corrupt("repository"))?;
        let roles = self
            .roles
            .iter()
            .map(|(principal, role)| {
                let principal = PrincipalId::parse(principal).map_err(|_| corrupt("principal"))?;
                let role = parse_role(role).ok_or_else(|| corrupt("role"))?;
                Ok((principal, role))
            })
            .collect::<Result<_, ControlError>>()?;
        Ok(AppliedPermissions {
            repository,
            epoch: self.epoch,
            visibility: if self.public {
                Visibility::Public
            } else {
                Visibility::Private
            },
            roles,
            required_approvals: self.required_approvals,
        })
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Read => "read",
        Role::Review => "review",
        Role::Maintain => "maintain",
    }
}

fn parse_role(value: &str) -> Option<Role> {
    match value {
        "read" => Some(Role::Read),
        "review" => Some(Role::Review),
        "maintain" => Some(Role::Maintain),
        _ => None,
    }
}
