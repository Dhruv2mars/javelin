//! Members and invites against a real state directory and the in-process self-hosted node:
//! every change reaches the node's stored permissions at a new epoch, the node's gate decides
//! with them at once, and only a Maintain holder or the admin changes them.

use std::path::Path;

use javelin_collaboration::{RepositoryId, Role};
use javelin_control::{
    ControlPlane, CreateRequest, LocalNode, MemberChange, NodeTarget, Poll, Publish,
};
use javelin_identity::{Need, Scope};

const T0: u64 = 1_800_000_000;

fn open(dir: &Path) -> (ControlPlane, String) {
    let plane = ControlPlane::open(dir).expect("open");
    let admin = plane.bootstrap().expect("bootstrap");
    (plane, admin)
}

/// The admin's sign-in, and `acme/app` created with it (private, admin as Maintain).
fn repository(plane: &ControlPlane, admin: &str) -> (String, String) {
    let start = plane.device_start(T0).unwrap();
    plane.device_approve(admin, &start.user_code, T0).unwrap();
    let Poll::Approved(owner) = plane.device_poll(&start.device_code, T0).unwrap() else {
        panic!("approved");
    };
    let created = plane
        .create_repository(
            &owner.access_token,
            &CreateRequest {
                owner: "acme".into(),
                name: "app".into(),
                public: false,
                publish: Publish::Requests,
                operation: "create-app".into(),
            },
            T0,
        )
        .unwrap();
    (owner.access_token, created.repository)
}

/// What the node's gate decides for `bearer`'s token for `repository`, asking for `scope`.
fn node_allows(
    dir: &Path,
    plane: &ControlPlane,
    repository: &str,
    signin: &str,
    scope: Scope,
) -> Result<Role, String> {
    let token = plane
        .repository_token(signin, repository, T0)
        .map_err(|error| error.code().to_string())?;
    let gate = LocalNode::new(dir.join("nodes"))
        .gate(
            &RepositoryId::parse(repository).unwrap(),
            plane.node_keys().unwrap(),
        )
        .unwrap();
    gate.check(Some(&token.access_token), T0, Need { scope, layer: None })
        .map(|grant| grant.role)
        .map_err(|error| error.code)
}

