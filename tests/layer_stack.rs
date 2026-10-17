use javelin::{
    CheckpointId, Error, FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, OperationId,
    PublishAttribution, RepoPath, Repository, SparseChanges, VersionNumber,
};
use rusqlite::Connection;
use tempfile::TempDir;

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn layer(repo: &mut Repository, s: &str) -> LayerHead {
    let base = repo.layer_base(VersionNumber::new(0).unwrap()).unwrap();
    repo.create_layer(&LayerId::parse(s).unwrap(), &base)
        .unwrap()
}
fn applied(outcome: LayerUpdate) -> LayerHead {
    match outcome {
        LayerUpdate::Applied {
            state: LayerState::Active { head },
        } => head,
        other => panic!("{other:?}"),
    }
}
fn delta(repo: &mut Repository, value: &[u8]) -> SparseChanges {
    SparseChanges::new([(
        RepoPath::parse("file").unwrap(),
        Some(FileEntry::Regular {
            content: repo.put_blob(value).unwrap(),
            executable: false,
        }),
    )])
    .unwrap()
}
fn db(root: &TempDir) -> Connection {
    Connection::open(root.path().join(".javelin/repository.sqlite3")).unwrap()
}

#[test]
fn native_stack_binds_input_and_carries_ancestry_through_capture_clone_discard_publish() {
    for publish in [false, true] {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let source = layer(&mut repo, "source");
        let changes = delta(&mut repo, b"source");
        let source = applied(
            repo.capture_layer(&op("source"), &source, &changes)
                .unwrap(),
        );
        let frozen = repo.read_checkpoint(source.checkpoint()).unwrap();
        let target = layer(&mut repo, "target");
        let result = repo
            .commit_stack(&op("stack"), &target, source.checkpoint(), &changes)
            .unwrap();
        let stacked = applied(result.clone());
        assert_eq!(
            repo.read_checkpoint(stacked.checkpoint())
                .unwrap()
                .stack_sources(),
            &[source.checkpoint()]
        );
        let later = delta(&mut repo, b"owner resolution");
        let next = applied(repo.capture_layer(&op("later"), &stacked, &later).unwrap());
        let next_checkpoint = repo.read_checkpoint(next.checkpoint()).unwrap();
        assert_eq!(next_checkpoint.stack_sources(), &[source.checkpoint()]);
        let clone = repo
            .clone_layer(&LayerId::parse("clone").unwrap(), next.checkpoint())
            .unwrap();
        assert_eq!(clone.checkpoint(), next.checkpoint());
        if publish {
            repo.publish_layer(&op("publish"), &next, &PublishAttribution::Human, |_, _| {
                Ok(())
            })
            .unwrap();
        } else {
            repo.discard_layer(&op("discard"), &next).unwrap();
        }
        // Original capture and Stack replays remain historical Active outcomes.
        assert_eq!(
            repo.capture_layer(&op("later"), &stacked, &later).unwrap(),
            LayerUpdate::Applied {
                state: LayerState::Active { head: next }
            }
        );
        drop(repo);
        let mut repo = Repository::open(root.path()).unwrap();
        assert_eq!(
            repo.commit_stack(&op("stack"), &target, source.checkpoint(), &changes)
                .unwrap(),
            result
        );
        assert_eq!(
            repo.stack_operation(&op("stack"), &target, source.checkpoint())
                .unwrap(),
            Some(result)
        );
        for error in [
            repo.commit_stack(&op("stack"), &target, target.checkpoint(), &changes)
                .unwrap_err(),
            repo.commit_stack(&op("stack"), &target, source.checkpoint(), &later)
                .unwrap_err(),
            repo.capture_layer(&op("stack"), &target, &changes)
                .unwrap_err(),
            repo.commit_stack(&op("later"), &stacked, source.checkpoint(), &later)
                .unwrap_err(),
        ] {
            assert!(matches!(error, Error::OperationIdReused(_)), "{error}");
        }
        assert_eq!(repo.read_checkpoint(source.checkpoint()).unwrap(), frozen);
        repo.check().unwrap();
    }
}

#[test]
fn stale_stack_replay_keeps_original_observation_and_rejects_new_target() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let source = layer(&mut repo, "source");
    let target = layer(&mut repo, "target");
    let changes = delta(&mut repo, b"winner");
    let winner = applied(
        repo.capture_layer(&op("winner"), &target, &changes)
            .unwrap(),
    );
    let stale = repo
        .commit_stack(&op("stack"), &target, source.checkpoint(), &changes)
        .unwrap();
    assert_eq!(
        stale,
        LayerUpdate::Stale {
            state: LayerState::Active {
                head: winner.clone()
            }
        }
    );
    let later = applied(
        repo.capture_layer(&op("later"), &winner, &SparseChanges::new([]).unwrap())
            .unwrap(),
    );
    drop(repo);
    let mut repo = Repository::open(root.path()).unwrap();
    assert_eq!(
        repo.commit_stack(&op("stack"), &target, source.checkpoint(), &changes)
            .unwrap(),
        stale
    );
    assert_eq!(
        repo.stack_operation(&op("stack"), &target, source.checkpoint())
            .unwrap(),
        Some(stale)
    );
    assert!(matches!(
        repo.stack_operation(&op("stack"), &later, source.checkpoint()),
        Err(Error::OperationIdReused(_))
    ));
    assert!(
        repo.read_checkpoint(later.checkpoint())
            .unwrap()
            .stack_sources()
            .is_empty()
    );
    repo.check().unwrap();
}

