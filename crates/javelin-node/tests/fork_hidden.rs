//! Forks of a repository with hidden entries: the fork copies committed trees whole, so
//! its version IDs match the source's, and copies an opening only for a reader in its
//! audience.

use javelin::{
    FileEntry, LayerId, ObjectId, OperationId, POLICY_PATH, PublishAttribution, Reader, RepoPath,
    SparseChanges, VersionNumber,
};
use javelin_collaboration::Role;
use javelin_node::{FileWorldLog, fork};
use javelin_protocol::{codec::PublishOutcome, receiver::WorldLog};
use tempfile::TempDir;

fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}
fn path(s: &str) -> RepoPath {
    RepoPath::parse(s).unwrap()
}

fn publish(log: &mut FileWorldLog, name: &str, files: &[(&str, &[u8])]) {
    let layer = LayerId::parse(name).unwrap();
    let base = log.head().unwrap().number;
    log.create_layer(&layer, base).unwrap();
    let changes = SparseChanges::new(files.iter().map(|(p, bytes)| {
        let content = log.repository().put_blob(bytes).unwrap();
        (
            path(p),
            Some(FileEntry::Regular {
                content,
                executable: false,
            }),
        )
    }))
    .unwrap();
    let op = |s: &str| OperationId::parse(&format!("{s}-{name}")).unwrap();
    let expected = log
        .append(&op("write"), &layer, &changes)
        .unwrap()
        .checkpoint()
        .object();
    let outcome = log
        .publish_layer(
            &op("publish"),
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

fn source(dir: &TempDir) -> FileWorldLog {
    let mut log = FileWorldLog::open(dir.path()).unwrap();
    publish(
        &mut log,
        "one",
        &[
            (POLICY_PATH, b".env maintainers\ninternal/** members\n"),
            (".env", b"TOKEN=1\n"),
            ("internal/notes", b"notes\n"),
            ("src/a", b"a\n"),
        ],
    );
    publish(
        &mut log,
        "two",
        &[(".env", b"TOKEN=2\n"), ("src/a", b"b\n")],
    );
    log
}

fn hidden_commitments(log: &FileWorldLog, n: u64) -> Vec<ObjectId> {
    log.repo()
        .committed_at(v(n))
        .unwrap()
        .entries()
        .values()
        .filter_map(|entry| match entry {
            FileEntry::Hidden { commitment, .. } => Some(*commitment),
            _ => None,
        })
        .collect()
}

#[test]
fn an_outsiders_fork_keeps_commitments_and_matching_ids() {
    let (src_dir, dst_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let src = source(&src_dir);
    let mut dst = FileWorldLog::open(dst_dir.path()).unwrap();
    fork::fork_for(&src, "repo-src", &mut dst, v(2), None).unwrap();
    for n in 0..=2 {
        let (ours, theirs) = (
            dst.repo().version(v(n)).unwrap(),
            src.repo().version(v(n)).unwrap(),
        );
        assert_eq!((ours.id, ours.tree), (theirs.id, theirs.tree));
        assert_eq!(
            dst.repo().committed_at(v(n)).unwrap(),
            src.repo().committed_at(v(n)).unwrap()
        );
    }
    // The fork holds commitments only: no opening, no hidden bytes, and its view leaves
    // hidden entries closed.
    let commitments = [hidden_commitments(&src, 1), hidden_commitments(&src, 2)].concat();
    assert_eq!(commitments.len(), 4);
    assert!(
        dst.repo()
            .openings(&commitments, &Reader::maintainer())
            .unwrap()
            .is_empty()
    );
    for secret in [&b"TOKEN=1\n"[..], b"TOKEN=2\n", b"notes\n"] {
        assert!(!dst.repo().has_blob(ObjectId::for_blob(secret)).unwrap());
    }
    let view = dst.repo().manifest_at(v(2)).unwrap();
    assert!(view.entries()[&path(".env")].is_hidden());
    assert!(view.entries()[&path("internal")].is_hidden());
    dst.repo().check().unwrap();
}

#[test]
fn a_fork_copies_the_openings_its_reader_may_read() {
    let src_dir = TempDir::new().unwrap();
    let src = source(&src_dir);
    let commitments = [hidden_commitments(&src, 1), hidden_commitments(&src, 2)].concat();
    for (role, opened) in [(Role::Read, 2), (Role::Maintain, 4)] {
        let dst_dir = TempDir::new().unwrap();
        let mut dst = FileWorldLog::open(dst_dir.path()).unwrap();
        fork::fork(&src, "repo-src", &mut dst, v(2), role).unwrap();
        let (ours, theirs) = (dst.repo().head().unwrap(), src.repo().head().unwrap());
        assert_eq!((ours.id, ours.tree), (theirs.id, theirs.tree));
        let held = dst
            .repo()
            .openings(&commitments, &Reader::maintainer())
            .unwrap();
        assert_eq!(held.len(), opened, "{role:?}");
        let view = dst.repo().manifest_at(v(2)).unwrap();
        assert!(!view.entries().contains_key("internal"));
        assert_eq!(
            view.entries()[&path("internal/notes")],
            src.repo().manifest_at(v(2)).unwrap().entries()[&path("internal/notes")]
        );
        assert_eq!(
            view.entries()[&path(".env")].is_hidden(),
            role != Role::Maintain
        );
        dst.repo().check().unwrap();
    }
}
