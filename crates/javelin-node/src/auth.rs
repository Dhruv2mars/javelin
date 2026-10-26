//! The node's authorization gate. It checks a bearer token against the repository's
//! applied permissions and revocation set, and reports each refusal as a protocol error:
//! `unauthorized` for who the caller is, `forbidden` for what it may do, and
//! `stale_permissions` for an out-of-date push. The wire does not carry tokens yet, so
//! the caller passes the token in.

use javelin::Reader;
use javelin_collaboration::{AppliedPermissions, RepositoryId, Role};
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

/// The audiences a caller reads, from its role in the repository: no role is an outsider,
/// Read and Review are members, Maintain is a maintainer. Named groups are not applied yet.
pub fn reader_for(role: Option<Role>) -> Reader {
    match role {
        None => Reader::outsider(),
        Some(Role::Maintain) => Reader::maintainer(),
        Some(Role::Read | Role::Review) => Reader::member([]),
    }
}

/// The audiences of an authorized request. An anonymous grant is an outsider.
pub fn grant_reader(grant: &Grant) -> Reader {
    reader_for(grant.principal.as_ref().map(|_| grant.role))
}

fn refused(refusal: Refusal) -> ProtocolError {
    ProtocolError::new(refusal.code(), refusal.detail())
}