#[test]
fn failed_storage_rolls_back_checkpoint_ancestry_head_and_retry_record() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let source = layer(&mut repo, "source");
    let target = layer(&mut repo, "target");
    let changes = delta(&mut repo, b"new content");
    let before = repo.check().unwrap().objects;
    let raw = db(&root);
    raw.execute_batch("CREATE TRIGGER refuse_stack BEFORE INSERT ON stacks BEGIN SELECT RAISE(ABORT,'test refusal'); END;").unwrap();
    assert!(
        repo.commit_stack(&op("stack"), &target, source.checkpoint(), &changes)
            .is_err()
    );
    assert_eq!(repo.check().unwrap().objects, before);
    assert_eq!(
        repo.layer_state(target.layer()).unwrap(),
        LayerState::Active {
            head: target.clone()
        }
    );
    assert_eq!(
        repo.stack_operation(&op("stack"), &target, source.checkpoint())
            .unwrap(),
        None
    );
    raw.execute_batch("DROP TRIGGER refuse_stack;").unwrap();
    repo.commit_stack(&op("stack"), &target, source.checkpoint(), &changes)
        .unwrap();
    assert!(raw.execute("UPDATE stacks SET source=source", []).is_err());
    assert!(raw.execute("DELETE FROM stacks", []).is_err());
    repo.check().unwrap();
}