#[test]
fn an_invited_account_works_at_once_and_loses_access_when_removed() {
    let dir = tempfile::tempdir().unwrap();
    let (plane, admin) = open(dir.path());
    let (owner, repo) = repository(&plane, &admin);

    let invite = plane
        .invite(&owner, "acme/app", "cora", Role::Review, T0)
        .unwrap();
    assert!(invite.code.starts_with("jav_invite_"));
    assert_eq!(invite.repository, repo);
    let listed = plane.members(&owner, &repo, T0).unwrap();
    assert_eq!(listed.invites.len(), 1);
    assert_eq!(listed.members.len(), 1, "an invite is not a membership");

    let redeemed = plane.redeem_invite(&invite.code, T0 + 10).unwrap();
    assert_eq!(redeemed.session.principal, "cora");
    assert_eq!(redeemed.role, Role::Review);
    assert_eq!(redeemed.node_error, None);
    let cora = redeemed.session.access_token;
    assert_eq!(
        plane
            .redeem_invite(&invite.code, T0 + 11)
            .unwrap_err()
            .code(),
        "not_found",
        "a code works once"
    );

    // The node holds the new epoch, and cora approves but cannot Publish.
    let stored = javelin_node::auth::read_permissions(&dir.path().join("nodes").join(&repo))
        .unwrap()
        .unwrap();
    assert_eq!(stored.epoch, 2);
    assert_eq!(
        node_allows(dir.path(), &plane, &repo, &cora, Scope::ReviewApprove),
        Ok(Role::Review)
    );
    assert_eq!(
        node_allows(dir.path(), &plane, &repo, &cora, Scope::Publish),
        Err("forbidden".into())
    );

    // Promoted, cora can Publish; a member who is not Maintain cannot change members.
    assert_eq!(
        plane
            .change_member(&cora, &repo, "cora", MemberChange::Role(Role::Maintain), T0)
            .unwrap_err()
            .code(),
        "forbidden"
    );
    let promoted = plane
        .change_member(
            &owner,
            &repo,
            "cora",
            MemberChange::Role(Role::Maintain),
            T0,
        )
        .unwrap();
    assert!(promoted.changed);
    assert_eq!(promoted.epoch, 3);
    assert_eq!(
        node_allows(dir.path(), &plane, &repo, &cora, Scope::Publish),
        Ok(Role::Maintain)
    );
    let again = plane
        .change_member(
            &owner,
            &repo,
            "cora",
            MemberChange::Role(Role::Maintain),
            T0,
        )
        .unwrap();
    assert!(!again.changed, "a repeat changes nothing");
    assert_eq!(again.epoch, 3);

    // A token issued before the removal stops working at the node with the next request.
    let before = plane.repository_token(&cora, &repo, T0).unwrap();
    plane
        .change_member(&owner, &repo, "cora", MemberChange::Remove, T0)
        .unwrap();
    let gate = LocalNode::new(dir.path().join("nodes"))
        .gate(
            &RepositoryId::parse(&repo).unwrap(),
            plane.node_keys().unwrap(),
        )
        .unwrap();
    let refused = gate
        .check(
            Some(&before.access_token),
            T0,
            Need {
                scope: Scope::Read,
                layer: None,
            },
        )
        .unwrap_err();
    assert_eq!(refused.code, "forbidden");
    assert_eq!(
        plane.repository_token(&cora, &repo, T0).unwrap_err().code(),
        "forbidden",
        "no new token for a removed member"
    );

    // Back again by account name: cora's account still exists.
    let added = plane
        .change_member(&owner, &repo, "cora", MemberChange::Add(Role::Read), T0)
        .unwrap();
    assert_eq!(added.epoch, 5);
    assert_eq!(
        node_allows(dir.path(), &plane, &repo, &cora, Scope::Read),
        Ok(Role::Read)
    );
}

#[test]
fn members_name_existing_accounts_and_invites_name_new_ones() {
    let dir = tempfile::tempdir().unwrap();
    let (plane, admin) = open(dir.path());
    let (owner, repo) = repository(&plane, &admin);

    let unknown = plane
        .change_member(&owner, &repo, "dan", MemberChange::Add(Role::Read), T0)
        .unwrap_err();
    assert_eq!(unknown.code(), "not_found");
    assert!(unknown.to_string().contains("member invite"), "{unknown}");
    let existing = plane
        .invite(&owner, &repo, "admin", Role::Read, T0)
        .unwrap_err();
    assert_eq!(existing.code(), "conflict");

    // Two invites for one name: the first redeemed makes the account, the second is refused
    // instead of signing anyone in as it.
    let first = plane.invite(&owner, &repo, "dan", Role::Read, T0).unwrap();
    let replaced = plane
        .invite(&owner, &repo, "dan", Role::Review, T0)
        .unwrap();
    assert_eq!(plane.members(&owner, &repo, T0).unwrap().invites.len(), 1);
    assert_eq!(
        plane.redeem_invite(&first.code, T0).unwrap_err().code(),
        "not_found",
        "a new invite replaces the old one"
    );
    let late = plane
        .redeem_invite(&replaced.code, T0 + 7 * 24 * 3600)
        .unwrap_err();
    assert_eq!(late.code(), "expired");
}

#[test]
fn a_repository_keeps_a_maintainer_and_settings_push_a_new_epoch() {
    let dir = tempfile::tempdir().unwrap();
    let (plane, admin) = open(dir.path());
    let (owner, repo) = repository(&plane, &admin);

    for change in [MemberChange::Remove, MemberChange::Role(Role::Review)] {
        let refused = plane
            .change_member(&owner, &repo, "admin", change, T0)
            .unwrap_err();
        assert_eq!(refused.code(), "invalid_input", "{refused}");
    }
    let settings = plane
        .set_required_approvals(&owner, "acme/app", 1, T0)
        .unwrap();
    assert_eq!((settings.required_approvals, settings.epoch), (1, 2));
    let stored = javelin_node::auth::read_permissions(&dir.path().join("nodes").join(&repo))
        .unwrap()
        .unwrap();
    assert_eq!((stored.required_approvals, stored.epoch), (1, 2));
    assert_eq!(
        plane.members("not-a-token", &repo, T0).unwrap_err().code(),
        "unauthorized"
    );
}

