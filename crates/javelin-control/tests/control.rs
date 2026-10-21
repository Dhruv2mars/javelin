//! Control-plane behavior against a real state directory and the in-process self-hosted node.

use std::{cell::Cell, fs, path::Path};

use javelin_collaboration::AppliedPermissions;
use javelin_collaboration::{RepositoryId, Role};
use javelin_control::{
    ControlError, ControlPlane, CreateRequest, LocalNode, NodeLink, Poll, Publish,
};
use javelin_identity::{Need, Scope, test_keys};

const T0: u64 = 1_800_000_000;

fn request(name: &str, operation: &str) -> CreateRequest {
    CreateRequest {
        owner: "acme".into(),
        name: name.into(),
        public: false,
        publish: Publish::Direct,
        operation: operation.into(),
    }
}

/// Approves a fresh device code with `admin` and returns the session it yields.
fn sign_in(plane: &ControlPlane, admin: &str, now: u64) -> javelin_control::Session {
    let start = plane.device_start(now).expect("start");
    assert_eq!(
        plane.device_poll(&start.device_code, now).expect("poll"),
        Poll::Pending
    );
    plane
        .device_approve(admin, &start.user_code, now)
        .expect("approve");
    match plane.device_poll(&start.device_code, now).expect("poll") {
        Poll::Approved(session) => session,
        Poll::Pending => panic!("approved code still pending"),
    }
}

/// A node whose first permissions push fails, as if the node dropped the request.
struct FailFirstApply {
    node: LocalNode,
    fail: Cell<bool>,
}

impl NodeLink for FailFirstApply {
    fn provision(&self, repository: &RepositoryId) -> Result<(), ControlError> {
        self.node.provision(repository)
    }

    fn apply(&self, permissions: &AppliedPermissions) -> Result<(), ControlError> {
        if self.fail.replace(false) {
            return Err(ControlError::Node("injected: reply lost".into()));
        }
        self.node.apply(permissions)
    }
}

fn open(dir: &Path) -> (ControlPlane, String) {
    let plane = ControlPlane::open(dir).expect("open");
    let admin = plane.bootstrap().expect("bootstrap");
    (plane, admin)
}

#[test]
fn bootstrap_prints_once_and_stores_only_a_hash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plane, admin) = open(dir.path());
    assert!(admin.starts_with("jav_admin_"));
    assert_eq!(plane.bootstrap().unwrap_err().code(), "conflict");
    let state = fs::read_to_string(dir.path().join("control.json")).expect("state");
    assert!(
        !state.contains(&admin),
        "the admin token must not be stored"
    );
}

#[test]
fn commands_before_bootstrap_are_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plane = ControlPlane::open(dir.path()).expect("open");
    let error = plane.device_start(T0).unwrap_err();
    assert_eq!(error.code(), "not_initialized");
}

#[test]
fn device_code_signs_in_once_and_refresh_issues_access() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plane, admin) = open(dir.path());
    let start = plane.device_start(T0).expect("start");
    assert_eq!(start.user_code.len(), 9, "XXXX-XXXX");
    assert_eq!(start.interval, 5);

    let wrong = plane
        .device_approve("jav_admin_nope", &start.user_code, T0)
        .unwrap_err();
    assert_eq!(wrong.code(), "unauthorized");
    let typo = plane.device_approve(&admin, "0000-0000", T0).unwrap_err();
    assert_eq!(typo.code(), "invalid_input");
    assert_eq!(
        plane.device_poll(&start.device_code, T0).expect("poll"),
        Poll::Pending
    );

    plane
        .device_approve(&admin, &start.user_code.to_lowercase(), T0)
        .expect("approve");
    plane
        .device_approve(&admin, &start.user_code, T0)
        .expect("approving twice is harmless");
    let Poll::Approved(session) = plane.device_poll(&start.device_code, T0).expect("poll") else {
        panic!("expected a session");
    };
    assert_eq!(session.principal, "admin");
    assert!(session.access_token.starts_with("eyJ"), "a signed JWS");
    assert_eq!(
        plane
            .device_poll(&start.device_code, T0)
            .unwrap_err()
            .code(),
        "already_redeemed"
    );

    let refreshed = plane
        .refresh(&session.refresh_token, T0 + 60)
        .expect("refresh");
    assert_eq!(refreshed.expires_in, 3600);
    assert_eq!(
        plane.refresh("jav_refresh_forged", T0).unwrap_err().code(),
        "unauthorized"
    );
    assert_eq!(
        plane
            .refresh(&session.refresh_token, T0 + 30 * 24 * 3600)
            .unwrap_err()
            .code(),
        "unauthorized",
        "refresh tokens last 30 days"
    );
}

#[test]
fn an_unapproved_device_code_expires() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plane, admin) = open(dir.path());
    let start = plane.device_start(T0).expect("start");
    let late = T0 + 900;
    assert_eq!(
        plane
            .device_poll(&start.device_code, late)
            .unwrap_err()
            .code(),
        "expired"
    );
    assert_eq!(
        plane
            .device_approve(&admin, &start.user_code, late)
            .unwrap_err()
            .code(),
        "expired"
    );
}

