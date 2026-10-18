//! Publish's write transaction must not grow with World history.
use std::time::{Duration, Instant};

use rusqlite::params;
use tempfile::TempDir;

use super::lock_probe::LAST;
use crate::{
    FileEntry, LayerId, LayerState, LayerUpdate, Manifest, OperationId, RepoPath, Repository,
    SparseChanges, VersionNumber,
    domain::ObjectKind,
    publish::{PublishAttribution, PublishOutcome},
    repository::insert_object,
};

struct Cost {
    publish: Duration,
    locked: Duration,
    ops: u64,
}

/// Builds `prior` accepted Worlds over a constant-size tree, then publishes one Layer.
fn publish_after(prior: u64) -> Cost {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let tx = repo.conn.transaction().unwrap();
    for n in 1..=prior {
        let content = insert_object(&tx, ObjectKind::Blob, n.to_string().as_bytes()).unwrap();
        let manifest = Manifest::new([(
            RepoPath::parse("counter").unwrap(),
            FileEntry::Regular {
                content,
                executable: false,
            },
        )])
        .unwrap();
        let manifest =
            insert_object(&tx, ObjectKind::Manifest, &manifest.canonical_bytes()).unwrap();
        let operation = format!("world-{n}");
        tx.execute(
            "INSERT INTO attempts(id,expected,manifest,accepted,result) VALUES(?1,?2,?3,1,?4)",
            params![operation, n as i64 - 1, manifest.to_string(), n as i64],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO versions(number,manifest,operation) VALUES(?1,?2,?3)",
            params![n as i64, manifest.to_string(), operation],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let base = repo.layer_base(VersionNumber::new(prior).unwrap()).unwrap();
    let initial = repo
        .create_layer(&LayerId::parse("layer").unwrap(), &base)
        .unwrap();
    let content = repo.put_blob(b"layer").unwrap();
    let changes = SparseChanges::new([(
        RepoPath::parse("file").unwrap(),
        Some(FileEntry::Regular {
            content,
            executable: false,
        }),
    )])
    .unwrap();
    let LayerUpdate::Applied {
        state: LayerState::Active { head },
    } = repo
        .capture_layer(&OperationId::parse("capture").unwrap(), &initial, &changes)
        .unwrap()
    else {
        panic!("capture failed")
    };
    LAST.set(None);
    let start = Instant::now();
    let receipt = repo
        .publish_layer(
            &OperationId::parse("publish").unwrap(),
            &head,
            &PublishAttribution::Human,
            |_, _| Ok(()),
        )
        .unwrap();
    let publish = start.elapsed();
    assert!(matches!(receipt.outcome, PublishOutcome::Published { .. }));
    let (locked, ops) = LAST.take().expect("write transaction measured");
    repo.check().unwrap();
    Cost {
        publish,
        locked,
        ops,
    }
}

#[test]
fn publish_write_transaction_does_not_grow_with_history() {
    let small = publish_after(1);
    let large = publish_after(1000);
    for (prior, cost) in [(1, &small), (1000, &large)] {
        println!(
            "prior Worlds {prior}: publish {:?}, write transaction {:?}, {} SQLite VM instructions",
            cost.publish, cost.locked, cost.ops
        );
    }
    // VM instruction counts are deterministic; B-tree depth is not an instruction.
    assert!(
        large.ops <= small.ops + small.ops / 20,
        "write transaction grew with history: {} -> {} instructions",
        small.ops,
        large.ops
    );
}