#[test]
fn ancestry_graph_and_operation_corruption_fail_existing_integrity_boundaries() {
    for damage in ["source-blob", "source-checkpoint", "binding", "orphan"] {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let source = layer(&mut repo, "source");
        let changes = delta(&mut repo, b"source only");
        let source = applied(
            repo.capture_layer(&op("capture"), &source, &changes)
                .unwrap(),
        );
        let target = layer(&mut repo, "target");
        let stacked = applied(
            repo.commit_stack(
                &op("stack"),
                &target,
                source.checkpoint(),
                &SparseChanges::new([]).unwrap(),
            )
            .unwrap(),
        );
        let raw = db(&root);
        match damage {
            "source-blob" | "source-checkpoint" => {
                let id = if damage == "source-blob" {
                    changes
                        .entries()
                        .values()
                        .next()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .content()
                } else {
                    source.checkpoint().object()
                };
                raw.execute_batch("DROP TRIGGER objects_no_UPDATE;")
                    .unwrap();
                raw.execute(
                    "UPDATE objects SET bytes=x'00' WHERE id=?1",
                    [id.to_string()],
                )
                .unwrap();
                assert!(repo.read_checkpoint(stacked.checkpoint()).is_err());
            }
            "binding" => {
                raw.execute_batch("DROP TRIGGER stacks_no_UPDATE;").unwrap();
                raw.execute(
                    "UPDATE stacks SET source=?1",
                    [target.checkpoint().object().to_string()],
                )
                .unwrap();
            }
            "orphan" => {
                raw.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
                raw.execute(
                    "INSERT INTO stacks(id,source,binding) SELECT 'orphan',?1,binding FROM stacks LIMIT 1",
                    [source.checkpoint().object().to_string()],
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(repo.check().is_err(), "{damage}");
        assert!(repo.layer_state(target.layer()).is_err(), "{damage}");
        assert!(
            repo.capture_layer(&op("next"), &stacked, &SparseChanges::new([]).unwrap())
                .is_err(),
            "{damage}"
        );
        assert!(Repository::init(root.path()).is_err(), "{damage}");
    }
}

#[test]
fn format_three_refusal_does_not_rewrite_existing_store() {
    let root = TempDir::new().unwrap();
    drop(Repository::init(root.path()).unwrap());
    let raw = db(&root);
    raw.execute_batch("PRAGMA user_version=3;").unwrap();
    drop(raw);
    let path = root.path().join(".javelin/repository.sqlite3");
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        Repository::open(root.path()),
        Err(Error::UnsupportedFormat(3))
    ));
    assert!(matches!(
        Repository::init(root.path()),
        Err(Error::UnsupportedFormat(3))
    ));
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn ancestry_is_canonical_and_hash_bound_even_for_unreferenced_checkpoints() {
    use sha2::{Digest, Sha256};
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let head = layer(&mut repo, "target");
    let raw = db(&root);
    let bytes: Vec<u8> = raw
        .query_row(
            "SELECT bytes FROM objects WHERE id=?1",
            [head.checkpoint().object().to_string()],
            |r| r.get(0),
        )
        .unwrap();
    for sources in [
        serde_json::json!([head.checkpoint(), head.checkpoint()]),
        serde_json::json!(["00".repeat(32)]),
        serde_json::json!([repo.put_blob(b"wrong kind").unwrap()]),
    ] {
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["sources"] = sources;
        // Preserve canonical field order in this valid-hash adversarial fixture.
        let bytes = format!(
            "{{\"repository\":{},\"version\":{},\"manifest\":{},\"changes\":{},\"sources\":{}}}",
            value["repository"],
            value["version"],
            value["manifest"],
            value["changes"],
            value["sources"]
        )
        .into_bytes();
        let mut hash = Sha256::new();
        hash.update(b"javelin-object-v1\0");
        hash.update([3]);
        hash.update(&bytes);
        let id: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        raw.execute(
            "INSERT INTO objects VALUES(?1,3,?2)",
            rusqlite::params![id, bytes],
        )
        .unwrap();
        assert!(
            repo.read_checkpoint(CheckpointId::parse(&id).unwrap())
                .is_err()
        );
        assert!(repo.check().is_err());
    }
}

#[test]
fn retry_source_binding_cannot_be_substituted_with_an_already_retained_ancestor() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let a = layer(&mut repo, "a");
    let b = layer(&mut repo, "b");
    let changes = delta(&mut repo, b"a");
    let a = applied(repo.capture_layer(&op("a"), &a, &changes).unwrap());
    let changes_b = delta(&mut repo, b"b");
    let b = applied(repo.capture_layer(&op("b"), &b, &changes_b).unwrap());
    let target = layer(&mut repo, "target");
    let target = applied(
        repo.commit_stack(&op("stack-a"), &target, a.checkpoint(), &changes)
            .unwrap(),
    );
    let target = applied(
        repo.commit_stack(&op("stack-b"), &target, b.checkpoint(), &changes)
            .unwrap(),
    );
    repo.commit_stack(&op("repeat-a"), &target, a.checkpoint(), &changes)
        .unwrap();
    repo.check().unwrap();
    let raw = db(&root);
    raw.execute_batch("DROP TRIGGER stacks_no_UPDATE;").unwrap();
    raw.execute(
        "UPDATE stacks SET source=?1 WHERE id='repeat-a'",
        [b.checkpoint().object().to_string()],
    )
    .unwrap();
    assert!(repo.check().is_err());
    assert!(
        repo.stack_operation(&op("repeat-a"), &target, b.checkpoint())
            .is_err()
    );
}

#[test]
fn stale_stack_observation_cannot_be_changed_to_a_later_valid_head() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let source = layer(&mut repo, "source");
    let target = layer(&mut repo, "target");
    let changes = delta(&mut repo, b"winner");
    let winner = applied(
        repo.capture_layer(&op("winner"), &target, &changes)
            .unwrap(),
    );
    repo.commit_stack(&op("stale"), &target, source.checkpoint(), &changes)
        .unwrap();
    repo.capture_layer(&op("later"), &winner, &changes).unwrap();
    repo.check().unwrap();
    let raw = db(&root);
    raw.execute_batch("DROP TRIGGER layer_attempts_no_UPDATE;")
        .unwrap();
    raw.execute("UPDATE layer_attempts SET result=2 WHERE id='stale'", [])
        .unwrap();
    assert!(repo.check().is_err());
    assert!(
        repo.stack_operation(&op("stale"), &target, source.checkpoint())
            .is_err()
    );
}

#[test]
fn missing_repeated_source_stack_binding_is_corruption_for_success_and_stale() {
    for stale in [false, true] {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let source = layer(&mut repo, "source");
        let target = layer(&mut repo, "target");
        let changes = SparseChanges::new([]).unwrap();
        let target = applied(
            repo.commit_stack(&op("first"), &target, source.checkpoint(), &changes)
                .unwrap(),
        );
        if stale {
            repo.capture_layer(&op("concurrent"), &target, &changes)
                .unwrap();
        }
        let original = repo
            .commit_stack(&op("repeat"), &target, source.checkpoint(), &changes)
            .unwrap();
        assert_eq!(matches!(original, LayerUpdate::Stale { .. }), stale);
        assert_eq!(
            repo.stack_operation(&op("repeat"), &target, source.checkpoint())
                .unwrap(),
            Some(original)
        );
        repo.check().unwrap();
        let raw = db(&root);
        raw.execute_batch("DROP TRIGGER stacks_no_DELETE;").unwrap();
        raw.execute("DELETE FROM stacks WHERE id='repeat'", [])
            .unwrap();
        assert!(
            matches!(repo.check(), Err(Error::CorruptRepository(_))),
            "missing {stale} Stack binding passed integrity"
        );
        assert!(matches!(
            repo.stack_operation(&op("repeat"), &target, source.checkpoint()),
            Err(Error::CorruptRepository(_))
        ));
        assert!(repo.layer_state(target.layer()).is_err());
        assert!(repo.capture_layer(&op("next"), &target, &changes).is_err());
    }
}