#[test]
fn create_twice_with_one_operation_makes_one_repository() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plane, admin) = open(dir.path());
    let first = plane
        .create_repository(&admin, &request("app", "op-1"), T0)
        .expect("create");
    assert!(first.created);
    assert_eq!(first.epoch, 1);
    let again = plane
        .create_repository(&admin, &request("app", "op-1"), T0 + 5)
        .expect("retry");
    assert!(!again.created);
    assert_eq!(again.repository, first.repository);

    let nodes: Vec<_> = fs::read_dir(dir.path().join("nodes"))
        .expect("nodes")
        .collect();
    assert_eq!(nodes.len(), 1, "no duplicate namespace");
}

#[test]
fn name_collision_and_reused_operations_are_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plane, admin) = open(dir.path());
    plane
        .create_repository(&admin, &request("app", "op-1"), T0)
        .expect("create");
    let taken = plane
        .create_repository(&admin, &request("app", "op-2"), T0)
        .unwrap_err();
    assert_eq!(taken.code(), "conflict");
    let reused = plane
        .create_repository(&admin, &request("other", "op-1"), T0)
        .unwrap_err();
    assert_eq!(reused.code(), "operation_id_reused");
}

#[test]
fn a_crash_after_provisioning_is_resumed_by_retry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let node = FailFirstApply {
        node: LocalNode::new(dir.path().join("nodes")),
        fail: Cell::new(true),
    };
    let plane = ControlPlane::with_node(dir.path(), Box::new(node)).expect("open");
    let admin = plane.bootstrap().expect("bootstrap");

    let lost = plane
        .create_repository(&admin, &request("app", "op-1"), T0)
        .unwrap_err();
    assert_eq!(lost.code(), "node_unavailable");

    let retried = plane
        .create_repository(&admin, &request("app", "op-1"), T0 + 1)
        .expect("retry");
    assert!(!retried.created);
    assert_eq!(retried.epoch, 1);
    let nodes: Vec<_> = fs::read_dir(dir.path().join("nodes"))
        .expect("nodes")
        .collect();
    assert_eq!(nodes.len(), 1, "retry must not duplicate the namespace");
}

#[test]
fn a_session_creates_a_repository_whose_creator_can_publish_on_the_node() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plane, admin) = open(dir.path());
    let session = sign_in(&plane, &admin, T0);
    let created = plane
        .create_repository(&session.access_token, &request("app", "op-1"), T0)
        .expect("create");

    let node = LocalNode::new(dir.path().join("nodes"));
    let repository = RepositoryId::parse(&created.repository).expect("id");
    let gate = node
        .gate(&repository, plane.node_keys().expect("keys"))
        .expect("gate");
    let token = plane
        .repository_token(&session.access_token, &created.repository, T0)
        .expect("token");
    let grant = gate
        .check(
            Some(&token.access_token),
            T0,
            Need {
                scope: Scope::Publish,
                layer: None,
            },
        )
        .expect("creator may publish");
    assert_eq!(grant.role, Role::Maintain);
    assert_eq!(grant.principal.as_deref(), Some("admin"));
    let other = plane.repository_token(&session.access_token, "repo_missing", T0);
    assert_eq!(other.unwrap_err().code(), "not_found");
    let anonymous = gate.check(
        None,
        T0,
        Need {
            scope: Scope::Read,
            layer: None,
        },
    );
    assert!(
        anonymous.is_err(),
        "a private repository needs a token to read"
    );
}

#[test]
fn creation_needs_a_token_from_this_control_plane() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plane, _admin) = open(dir.path());
    let forged = test_keys::issuer()
        .issue(&test_keys::claims(
            "admin",
            "*",
            &[Scope::RepoCreate],
            T0,
            T0 + 60,
            "jti_forged",
        ))
        .expect("issue");
    let refused = plane
        .create_repository(&forged, &request("app", "op-1"), T0)
        .unwrap_err();
    assert_eq!(refused.code(), "unauthorized");
    let missing = plane
        .create_repository("jav_admin_nope", &request("app", "op-1"), T0)
        .unwrap_err();
    assert_eq!(missing.code(), "unauthorized");
}

#[test]
fn a_stale_permissions_push_is_refused_by_the_node() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plane, admin) = open(dir.path());
    let created = plane
        .create_repository(&admin, &request("app", "op-1"), T0)
        .expect("create");
    let node = LocalNode::new(dir.path().join("nodes"));
    let repository = RepositoryId::parse(&created.repository).expect("id");
    let stale = AppliedPermissions {
        repository,
        epoch: 0,
        visibility: javelin_collaboration::Visibility::Private,
        roles: Default::default(),
        required_approvals: 0,
    };
    let refused = node.apply(&stale).unwrap_err();
    assert_eq!(refused.code(), "stale_permissions");
}
