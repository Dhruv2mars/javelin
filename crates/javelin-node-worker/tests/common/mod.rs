//! The node conformance suite: the same assertions for every node that implements
//! `NodeLink`. `local_node.rs` runs it against the self-hosted node in CI; `workerd.rs` runs it
//! against the Cloudflare node on `wrangler dev`.

use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_control::{ControlError, NodeLink};
use javelin_node_worker::{Snapshot, genesis, permissions_json};
use std::collections::BTreeMap;

/// The genesis every node must report after provisioning.
pub fn expected_genesis() -> Option<(String, String)> {
    Some(genesis().expect("genesis"))
}

pub fn permissions(epoch: u64, visibility: Visibility, extra_reviewer: bool) -> AppliedPermissions {
    let mut roles = BTreeMap::from([(
        PrincipalId::parse("admin").expect("principal"),
        Role::Maintain,
    )]);
    if extra_reviewer {
        roles.insert(
            PrincipalId::parse("reviewer").expect("principal"),
            Role::Review,
        );
    }
    AppliedPermissions {
        repository: RepositoryId::parse("repo_conformance").expect("repository"),
        epoch,
        visibility,
        roles,
        required_approvals: 0,
    }
}

fn refused_code(error: &ControlError) -> Option<&str> {
    match error {
        ControlError::Refused(protocol) => Some(protocol.code.as_str()),
        _ => None,
    }
}

/// Runs the suite. `snapshot` reads what the node holds for the repository.
pub fn run(node: &dyn NodeLink, snapshot: &dyn Fn() -> Snapshot) {
    let repository = RepositoryId::parse("repo_conformance").expect("repository");
    assert_eq!(snapshot().genesis, None, "a fresh node holds no World 0");

    // Provision creates World 0 once; a repeat changes nothing.
    node.provision(&repository).expect("provision");
    assert_eq!(snapshot().genesis, expected_genesis());
    node.provision(&repository).expect("provision again");
    assert_eq!(snapshot().genesis, expected_genesis());
    assert_eq!(snapshot().permissions, None);

    // First permissions push; the same push again is accepted.
    let first = permissions(1, Visibility::Private, false);
    node.apply(&first).expect("apply epoch 1");
    assert_eq!(snapshot().permissions, Some(permissions_json(&first)));
    node.apply(&first).expect("apply epoch 1 again");

    // The same epoch with different content is refused, and nothing changes.
    let altered = permissions(1, Visibility::Public, false);
    let error = node.apply(&altered).expect_err("same epoch, other content");
    assert_eq!(refused_code(&error), Some("stale_permissions"), "{error}");
    assert_eq!(snapshot().permissions, Some(permissions_json(&first)));

    // An older epoch is refused.
    let older = permissions(0, Visibility::Private, false);
    let error = node.apply(&older).expect_err("older epoch");
    assert_eq!(refused_code(&error), Some("stale_permissions"), "{error}");

    // A newer epoch replaces the permissions.
    let second = permissions(2, Visibility::Public, true);
    node.apply(&second).expect("apply epoch 2");
    assert_eq!(snapshot().permissions, Some(permissions_json(&second)));

    // Provisioning again must not reset the applied permissions.
    node.provision(&repository).expect("provision after apply");
    assert_eq!(snapshot().genesis, expected_genesis());
    assert_eq!(snapshot().permissions, Some(permissions_json(&second)));
}
