//! Fork through the control plane, over the in-process self-hosted node.

use std::{cell::Cell, path::Path, rc::Rc};

use javelin::{
    FileEntry, LayerId, ObjectId, OperationId, PublishAttribution, RepoPath, SparseChanges,
    VersionNumber,
};
use javelin_collaboration::{AppliedPermissions, RepositoryId};
use javelin_control::{
    ControlError, ControlPlane, CreateRequest, Created, ForkPlan, ForkRequest, LocalNode, NodeLink,
    NodeTarget, Publish,
};
use javelin_node::{FileWorldLog, fork::Forked};
use javelin_protocol::{codec::PublishOutcome, receiver::WorldLog};

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

fn fork_request(source: &str, at: u64, name: &str, operation: &str) -> ForkRequest {
    ForkRequest {
        source: source.into(),
        at,
        target: request(name, operation),
    }
}

fn open(dir: &Path) -> (ControlPlane, String) {
    let plane = ControlPlane::open(dir).expect("open");
    let admin = plane.bootstrap().expect("bootstrap");
    (plane, admin)
}

/// Publishes one change as Layer `name` over World `base` in the node at `node_dir`.
fn publish_one(node_dir: &Path, name: &str, base: u64) {
    let mut log = FileWorldLog::open(node_dir).expect("open node");
    let layer = LayerId::parse(name).unwrap();
    let base = VersionNumber::new(base).unwrap();
    log.create_layer(&layer, base).unwrap();
    let content = log.repository().put_blob(name.as_bytes()).unwrap();
    let changes = SparseChanges::new([(
        RepoPath::parse(&format!("{name}.txt")).unwrap(),
        Some(FileEntry::Regular {
            content,
            executable: false,
        }),
    )])
    .unwrap();
    let expected = log
        .append(
            &OperationId::parse(&format!("write-{name}")).unwrap(),
            &layer,
            &changes,
        )
        .unwrap()
        .checkpoint()
        .object();
    let outcome = log
        .publish_layer(
            &OperationId::parse(&format!("publish-{name}")).unwrap(),
            &layer,
            expected,
            base,
            &PublishAttribution::Human,
        )
        .unwrap();
    assert!(
        matches!(outcome, PublishOutcome::Published { .. }),
        "{outcome:?}"
    );
}

/// The ID of `number` in the node at `node_dir`.
fn version_id(node_dir: &Path, number: u64) -> ObjectId {
    let log = FileWorldLog::open(node_dir).expect("open node");
    log.repo()
        .version(VersionNumber::new(number).unwrap())
        .unwrap()
        .id
}

/// A source repository with versions 1 and 2.
fn source(plane: &ControlPlane, admin: &str, nodes: &Path) -> Created {
    let created = plane
        .create_repository(admin, &request("app", "op-source"), T0)
        .expect("create");
    let node = nodes.join(&created.repository);
    publish_one(&node, "one", 0);
    publish_one(&node, "two", 1);
    created
}

/// A node whose first permissions push fails after the fork has been copied, as if the reply
/// was lost on the way back.
struct LoseFirstApply {
    node: LocalNode,
    lose: Cell<bool>,
}

impl NodeLink for LoseFirstApply {
    fn provision(&self, repository: &RepositoryId) -> Result<(), ControlError> {
        self.node.provision(repository)
    }

    fn apply(&self, permissions: &AppliedPermissions) -> Result<(), ControlError> {
        if self.lose.replace(false) {
            return Err(ControlError::Node("injected: reply lost".into()));
        }
        self.node.apply(permissions)
    }

    fn head(&self, repository: &RepositoryId) -> Result<VersionNumber, ControlError> {
        self.node.head(repository)
    }

    fn fork(&self, plan: &ForkPlan) -> Result<Forked, ControlError> {
        self.node.fork(plan)
    }
}

/// A node that counts the permissions pushes it receives.
struct CountApplies {
    node: LocalNode,
    applies: Rc<Cell<u32>>,
}

impl NodeLink for CountApplies {
    fn provision(&self, repository: &RepositoryId) -> Result<(), ControlError> {
        self.node.provision(repository)
    }

    fn apply(&self, permissions: &AppliedPermissions) -> Result<(), ControlError> {
        self.applies.set(self.applies.get() + 1);
        self.node.apply(permissions)
    }

    fn head(&self, repository: &RepositoryId) -> Result<VersionNumber, ControlError> {
        self.node.head(repository)
    }

    fn fork(&self, plan: &ForkPlan) -> Result<Forked, ControlError> {
        self.node.fork(plan)
    }
}