#[test]
fn a_signed_in_device_approves_another_device_for_the_same_account() {
    let dir = tempfile::tempdir().unwrap();
    let (plane, admin) = open(dir.path());
    let (owner, repo) = repository(&plane, &admin);
    let invite = plane.invite(&owner, &repo, "cora", Role::Read, T0).unwrap();
    let laptop = plane.redeem_invite(&invite.code, T0).unwrap().session;

    let start = plane.device_start(T0).unwrap();
    // A repository token reaches nodes, so it never approves a device.
    let repo_token = plane
        .repository_token(&laptop.access_token, &repo, T0)
        .unwrap();
    assert_eq!(
        plane
            .device_approve_as(&repo_token.access_token, &start.user_code, T0)
            .unwrap_err()
            .code(),
        "forbidden"
    );
    plane
        .device_approve_as(&laptop.access_token, &start.user_code, T0)
        .unwrap();
    let Poll::Approved(desktop) = plane.device_poll(&start.device_code, T0).unwrap() else {
        panic!("approved");
    };
    assert_eq!(desktop.principal, "cora");
    assert_ne!(desktop.device, laptop.device);
}

#[test]
fn removing_a_name_cancels_its_pending_invite() {
    let dir = tempfile::tempdir().unwrap();
    let (plane, admin) = open(dir.path());
    let (owner, repo) = repository(&plane, &admin);
    let invite = plane
        .invite(&owner, &repo, "cora", Role::Maintain, T0)
        .unwrap();
    let removed = plane
        .change_member(&owner, &repo, "cora", MemberChange::Remove, T0)
        .unwrap();
    assert!(removed.changed && removed.invites.is_empty(), "{removed:?}");
    assert_eq!(
        plane.redeem_invite(&invite.code, T0).unwrap_err().code(),
        "not_found"
    );
}

/// An invite to a repository whose node this control plane cannot push to (a Cloudflare node
/// needs its admin token) still makes the account and the membership; a maintainer's control
/// plane, which reaches the node, pushes it with the next member command.
#[test]
fn an_invite_redeemed_away_from_the_node_is_pushed_by_the_next_member_command() {
    let dir = tempfile::tempdir().unwrap();
    let url = "https://node.example.workers.dev";
    let at_node = ControlPlane::with_node(
        dir.path(),
        Box::new(LocalNode::new(dir.path().join("nodes"))),
        NodeTarget::cloudflare(url),
    )
    .unwrap();
    let admin = at_node.bootstrap().unwrap();
    let (owner, repo) = repository(&at_node, &admin);
    let invite = at_node
        .invite(&owner, &repo, "cora", Role::Review, T0)
        .unwrap();

    let elsewhere = ControlPlane::open(dir.path()).unwrap();
    let redeemed = elsewhere.redeem_invite(&invite.code, T0).unwrap();
    assert_eq!(redeemed.session.principal, "cora");
    let error = redeemed.node_error.expect("the node was not reached");
    assert!(error.contains("member add"), "{error}");
    let node_dir = dir.path().join("nodes").join(&repo);
    let stored = javelin_node::auth::read_permissions(&node_dir)
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.epoch, 1,
        "nothing was pushed to a node it cannot reach"
    );

    let pushed = at_node
        .change_member(&owner, &repo, "cora", MemberChange::Add(Role::Review), T0)
        .unwrap();
    assert!(!pushed.changed);
    let stored = javelin_node::auth::read_permissions(&node_dir)
        .unwrap()
        .unwrap();
    assert_eq!(stored.epoch, 2);
}
