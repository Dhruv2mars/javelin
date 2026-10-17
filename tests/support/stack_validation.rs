//! Counts graph loads, not wall time, to catch repeated ancestry hashing.
use super::*;
use tempfile::TempDir;

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn applied(update: LayerUpdate) -> LayerHead {
    match update {
        LayerUpdate::Applied {
            state: LayerState::Active { head },
        } => head,
        other => panic!("{other:?}"),
    }
}

#[test]
fn shared_ancestry_is_checked_once_in_each_integrity_transaction() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let base = repo.layer_base(VersionNumber::new(0).unwrap()).unwrap();
    let mut target = repo
        .create_layer(&LayerId::parse("target").unwrap(), &base)
        .unwrap();
    let mut first_content = None;
    for i in 0..8 {
        let content = repo.put_blob(&[i]).unwrap();
        first_content.get_or_insert(content);
        let changes = SparseChanges::new([(
            RepoPath::parse("file").unwrap(),
            Some(FileEntry::Regular {
                content,
                executable: false,
            }),
        )])
        .unwrap();
        let source = repo
            .create_layer(&LayerId::parse(&format!("source-{i}")).unwrap(), &base)
            .unwrap();
        let source = applied(
            repo.capture_layer(&op(&format!("capture-{i}")), &source, &changes)
                .unwrap(),
        );
        target = applied(
            repo.commit_stack(
                &op(&format!("stack-{i}")),
                &target,
                source.checkpoint(),
                &changes,
            )
            .unwrap(),
        );
    }
    // Include a stale candidate, which must be verified despite having no head.
    let initial = repo
        .create_layer(&LayerId::parse("target").unwrap(), &base)
        .unwrap();
    let changes = SparseChanges::new([]).unwrap();
    repo.commit_stack(&op("stale"), &initial, target.checkpoint(), &changes)
        .unwrap();
    let expected: i64 = repo
        .conn
        .query_row("SELECT count(*) FROM objects WHERE kind=3", [], |r| {
            r.get(0)
        })
        .unwrap();
    for _ in 0..2 {
        CHECKPOINT_NODE_READS.with(|reads| reads.borrow_mut().clear());
        repo.check().unwrap();
        CHECKPOINT_NODE_READS.with(|reads| {
            let reads = reads.borrow();
            assert_eq!(reads.len(), expected as usize);
            assert!(
                reads.values().all(|&count| count == 1),
                "checkpoint load counts in one transaction: {:?}",
                reads.values().collect::<Vec<_>>()
            );
        });
    }
    // A separate transaction must never trust the previous one's validation.
    let raw = rusqlite::Connection::open(root.path().join(".javelin/repository.sqlite3")).unwrap();
    raw.execute_batch("DROP TRIGGER objects_no_UPDATE;")
        .unwrap();
    raw.execute(
        "UPDATE objects SET bytes=x'ff' WHERE id=?1",
        [first_content.unwrap().to_string()],
    )
    .unwrap();
    assert!(repo.check().is_err());
    assert!(
        repo.capture_layer(&op("after-damage"), &target, &changes)
            .is_err()
    );
}
