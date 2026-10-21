//! Fixtures shared by the check tests: a node with applied permissions, tokens that carry
//! check-name patterns, and helpers for Layers, reports and Publishes.

#![allow(dead_code)] // each test binary uses a different subset

use javelin::{
    FileEntry, LayerId, ObjectId, OperationId, PublishAttribution, RepoPath, SparseChanges,
    VersionNumber,
};
use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_identity::{Scope, test_keys};
use javelin_node::{FileWorldLog, auth::Gate, checks};
use javelin_protocol::{
    codec::{CheckOutcome, CheckReport, Conclusion, PublishOutcome},
    receiver::WorldLog,
};
use tempfile::TempDir;

pub const NOW: u64 = 1_000_000;
pub const REPO: &str = "repo-1";

pub fn op(value: &str) -> OperationId {
    OperationId::parse(value).unwrap()
}

pub fn layer(value: &str) -> LayerId {
    LayerId::parse(value).unwrap()
}

pub fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}

/// `alice` and `rita` review, `mona` maintains, and `ci-bot` and `ci-2` are read-only
/// reporters. `rita` is absent once her role is removed. `ci-bot` is absent from epoch 2.
pub fn permissions(epoch: u64, reporter_roles: bool, rita: bool) -> AppliedPermissions {
    let mut roles = vec![
        (PrincipalId::parse("alice").unwrap(), Role::Review),
        (PrincipalId::parse("mona").unwrap(), Role::Maintain),
    ];
    if rita {
        roles.push((PrincipalId::parse("rita").unwrap(), Role::Review));
    }
    if reporter_roles {
        roles.push((PrincipalId::parse("ci-bot").unwrap(), Role::Read));
        roles.push((PrincipalId::parse("ci-2").unwrap(), Role::Read));
    }
    AppliedPermissions {
        repository: RepositoryId::parse(REPO).unwrap(),
        epoch,
        visibility: Visibility::Private,
        roles: roles.into_iter().collect(),
        required_approvals: 1,
    }
}

pub fn gate(permissions: AppliedPermissions) -> Gate {
    let mut gate = Gate::new(RepositoryId::parse(REPO).unwrap(), test_keys::keys());
    gate.apply(permissions).unwrap();
    gate
}

/// A token for `sub` with `scopes`, and `checks` as its check-name patterns.
pub fn token(sub: &str, scopes: &[Scope], checks: &[&str]) -> String {
    let mut claims = test_keys::claims(sub, REPO, scopes, NOW, NOW + 600, sub);
    claims.checks = checks.iter().map(|pattern| pattern.to_string()).collect();
    test_keys::issuer().issue(&claims).unwrap()
}

/// A reporter token: the `checks:report` scope with `patterns`.
pub fn reporter(sub: &str, patterns: &[&str]) -> String {
    token(sub, &[Scope::CheckReport], patterns)
}

/// Creates Layer `name` over World 0 and writes one file to it. Returns the checkpoint.
pub fn write(log: &mut FileWorldLog, operation: &str, name: &str, path: &str) -> ObjectId {
    let content = log.repository().put_blob(path.as_bytes()).unwrap();
    let changes = SparseChanges::new([(
        RepoPath::parse(path).unwrap(),
        Some(FileEntry::Regular {
            content,
            executable: false,
        }),
    )])
    .unwrap();
    if log.layer_state(&layer(name)).is_err() {
        log.create_layer(&layer(name), v(0)).unwrap();
    }
    log.append(&op(operation), &layer(name), &changes)
        .unwrap()
        .checkpoint()
        .object()
}

/// The tree a checkpoint composes to: the subject of a check on it.
pub fn tree_of(log: &mut FileWorldLog, checkpoint: ObjectId) -> ObjectId {
    let repo = log.repository();
    let manifest = repo
        .checkpoint_manifest(javelin::CheckpointId::parse(&checkpoint.to_string()).unwrap())
        .unwrap();
    repo.put_snapshot(&manifest).unwrap()
}

/// A report of `conclusion` for `tree`, repository `REPO`, with no checkpoint named.
pub fn report_for(
    operation: &str,
    tree: ObjectId,
    name: &str,
    conclusion: Conclusion,
) -> CheckReport {
    CheckReport {
        operation: op(operation),
        repository: REPO.into(),
        subject_tree: tree,
        subject_checkpoint: None,
        name: name.into(),
        conclusion,
        details_url: None,
        summary: None,
        started_at: NOW,
        completed_at: NOW,
    }
}

pub fn report(
    log: &mut FileWorldLog,
    gate: &Gate,
    token: &str,
    message: &CheckReport,
) -> CheckOutcome {
    checks::report(log, gate, Some(token), NOW, message).unwrap()
}

/// Sets the node's required checks, each `(name, reporter)`.
pub fn require(log: &mut FileWorldLog, required: &[(&str, Option<&str>)]) {
    let required: Vec<checks::RequiredCheck> = required
        .iter()
        .map(|(name, reporter)| checks::RequiredCheck {
            name: name.to_string(),
            reporter: reporter.map(str::to_owned),
        })
        .collect();
    checks::set_required(log, &required).unwrap();
}

/// Publishes `layer_name` at `expected` over World 0, with a human attribution.
pub fn publish(
    log: &mut FileWorldLog,
    operation: &str,
    layer_name: &str,
    expected: ObjectId,
) -> PublishOutcome {
    log.publish_layer(
        &op(operation),
        &layer(layer_name),
        expected,
        v(0),
        &PublishAttribution::Human,
    )
    .unwrap()
}

pub fn refused(code: &str) -> PublishOutcome {
    PublishOutcome::Refused { code: code.into() }
}

pub fn new_node(dir: &TempDir) -> FileWorldLog {
    FileWorldLog::open(dir.path()).unwrap()
}
