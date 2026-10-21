use std::{fs, path::Path};

use javelin::{LayerId, ObjectId, OperationId, PublishAttribution, VersionNumber};
use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_identity::{Scope, test_keys};
use javelin_node::{
    FileWorldLog, TraceVisibility,
    auth::Gate,
    fork::{self, Origin},
};
use javelin_protocol::{
    codec::{PublishOutcome, code},
    receiver::WorldLog,
};
use rusqlite::Connection;
use tempfile::TempDir;

const NOW: u64 = 1_000_000;
const REPO: &str = "repo-1";
const SECRET: &[u8] = b"private reasoning";

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn layer(s: &str) -> LayerId {
    LayerId::parse(s).unwrap()
}
fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}

fn gate(visibility: Visibility) -> Gate {
    let permissions = AppliedPermissions {
        repository: RepositoryId::parse(REPO).unwrap(),
        epoch: 1,
        visibility,
        roles: [(PrincipalId::parse("alice").unwrap(), Role::Maintain)]
            .into_iter()
            .collect(),
        required_approvals: 1,
    };
    let mut gate = Gate::new(RepositoryId::parse(REPO).unwrap(), test_keys::keys());
    gate.apply(permissions).unwrap();
    gate
}

fn token(sub: &str, scopes: &[Scope]) -> String {
    let claims = test_keys::claims(sub, REPO, scopes, NOW, NOW + 600, &format!("{sub}-tok"));
    test_keys::issuer().issue(&claims).unwrap()
}

/// Publishes one change to file `path` as Layer `name` over World `base`. A trace, when given,
/// is the Publish's agent context.
fn publish(
    log: &mut FileWorldLog,
    name: &str,
    base: u64,
    path: &str,
    bytes: &[u8],
    trace: Option<ObjectId>,
) -> PublishOutcome {
    let layer = layer(name);
    log.create_layer(&layer, v(base)).unwrap();
    let content = log.repository().put_blob(bytes).unwrap();
    let changes = javelin::SparseChanges::new([(
        javelin::RepoPath::parse(path).unwrap(),
        Some(javelin::FileEntry::Regular {
            content,
            executable: false,
        }),
    )])
    .unwrap();
    let expected = log
        .append(&op(&format!("write-{name}")), &layer, &changes)
        .unwrap()
        .checkpoint()
        .object();
    let attribution = match trace {
        Some(context) => PublishAttribution::Agent {
            context: Some(context),
            allow_missing_trace: false,
        },
        None => PublishAttribution::Human,
    };
    log.publish_layer(
        &op(&format!("publish-{name}")),
        &layer,
        expected,
        v(base),
        &attribution,
    )
    .unwrap()
}

/// A source with four versions. v3 cites a public trace, and v4 cites a trace that is then
/// restricted to Maintain.
fn source(dir: &Path) -> (FileWorldLog, ObjectId, ObjectId) {
    let mut log = FileWorldLog::open(dir).unwrap();
    assert!(matches!(
        publish(&mut log, "one", 0, "a.txt", b"one", None),
        PublishOutcome::Published { .. }
    ));
    assert!(matches!(
        publish(&mut log, "two", 1, "b.txt", b"two", None),
        PublishOutcome::Published { .. }
    ));
    let public = log.repository().put_blob(b"public reasoning").unwrap();
    assert!(matches!(
        publish(&mut log, "three", 2, "c.txt", b"three", Some(public)),
        PublishOutcome::Published { .. }
    ));
    let secret = log.repository().put_blob(SECRET).unwrap();
    assert!(matches!(
        publish(&mut log, "four", 3, "d.txt", b"four", Some(secret)),
        PublishOutcome::Published { .. }
    ));
    let alice = token("alice", &[Scope::Publish]);
    log.traces()
        .restrict(
            &gate(Visibility::Public),
            log.repo(),
            Some(&alice),
            NOW,
            secret,
        )
        .unwrap();
    (log, public, secret)
}

