//! A gate for wire tests: the node trusts the public test key, and `owner` holds Maintain on a
//! public repository. Requests carry the owner's token as `Authorization: Bearer`.

#![allow(dead_code)] // each test binary uses a different subset

use std::time::{SystemTime, UNIX_EPOCH};

use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_identity::{Scope, test_keys};
use javelin_node::auth::Gate;

pub const OWNER: &str = "owner";
const ALL: [Scope; 5] = [
    Scope::Read,
    Scope::LayerWrite,
    Scope::Publish,
    Scope::ReviewWrite,
    Scope::ReviewApprove,
];

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

pub fn permissions(repository: &str, visibility: Visibility) -> AppliedPermissions {
    AppliedPermissions {
        repository: RepositoryId::parse(repository).unwrap(),
        epoch: 1,
        visibility,
        roles: [(PrincipalId::parse(OWNER).unwrap(), Role::Maintain)]
            .into_iter()
            .collect(),
        required_approvals: 0,
    }
}

/// The gate of public repository `repository`.
pub fn gate(repository: &str) -> Gate {
    let mut gate = Gate::new(RepositoryId::parse(repository).unwrap(), test_keys::keys());
    gate.apply(permissions(repository, Visibility::Public))
        .unwrap();
    gate
}

/// A ten-minute token for `sub` on `repository`, with every repository scope.
pub fn token_for(sub: &str, repository: &str) -> String {
    let now = now();
    let claims = test_keys::claims(sub, repository, &ALL, now, now + 600, &format!("{sub}-jti"));
    test_keys::issuer().issue(&claims).unwrap()
}

/// The owner's token for `repository`.
pub fn token(repository: &str) -> String {
    token_for(OWNER, repository)
}

/// `request` with the owner's `Authorization` line after its request line.
pub fn authorized(repository: &str, request: &[u8]) -> Vec<u8> {
    let Some(end) = request.windows(2).position(|w| w == b"\r\n") else {
        return request.to_vec();
    };
    let mut out = request[..end + 2].to_vec();
    out.extend_from_slice(format!("Authorization: Bearer {}\r\n", token(repository)).as_bytes());
    out.extend_from_slice(&request[end + 2..]);
    out
}
