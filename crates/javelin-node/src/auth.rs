//! The node's authorization gate. It checks a bearer token against the repository's
//! applied permissions and revocation set, and reports each refusal as a protocol error:
//! `unauthorized` for who the caller is, `forbidden` for what it may do, and
//! `stale_permissions` for an out-of-date push. The wire does not carry tokens yet, so
//! the caller passes the token in.

use javelin_collaboration::{AppliedPermissions, RepositoryId};
use javelin_identity::{Access, Grant, KeySet, Need, Refusal, Revocation};
use javelin_protocol::ProtocolError;

pub struct Gate {
    access: Access,
}

impl Gate {
    pub fn new(repository: RepositoryId, keys: KeySet) -> Self {
        Self {
            access: Access::new(repository, keys),
        }
    }

    /// The permissions currently applied, if any.
    pub fn permissions(&self) -> Option<&AppliedPermissions> {
        self.access.permissions()
    }

    /// The trusted keys, for rotation.
    pub fn keys_mut(&mut self) -> &mut KeySet {
        self.access.keys_mut()
    }

    /// Applies a control-plane permissions push. The caller must authenticate the push.
    pub fn apply(&mut self, permissions: AppliedPermissions) -> Result<(), ProtocolError> {
        self.access.apply(permissions).map_err(refused)
    }

    /// Applies a control-plane revocation. The caller must authenticate the push.
    pub fn revoke(&mut self, target: Revocation) {
        self.access.revoke(target);
    }

    /// Decides one request at Unix time `now`. `token` is `None` for an anonymous caller.
    pub fn check(
        &self,
        token: Option<&str>,
        now: u64,
        need: Need<'_>,
    ) -> Result<Grant, ProtocolError> {
        self.access.authorize(token, now, need).map_err(refused)
    }
}

fn refused(refusal: Refusal) -> ProtocolError {
    ProtocolError::new(refusal.code(), refusal.detail())
}