#[test]
fn retrying_a_finished_fork_pushes_no_permissions_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plane, admin) = open(dir.path());
    let nodes = dir.path().join("nodes");
    let source = source(&plane, &admin, &nodes);
    let fork = fork_request(&source.repository, 2, "fork", "op-fork");
    let first = plane.fork_repository(&admin, &fork, T0).expect("fork");

    let applies = Rc::new(Cell::new(0));
    let counting = ControlPlane::with_node(
        dir.path(),
        Box::new(CountApplies {
            node: LocalNode::new(nodes),
            applies: applies.clone(),
        }),
        NodeTarget::self_hosted(),
    )
    .expect("open");
    let again = counting
        .fork_repository(&admin, &fork, T0 + 1)
        .expect("retry");

    assert_eq!(applies.get(), 0, "a Ready retry must not reset the epoch");
    assert_eq!(again.repository.epoch, 1);
    assert_eq!(again.origin, first.origin);
}

#[test]
fn a_fork_inherits_the_source_log_and_records_its_origin() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plane, admin) = open(dir.path());
    let nodes = dir.path().join("nodes");
    let source = source(&plane, &admin, &nodes);

    let forked = plane
        .fork_repository(
            &admin,
            &fork_request(&source.repository, 2, "fork", "op-fork"),
            T0,
        )
        .expect("fork");

    assert!(forked.repository.created);
    assert_eq!(forked.repository.epoch, 1);
    assert_eq!(forked.origin.source, source.repository);
    assert_eq!(forked.origin.number, VersionNumber::new(2).unwrap());
    assert_eq!(
        forked.origin.version,
        version_id(&nodes.join(&source.repository), 2)
    );
    assert!(forked.omitted.is_empty());
    assert_eq!(
        version_id(&nodes.join(&forked.repository.repository), 2),
        forked.origin.version,
        "the fork's version N carries the source's ID"
    );
    plane
        .repository_token(&admin, &forked.repository.repository, T0)
        .expect("the fork has its own token");
}

#[test]
fn repeating_a_fork_returns_the_same_repository() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plane, admin) = open(dir.path());
    let source = source(&plane, &admin, &dir.path().join("nodes"));
    let fork = fork_request(&source.repository, 1, "fork", "op-fork");

    let first = plane.fork_repository(&admin, &fork, T0).expect("fork");
    let again = plane.fork_repository(&admin, &fork, T0 + 5).expect("retry");

    assert!(!again.repository.created);
    assert_eq!(again.repository.repository, first.repository.repository);
    assert_eq!(again.origin, first.origin);
}

#[test]
fn an_operation_is_one_fork_or_one_create() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plane, admin) = open(dir.path());
    let source = source(&plane, &admin, &dir.path().join("nodes"));
    plane
        .fork_repository(
            &admin,
            &fork_request(&source.repository, 1, "fork", "op-fork"),
            T0,
        )
        .expect("fork");

    let other_version = plane
        .fork_repository(
            &admin,
            &fork_request(&source.repository, 2, "fork", "op-fork"),
            T0,
        )
        .unwrap_err();
    assert_eq!(other_version.code(), "operation_id_reused");
    let as_create = plane
        .create_repository(&admin, &request("other", "op-fork"), T0)
        .unwrap_err();
    assert_eq!(as_create.code(), "operation_id_reused");
}

#[test]
fn a_fork_past_the_source_head_is_refused_before_anything_is_created() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plane, admin) = open(dir.path());
    let source = source(&plane, &admin, &dir.path().join("nodes"));

    let refused = plane
        .fork_repository(
            &admin,
            &fork_request(&source.repository, 9, "fork", "op-fork"),
            T0,
        )
        .unwrap_err();
    assert_eq!(refused.code(), "invalid_input");

    // The refusal reserved nothing, so the same operation and name are free for a valid fork.
    let forked = plane
        .fork_repository(
            &admin,
            &fork_request(&source.repository, 2, "fork", "op-fork"),
            T0,
        )
        .expect("fork");
    assert!(forked.repository.created);
}

#[test]
fn a_lost_permissions_push_is_resumed_by_retrying_the_fork() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plane, admin) = open(dir.path());
    let nodes = dir.path().join("nodes");
    let source = source(&plane, &admin, &nodes);
    let fork = fork_request(&source.repository, 2, "fork", "op-fork");

    let lossy = ControlPlane::with_node(
        dir.path(),
        Box::new(LoseFirstApply {
            node: LocalNode::new(nodes.clone()),
            lose: Cell::new(true),
        }),
        NodeTarget::self_hosted(),
    )
    .expect("open");
    let lost = lossy.fork_repository(&admin, &fork, T0).unwrap_err();
    assert_eq!(lost.code(), "node_unavailable");

    let retried = plane.fork_repository(&admin, &fork, T0 + 1).expect("retry");
    assert!(!retried.repository.created);
    assert_eq!(
        retried.repository.epoch, 1,
        "the retry finishes provisioning"
    );
    assert_eq!(retried.origin.number, VersionNumber::new(2).unwrap());
    plane
        .repository_token(&admin, &retried.repository.repository, T0 + 1)
        .expect("the retried fork is ready");
}
