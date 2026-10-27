//! The control plane's pushes to a node (`NodeLink` in `javelin-control`), on any storage:
//! provision World 0, apply permissions under the gate's epoch rules, and trust keys. The
//! self-hosted `LocalNode` and the Cloudflare node both call these, so a push is decided the
//! same way on every node.

use javelin::{ObjectId, Repository};
use javelin_collaboration::AppliedPermissions;
use javelin_identity::KeySet;
use javelin_protocol::ProtocolError;

use crate::{
    auth::{self, Gate},
    storage::Storage,
};

/// World 0 of every repository, as `(id, tree)`: the version an empty `Repository` starts at.
pub fn genesis() -> (ObjectId, ObjectId) {
    (javelin::genesis_version_id(), javelin::tree::empty())
}

/// Creates the repository with World 0. Returns whether it was created; a repeat changes
/// nothing.
pub fn provision(storage: &dyn Storage) -> Result<bool, ProtocolError> {
    if storage.provisioned()? {
        return Ok(false);
    }
    storage.repository()?;
    Ok(true)
}

/// World 0 of a provisioned repository, as `(id, tree)`, or `None` before provisioning.
pub fn provisioned_genesis(
    storage: &dyn Storage,
) -> Result<Option<(ObjectId, ObjectId)>, ProtocolError> {
    if !storage.provisioned()? {
        return Ok(None);
    }
    let repository: Repository = storage.repository()?;
    let zero = repository.version(javelin::VersionNumber::new(0)?)?;
    Ok(Some((zero.id, zero.tree)))
}

/// Applies a permissions push with `Gate::apply`'s epoch rules: an older epoch, or the same
/// epoch with other content, is `stale_permissions`; the same push again is accepted and
/// changes nothing. Returns whether the stored permissions changed.
pub fn apply(
    storage: &dyn Storage,
    permissions: &AppliedPermissions,
) -> Result<bool, ProtocolError> {
    let stored = auth::stored_permissions(storage)?;
    let mut gate = Gate::new(permissions.repository.clone(), KeySet::new());
    if let Some(current) = &stored {
        gate.apply(current.clone())?;
    }
    gate.apply(permissions.clone())?;
    if stored.as_ref() == Some(permissions) {
        return Ok(false);
    }
    auth::store_permissions(storage, permissions)?;
    Ok(true)
}

/// Replaces the keys the node verifies tokens with.
pub fn trust(storage: &dyn Storage, keys: &KeySet) -> Result<(), ProtocolError> {
    Ok(auth::store_keys(storage, keys)?)
}