/// (number, ID, tree) of every version, which is what a fork must reproduce.
fn shape(log: &FileWorldLog) -> Vec<(u64, ObjectId, ObjectId)> {
    log.repo()
        .history()
        .unwrap()
        .iter()
        .map(|version| (version.number.get(), version.id, version.tree))
        .collect()
}

/// Whether any file under `dir` holds `needle`.
fn holds(dir: &Path, needle: &[u8]) -> bool {
    fs::read_dir(dir).unwrap().any(|entry| {
        let path = entry.unwrap().path();
        if path.is_dir() {
            return holds(&path, needle);
        }
        fs::read(&path)
            .unwrap()
            .windows(needle.len())
            .any(|window| window == needle)
    })
}

#[test]
fn a_fork_reproduces_the_source_versions_with_equal_ids_and_trees() {
    let (src_dir, dst_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (src, _, _) = source(src_dir.path());
    let mut dst = FileWorldLog::open(dst_dir.path()).unwrap();

    let forked = fork::fork(&src, "repo-src", &mut dst, v(4), Role::Maintain).unwrap();

    assert_eq!(
        forked.origin,
        Origin {
            source: "repo-src".into(),
            number: v(4),
            version: src.repo().version(v(4)).unwrap().id,
        }
    );
    assert!(forked.omitted.is_empty());
    assert_eq!(dst.head().unwrap().number, v(4));
    assert_eq!(shape(&dst), shape(&src));
}

#[test]
fn a_fork_at_an_earlier_version_stops_there() {
    let (src_dir, dst_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (src, _, _) = source(src_dir.path());
    let mut dst = FileWorldLog::open(dst_dir.path()).unwrap();

    fork::fork(&src, "repo-src", &mut dst, v(2), Role::Maintain).unwrap();

    assert_eq!(dst.head().unwrap().number, v(2));
    assert_eq!(shape(&dst), shape(&src)[..3]);
    assert_eq!(src.head().unwrap().number, v(4));
}

#[test]
fn a_fork_publishes_on_its_own_log_and_leaves_the_source_alone() {
    let (src_dir, dst_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (src, _, _) = source(src_dir.path());
    let mut dst = FileWorldLog::open(dst_dir.path()).unwrap();
    fork::fork(&src, "repo-src", &mut dst, v(4), Role::Maintain).unwrap();

    assert!(matches!(
        publish(&mut dst, "after", 4, "e.txt", b"after", None),
        PublishOutcome::Published { number, .. } if number == v(5)
    ));
    assert_eq!(dst.head().unwrap().number, v(5));
    assert_eq!(src.head().unwrap().number, v(4));
    assert_eq!(shape(&src).len(), 5);
}

#[test]
fn a_fork_outlives_its_source() {
    let (src_dir, dst_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (src, _, _) = source(src_dir.path());
    let mut dst = FileWorldLog::open(dst_dir.path()).unwrap();
    let expected = {
        fork::fork(&src, "repo-src", &mut dst, v(4), Role::Maintain).unwrap();
        shape(&src)
    };
    drop(src);
    fs::remove_dir_all(src_dir.path()).unwrap();
    drop(dst);

    let reopened = FileWorldLog::open(dst_dir.path()).unwrap();
    assert_eq!(shape(&reopened), expected);
    assert_eq!(fork::origin(&reopened).unwrap().unwrap().number, v(4));
}

#[test]
fn a_reader_who_cannot_see_a_restricted_trace_gets_a_marker_and_no_bytes() {
    let (src_dir, dst_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (src, public, secret) = source(src_dir.path());
    let source_shape = shape(&src);
    // Windows locks the files an open log holds, so each scan runs with its log closed.
    drop(src);
    assert!(
        holds(src_dir.path(), SECRET),
        "the scan must see the source's bytes"
    );
    let src = FileWorldLog::open(src_dir.path()).unwrap();
    let mut dst = FileWorldLog::open(dst_dir.path()).unwrap();

    let forked = fork::fork(&src, "repo-src", &mut dst, v(4), Role::Read).unwrap();

    assert_eq!(forked.omitted, vec![secret]);
    assert!(!dst.repo().has_blob(secret).unwrap());
    drop(dst);
    assert!(
        !holds(dst_dir.path(), SECRET),
        "no copy of the restricted bytes"
    );
    let dst = FileWorldLog::open(dst_dir.path()).unwrap();
    assert_eq!(
        dst.traces().visibility(public).unwrap(),
        TraceVisibility::Inherit
    );
    assert!(dst.repo().has_blob(public).unwrap());
    assert_eq!(
        fork::omitted_traces(&dst).unwrap(),
        vec![secret],
        "the marker is recorded in the fork"
    );
    assert_eq!(
        shape(&dst),
        source_shape,
        "omission does not change the log"
    );
}

#[test]
fn a_fork_of_a_fork_inherits_the_traces_and_omissions_of_its_source() {
    let (src_dir, first_dir, second_dir) = (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    );
    let (src, public, secret) = source(src_dir.path());
    let source_shape = shape(&src);
    {
        let mut first = FileWorldLog::open(first_dir.path()).unwrap();
        fork::fork(&src, "repo-src", &mut first, v(4), Role::Read).unwrap();
    }
    // The first fork's versions have no publish outcomes of their own, so its traces come only
    // from what it inherited.
    let first = FileWorldLog::open(first_dir.path()).unwrap();
    let mut second = FileWorldLog::open(second_dir.path()).unwrap();

    let forked = fork::fork(&first, "repo-first", &mut second, v(4), Role::Maintain).unwrap();

    assert_eq!(
        forked.omitted,
        vec![secret],
        "omitted in the first fork, so still omitted"
    );
    assert!(
        second.repo().has_blob(public).unwrap(),
        "the public trace is inherited"
    );
    assert!(!second.repo().has_blob(secret).unwrap());
    assert_eq!(shape(&second), source_shape);
}

#[test]
fn a_fork_of_a_fork_carries_only_the_traces_up_to_its_fork_point() {
    let (src_dir, first_dir, second_dir) = (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    );
    let (src, public, secret) = source(src_dir.path());
    {
        let mut first = FileWorldLog::open(first_dir.path()).unwrap();
        fork::fork(&src, "repo-src", &mut first, v(4), Role::Maintain).unwrap();
    }
    let first = FileWorldLog::open(first_dir.path()).unwrap();
    let mut second = FileWorldLog::open(second_dir.path()).unwrap();

    // Version 3 names the public trace; the secret trace is first named at version 4.
    let forked = fork::fork(&first, "repo-first", &mut second, v(3), Role::Maintain).unwrap();

    assert!(forked.omitted.is_empty());
    assert!(second.repo().has_blob(public).unwrap());
    assert!(!second.repo().has_blob(secret).unwrap());
}

#[test]
fn restriction_passes_through_a_fork_chain() {
    let (src_dir, first_dir, read_dir, maintain_dir) = (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    );
    let (src, _, secret) = source(src_dir.path());
    {
        let mut first = FileWorldLog::open(first_dir.path()).unwrap();
        fork::fork(&src, "repo-src", &mut first, v(4), Role::Maintain).unwrap();
    }
    let first = FileWorldLog::open(first_dir.path()).unwrap();

    {
        let mut read = FileWorldLog::open(read_dir.path()).unwrap();
        let forked = fork::fork(&first, "repo-first", &mut read, v(4), Role::Read).unwrap();
        assert_eq!(forked.omitted, vec![secret]);
        assert!(!read.repo().has_blob(secret).unwrap());
    }

    let mut maintain = FileWorldLog::open(maintain_dir.path()).unwrap();
    let forked = fork::fork(&first, "repo-first", &mut maintain, v(4), Role::Maintain).unwrap();
    assert!(forked.omitted.is_empty());
    assert!(maintain.repo().has_blob(secret).unwrap());
    assert_eq!(
        maintain.traces().visibility(secret).unwrap(),
        TraceVisibility::Restricted
    );
}

#[test]
fn a_maintainer_fork_keeps_a_restricted_trace_restricted() {
    let (src_dir, dst_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (src, _, secret) = source(src_dir.path());
    let mut dst = FileWorldLog::open(dst_dir.path()).unwrap();

    let forked = fork::fork(&src, "repo-src", &mut dst, v(4), Role::Maintain).unwrap();

    assert!(forked.omitted.is_empty());
    assert!(dst.repo().has_blob(secret).unwrap());
    assert_eq!(
        dst.traces().visibility(secret).unwrap(),
        TraceVisibility::Restricted
    );
    let anonymous = gate(Visibility::Public);
    assert_eq!(
        dst.traces()
            .read(&anonymous, dst.repo(), None, NOW, secret)
            .unwrap_err()
            .code,
        code::FORBIDDEN
    );
}

#[test]
fn a_fork_resumes_from_the_target_head_when_its_origin_was_never_written() {
    let (src_dir, dst_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (src, _, _) = source(src_dir.path());
    let mut dst = FileWorldLog::open(dst_dir.path()).unwrap();
    fork::fork(&src, "repo-src", &mut dst, v(1), Role::Maintain).unwrap();
    // A crash after the log copy and before the origin is written leaves exactly this.
    drop(dst);
    Connection::open(dst_dir.path().join("node.db"))
        .unwrap()
        .execute("DELETE FROM fork_origin", [])
        .unwrap();
    let mut dst = FileWorldLog::open(dst_dir.path()).unwrap();
    assert!(fork::origin(&dst).unwrap().is_none());

    let forked = fork::fork(&src, "repo-src", &mut dst, v(4), Role::Maintain).unwrap();

    assert_eq!(forked.origin.number, v(4));
    assert_eq!(shape(&dst), shape(&src));
}

#[test]
fn a_repeated_fork_returns_its_origin_and_a_different_one_is_refused() {
    let (src_dir, dst_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (src, _, _) = source(src_dir.path());
    let mut dst = FileWorldLog::open(dst_dir.path()).unwrap();
    let first = fork::fork(&src, "repo-src", &mut dst, v(4), Role::Read).unwrap();

    let again = fork::fork(&src, "repo-src", &mut dst, v(4), Role::Read).unwrap();
    assert_eq!(again, first);

    let other_source = fork::fork(&src, "repo-other", &mut dst, v(4), Role::Read).unwrap_err();
    assert_eq!(other_source.code, code::INVALID_INPUT);
    let other_version = fork::fork(&src, "repo-src", &mut dst, v(3), Role::Read).unwrap_err();
    assert_eq!(other_version.code, code::INVALID_INPUT);
}

#[test]
fn a_fork_past_the_source_head_is_refused() {
    let (src_dir, dst_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (src, _, _) = source(src_dir.path());
    let mut dst = FileWorldLog::open(dst_dir.path()).unwrap();

    let refused = fork::fork(&src, "repo-src", &mut dst, v(9), Role::Maintain).unwrap_err();

    assert_eq!(refused.code, code::INVALID_INPUT);
    assert_eq!(dst.head().unwrap().number, v(0));
    assert!(fork::origin(&dst).unwrap().is_none());
}

#[test]
fn the_origin_and_omissions_survive_a_restart() {
    let (src_dir, dst_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (src, _, secret) = source(src_dir.path());
    let mut dst = FileWorldLog::open(dst_dir.path()).unwrap();
    let forked = fork::fork(&src, "repo-src", &mut dst, v(4), Role::Read).unwrap();
    drop(dst);

    let reopened = FileWorldLog::open(dst_dir.path()).unwrap();
    assert_eq!(fork::origin(&reopened).unwrap(), Some(forked.origin));
    assert_eq!(fork::omitted_traces(&reopened).unwrap(), vec![secret]);
}
