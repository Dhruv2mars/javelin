use javelin::{
    Error, FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, Manifest, ObjectId, OperationId,
    PublicationReceipt, PublishAttribution, PublishOutcome, PublishWarning, RepoPath, Repository,
    SparseChanges, VersionNumber, WorldVersion,
};
use rusqlite::{Connection, params};
use tempfile::TempDir;

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn path(s: &str) -> RepoPath {
    RepoPath::parse(s).unwrap()
}
fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}
fn regular(repo: &mut Repository, bytes: &[u8]) -> FileEntry {
    FileEntry::Regular {
        content: repo.put_blob(bytes).unwrap(),
        executable: false,
    }
}
fn delta(entries: impl IntoIterator<Item = (&'static str, Option<FileEntry>)>) -> SparseChanges {
    SparseChanges::new(entries.into_iter().map(|(p, e)| (path(p), e))).unwrap()
}
fn active(update: LayerUpdate) -> LayerHead {
    match update {
        LayerUpdate::Applied {
            state: LayerState::Active { head },
        } => head,
        other => panic!("expected applied head: {other:?}"),
    }
}
fn capture(repo: &mut Repository, name: &str, base: u64, changes: &SparseChanges) -> LayerHead {
    let head = repo
        .create_layer(
            &LayerId::parse(name).unwrap(),
            &repo.layer_base(v(base)).unwrap(),
        )
        .unwrap();
    active(
        repo.capture_layer(&op(&format!("capture-{name}")), &head, changes)
            .unwrap(),
    )
}
fn advance(
    repo: &mut Repository,
    name: &str,
    entries: impl IntoIterator<Item = (&'static str, FileEntry)>,
) -> WorldVersion {
    let expected = repo.history().unwrap().last().unwrap().number;
    let manifest = repo
        .put_manifest(&Manifest::new(entries.into_iter().map(|(p, e)| (path(p), e))).unwrap())
        .unwrap();
    match repo
        .compare_and_advance(&op(name), expected, manifest)
        .unwrap()
    {
        javelin::AdvanceOutcome::Accepted { version } => version,
        _ => panic!("expected World advance"),
    }
}
fn publish(repo: &mut Repository, operation: &str, head: &LayerHead) -> PublicationReceipt {
    repo.publish_layer(&op(operation), head, &PublishAttribution::Human, |_, _| {
        Ok(())
    })
    .unwrap()
}
fn published(receipt: &PublicationReceipt) -> WorldVersion {
    match &receipt.outcome {
        PublishOutcome::Published { version } => version.clone(),
        other => panic!("expected publication: {other:?}"),
    }
}
fn raw(root: &TempDir) -> Connection {
    Connection::open(root.path().join(".javelin/repository.sqlite3")).unwrap()
}
fn snapshot(db: &Connection) -> Vec<serde_json::Value> {
    [
        "objects",
        "versions",
        "changes",
        "attempts",
        "layer_heads",
        "layer_attempts",
        "publications",
    ]
    .into_iter()
    .map(|table| {
        let mut q = db
            .prepare(&format!("SELECT * FROM {table} ORDER BY 1,2"))
            .unwrap();
        let columns = q.column_count();
        let rows = q
            .query_map([], |r| {
                Ok((0..columns)
                    .map(|i| {
                        use rusqlite::types::ValueRef;
                        match r.get_ref(i).unwrap() {
                            ValueRef::Null => serde_json::Value::Null,
                            ValueRef::Integer(n) => serde_json::json!(n),
                            ValueRef::Real(n) => serde_json::json!(n),
                            ValueRef::Text(s) => serde_json::json!(String::from_utf8_lossy(s)),
                            ValueRef::Blob(s) => serde_json::json!(s),
                        }
                    })
                    .collect::<Vec<_>>())
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        serde_json::json!([table, rows])
    })
    .collect()
}

#[test]
fn whole_latest_checkpoint_publishes_with_retained_history_clone_and_readonly_state() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let entry = regular(&mut repo, b"final checkpoint");
    let changes = delta([("file", Some(entry.clone()))]);
    let initial = repo
        .create_layer(
            &LayerId::parse("work").unwrap(),
            &repo.layer_base(v(0)).unwrap(),
        )
        .unwrap();
    let accepted = repo
        .capture_layer(&op("capture"), &initial, &changes)
        .unwrap();
    let head = active(accepted.clone());
    let stale = repo
        .capture_layer(&op("stale-capture"), &initial, &delta([]))
        .unwrap();
    let history = repo.layer_history(head.layer()).unwrap();
    let receipt = publish(&mut repo, "publish", &head);
    let world = published(&receipt);
    assert_eq!(receipt.expected, head);
    assert_eq!(receipt.against.number, v(0));
    assert_eq!(world.number, v(1));
    assert_eq!(
        repo.manifest_at(world.number)
            .unwrap()
            .entries()
            .get(&path("file")),
        Some(&entry)
    );
    assert!(receipt.warnings.is_empty());
    assert_eq!(
        repo.layer_state(head.layer()).unwrap(),
        LayerState::Published {
            head: head.clone(),
            operation: op("publish"),
            version: world.clone(),
        }
    );
    let after = repo.layer_history(head.layer()).unwrap();
    assert_eq!(&after[..history.len()], history);
    assert!(matches!(after.last(), Some(LayerState::Published { .. })));
    assert_eq!(
        repo.capture_layer(&op("capture"), &initial, &changes)
            .unwrap(),
        accepted
    );
    assert_eq!(
        repo.capture_layer(&op("stale-capture"), &initial, &delta([]))
            .unwrap(),
        stale
    );
    let before = snapshot(&raw(&root));
    assert!(matches!(
        repo.capture_layer(&op("new-capture"), &head, &delta([])),
        Err(Error::LayerPublished { .. })
    ));
    assert!(matches!(
        repo.discard_layer(&op("new-discard"), &head),
        Err(Error::LayerPublished { .. })
    ));
    assert_eq!(snapshot(&raw(&root)), before);
    let clone = repo
        .clone_layer(&LayerId::parse("clone").unwrap(), head.checkpoint())
        .unwrap();
    assert_eq!(clone.checkpoint(), head.checkpoint());
    assert_eq!(
        repo.read_checkpoint(head.checkpoint())
            .unwrap()
            .base()
            .version(),
        v(0)
    );
    assert_eq!(repo.layer_diff(&head).unwrap()[0].after, Some(entry));
    let terminal = publish(&mut repo, "second-publish", &head);
    assert!(matches!(
        terminal.outcome,
        PublishOutcome::LayerStale {
            actual: LayerState::Published { .. }
        }
    ));
    advance(&mut repo, "later", []);
    assert_eq!(publish(&mut repo, "publish", &head), receipt);
    assert_eq!(publish(&mut repo, "second-publish", &head), terminal);
    assert_eq!(repo.publication(&op("publish")).unwrap(), Some(receipt));
    repo.check().unwrap();
}

#[test]
fn advanced_world_composes_complete_delta_and_preserves_immutable_base() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let keep = regular(&mut repo, b"keep");
    let remove = regular(&mut repo, b"remove");
    advance(
        &mut repo,
        "base",
        [("keep", keep.clone()), ("remove", remove.clone())],
    );
    let executable = FileEntry::Regular {
        content: repo.put_blob(b"program").unwrap(),
        executable: true,
    };
    let link = FileEntry::Symlink {
        target: repo.put_blob(b"../keep").unwrap(),
    };
    let head = capture(
        &mut repo,
        "layer",
        1,
        &delta([
            ("remove", None),
            ("program", Some(executable.clone())),
            ("link", Some(link.clone())),
        ]),
    );
    let checkpoint = repo.read_checkpoint(head.checkpoint()).unwrap();
    let independent = regular(&mut repo, b"World change");
    let locked = advance(
        &mut repo,
        "advance",
        [
            ("keep", keep.clone()),
            ("remove", remove),
            ("world", independent.clone()),
        ],
    );
    let receipt = repo
        .publish_layer(
            &op("publish"),
            &head,
            &PublishAttribution::Human,
            |world, candidate| {
                assert_eq!(*world, locked);
                assert_eq!(
                    candidate.entries(),
                    &std::collections::BTreeMap::from([
                        (path("remove"), None),
                        (path("program"), Some(executable.clone())),
                        (path("link"), Some(link.clone())),
                    ])
                );
                Ok(())
            },
        )
        .unwrap();
    let final_world = published(&receipt);
    assert_eq!(final_world.number, v(3));
    assert_eq!(repo.read_checkpoint(head.checkpoint()).unwrap(), checkpoint);
    assert_eq!(
        repo.manifest_at(final_world.number)
            .unwrap()
            .entries()
            .len(),
        4
    );
    repo.check().unwrap();
}

#[test]
fn net_revert_to_base_permits_publish_but_identical_new_results_conflict() {
    for revert in [false, true] {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let a = regular(&mut repo, b"A");
        let b = regular(&mut repo, b"B");
        advance(&mut repo, "base", [("same", a.clone())]);
        let head = capture(&mut repo, "layer", 1, &delta([("same", Some(b.clone()))]));
        advance(&mut repo, "B", [("same", b)]);
        if revert {
            advance(&mut repo, "A", [("same", a.clone())]);
        }
        let receipt = publish(&mut repo, "publish", &head);
        if revert {
            assert_eq!(
                repo.read_blob(
                    repo.manifest_at(published(&receipt).number)
                        .unwrap()
                        .entries()[&path("same")]
                        .content()
                )
                .unwrap(),
                b"B"
            );
        } else {
            assert_eq!(
                receipt.outcome,
                PublishOutcome::Conflict {
                    paths: vec![path("same")]
                }
            );
            advance(&mut repo, "net-revert", [("same", a)]);
            assert_eq!(publish(&mut repo, "publish", &head), receipt);
            assert!(matches!(
                publish(&mut repo, "fresh", &head).outcome,
                PublishOutcome::Published { .. }
            ));
            assert_eq!(repo.publication(&op("publish")).unwrap(), Some(receipt));
        }
        repo.check().unwrap();
    }
}

#[test]
fn exact_prefix_delete_modify_mode_and_symlink_intersections_are_explicit() {
    for case in [
        "exact",
        "prefix-world",
        "prefix-layer",
        "delete-modify",
        "modify-delete",
        "mode",
        "symlink",
        "kind",
        "structure",
    ] {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let a = regular(&mut repo, b"A");
        let b = regular(&mut repo, b"B");
        let executable = FileEntry::Regular {
            content: a.content(),
            executable: true,
        };
        let link = FileEntry::Symlink {
            target: repo.put_blob(b"target").unwrap(),
        };
        let (base, changes, world, paths) = match case {
            "prefix-world" => (
                vec![],
                delta([("a/file", Some(a.clone()))]),
                vec![("a", b.clone())],
                vec![path("a"), path("a/file")],
            ),
            "prefix-layer" => (
                vec![],
                delta([("a", Some(a.clone()))]),
                vec![("a/file", b.clone())],
                vec![path("a"), path("a/file")],
            ),
            "delete-modify" => (
                vec![("a", a.clone())],
                delta([("a", None)]),
                vec![("a", b.clone())],
                vec![path("a")],
            ),
            "modify-delete" => (
                vec![("a", a.clone())],
                delta([("a", Some(b.clone()))]),
                vec![],
                vec![path("a")],
            ),
            "mode" => (
                vec![("a", a.clone())],
                delta([("a", Some(executable.clone()))]),
                vec![("a", executable)],
                vec![path("a")],
            ),
            "symlink" => (
                vec![("a", link.clone())],
                delta([("a", None)]),
                vec![(
                    "a",
                    FileEntry::Symlink {
                        target: repo.put_blob(b"other").unwrap(),
                    },
                )],
                vec![path("a")],
            ),
            "kind" => (
                vec![("a", a.clone())],
                delta([("a", Some(link.clone()))]),
                vec![("a", b.clone())],
                vec![path("a")],
            ),
            "structure" => (
                vec![("a/file", a.clone())],
                delta([("a/file", None), ("a", Some(b.clone()))]),
                vec![("a/file", b.clone())],
                vec![path("a"), path("a/file")],
            ),
            _ => (
                vec![],
                delta([("a", Some(a.clone()))]),
                vec![("a", b.clone())],
                vec![path("a")],
            ),
        };
        advance(&mut repo, "base", base);
        let head = capture(&mut repo, "layer", 1, &changes);
        advance(&mut repo, "world", world);
        let receipt = publish(&mut repo, "publish", &head);
        assert_eq!(
            receipt.outcome,
            PublishOutcome::Conflict { paths },
            "{case}"
        );
        assert!(matches!(
            repo.layer_state(head.layer()).unwrap(),
            LayerState::Active { .. }
        ));
        repo.check().unwrap();
    }
}

#[test]
fn separate_children_and_name_prefix_without_slash_compose() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let a = regular(&mut repo, b"A");
    let b = regular(&mut repo, b"B");
    let head = capture(
        &mut repo,
        "layer",
        0,
        &delta([("a", Some(a.clone())), ("dir/one", Some(a.clone()))]),
    );
    advance(&mut repo, "world", [("ab", b.clone()), ("dir/two", b)]);
    let receipt = publish(&mut repo, "publish", &head);
    assert_eq!(
        repo.manifest_at(published(&receipt).number)
            .unwrap()
            .entries()
            .len(),
        4
    );
}

#[test]
fn stale_active_and_discarded_receipts_replay_original_observation_after_later_publication() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let a = regular(&mut repo, b"A");
    let b = regular(&mut repo, b"B");
    let head = capture(&mut repo, "layer", 0, &delta([("a", Some(a.clone()))]));
    let next = active(
        repo.capture_layer(&op("next"), &head, &delta([("a", Some(b))]))
            .unwrap(),
    );
    let stale = publish(&mut repo, "stale", &head);
    assert_eq!(
        stale.outcome,
        PublishOutcome::LayerStale {
            actual: LayerState::Active { head: next.clone() }
        }
    );
    publish(&mut repo, "accepted", &next);
    advance(&mut repo, "later", []);
    assert_eq!(publish(&mut repo, "stale", &head), stale);
    let discarded = capture(&mut repo, "discarded", 0, &delta([("b", Some(a))]));
    let terminal = repo.discard_layer(&op("discard"), &discarded).unwrap();
    let receipt = publish(&mut repo, "discard-publish", &discarded);
    let LayerUpdate::Applied { state } = terminal else {
        panic!("expected discard")
    };
    assert_eq!(
        receipt.outcome,
        PublishOutcome::LayerStale { actual: state }
    );
    advance(&mut repo, "later-again", []);
    assert_eq!(publish(&mut repo, "discard-publish", &discarded), receipt);
    repo.check().unwrap();
}

#[test]
fn operation_reuse_binds_complete_input_and_raw_world_namespace_in_both_directions() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let entry = regular(&mut repo, b"A");
    let head = capture(&mut repo, "layer", 0, &delta([("a", Some(entry.clone()))]));
    let receipt = publish(&mut repo, "success", &head);
    let world = published(&receipt);
    let manifest = repo
        .put_manifest(&repo.manifest_at(world.number).unwrap())
        .unwrap();
    assert!(matches!(
        repo.compare_and_advance(&op("success"), receipt.against.number, manifest),
        Err(Error::OperationIdReused(_))
    ));
    let other = capture(&mut repo, "other", 0, &delta([("a", Some(entry))]));
    let conflict = publish(&mut repo, "conflict", &other);
    assert!(matches!(conflict.outcome, PublishOutcome::Conflict { .. }));
    let stale = publish(&mut repo, "terminal", &head);
    assert!(matches!(stale.outcome, PublishOutcome::LayerStale { .. }));
    for name in ["conflict", "terminal"] {
        assert!(matches!(
            repo.compare_and_advance(&op(name), world.number, manifest),
            Err(Error::OperationIdReused(_))
        ));
    }
    repo.compare_and_advance(&op("raw-success"), world.number, manifest)
        .unwrap();
    repo.compare_and_advance(&op("raw-stale"), v(0), manifest)
        .unwrap();
    for name in ["raw-success", "raw-stale"] {
        assert!(matches!(
            repo.publish_layer(
                &op(name),
                &other,
                &PublishAttribution::Human,
                |_, _| panic!("must not guard")
            ),
            Err(Error::OperationIdReused(_))
        ));
    }
    let changed_inputs = [
        PublishAttribution::Agent {
            context: None,
            allow_missing_trace: false,
        },
        PublishAttribution::Agent {
            context: None,
            allow_missing_trace: true,
        },
        PublishAttribution::Agent {
            context: Some(ObjectId::for_blob(b"missing")),
            allow_missing_trace: true,
        },
    ];
    for attribution in changed_inputs {
        assert!(matches!(
            repo.publish_layer(&op("success"), &head, &attribution, |_, _| panic!(
                "must not guard"
            )),
            Err(Error::OperationIdReused(_))
        ));
    }
    assert!(matches!(
        repo.publish_layer(
            &op("success"),
            &other,
            &PublishAttribution::Human,
            |_, _| panic!("must not guard")
        ),
        Err(Error::OperationIdReused(_))
    ));
    // Layer capture operation IDs retain their existing separate scope.
    let empty = SparseChanges::new([]).unwrap();
    let independent = repo
        .create_layer(
            &LayerId::parse("scope").unwrap(),
            &repo.layer_base(v(0)).unwrap(),
        )
        .unwrap();
    repo.capture_layer(&op("success"), &independent, &empty)
        .unwrap();
    repo.check().unwrap();
}

#[test]
fn human_agent_context_override_and_nonreservation_are_enforced() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let head = capture(&mut repo, "layer", 0, &delta([]));
    let before = snapshot(&raw(&root));
    assert!(matches!(
        repo.publish_layer(
            &op("retry"),
            &head,
            &PublishAttribution::Agent {
                context: None,
                allow_missing_trace: false
            },
            |_, _| panic!("must refuse first")
        ),
        Err(Error::MissingAgentTrace)
    ));
    assert_eq!(snapshot(&raw(&root)), before);
    let receipt = repo
        .publish_layer(
            &op("retry"),
            &head,
            &PublishAttribution::Agent {
                context: None,
                allow_missing_trace: true,
            },
            |_, _| Ok(()),
        )
        .unwrap();
    assert_eq!(receipt.warnings, vec![PublishWarning::MissingAgentTrace]);
    assert_eq!(
        serde_json::to_value(&receipt).unwrap()["warnings"],
        serde_json::json!(["missing_agent_trace"])
    );
    let head = capture(&mut repo, "traced", 1, &delta([]));
    let trace = repo.put_blob(b"opaque trace").unwrap();
    let attribution = PublishAttribution::Agent {
        context: Some(trace),
        allow_missing_trace: false,
    };
    let receipt = repo
        .publish_layer(&op("traced"), &head, &attribution, |_, _| Ok(()))
        .unwrap();
    assert!(receipt.warnings.is_empty());
    let before = snapshot(&raw(&root));
    assert!(matches!(
        repo.publish_layer(
            &op("traced"),
            &head,
            &PublishAttribution::Agent {
                context: Some(trace),
                allow_missing_trace: true
            },
            |_, _| panic!("no replay guard")
        ),
        Err(Error::OperationIdReused(_))
    ));
    assert_eq!(snapshot(&raw(&root)), before);
    let missing = ObjectId::for_blob(b"absent");
    for context in [
        missing,
        repo.put_manifest(&Manifest::new([]).unwrap()).unwrap(),
        head.checkpoint().object(),
    ] {
        let before = snapshot(&raw(&root));
        let error = repo
            .publish_layer(
                &op("bad-context"),
                &head,
                &PublishAttribution::Agent {
                    context: Some(context),
                    allow_missing_trace: true,
                },
                |_, _| panic!("invalid trace"),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            Error::MissingObject(_) | Error::WrongObjectKind(_)
        ));
        assert_eq!(snapshot(&raw(&root)), before);
    }
    raw(&root)
        .execute_batch("DROP TRIGGER objects_no_UPDATE;")
        .unwrap();
    raw(&root)
        .execute(
            "UPDATE objects SET bytes=x'00' WHERE id=?1",
            [trace.to_string()],
        )
        .unwrap();
    assert!(matches!(repo.publication(&op("traced")),Err(Error::CorruptObject(id)) if id==trace));
    assert!(repo.check().is_err());
}

#[test]
fn guard_holds_write_lock_and_refusal_changes_no_rows_or_objects() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let a = regular(&mut repo, b"A");
    let b = regular(&mut repo, b"B");
    let head = capture(&mut repo, "layer", 0, &delta([("layer", Some(a.clone()))]));
    let world = advance(&mut repo, "world", [("world", b.clone())]);
    let db = raw(&root);
    db.busy_timeout(std::time::Duration::ZERO).unwrap();
    let before = snapshot(&db);
    let error = repo
        .publish_layer(
            &op("retry"),
            &head,
            &PublishAttribution::Human,
            |locked, candidate| {
                assert_eq!(*locked, world);
                assert_eq!(
                    candidate.entries(),
                    &std::collections::BTreeMap::from([(path("layer"), Some(a))])
                );
                let error = db.execute_batch("BEGIN IMMEDIATE;").unwrap_err();
                assert_eq!(
                    error.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy)
                );
                assert_eq!(snapshot(&db), before);
                Err(Error::InvalidInput("guard refused".into()))
            },
        )
        .unwrap_err();
    assert!(matches!(error,Error::InvalidInput(s) if s=="guard refused"));
    assert_eq!(snapshot(&db), before);
    assert!(repo.publication(&op("retry")).unwrap().is_none());
    publish(&mut repo, "retry", &head);
    repo.publish_layer(&op("retry"), &head, &PublishAttribution::Human, |_, _| {
        panic!("guard reran on replay")
    })
    .unwrap();
}

#[test]
fn foreign_head_is_a_typed_no_write_refusal() {
    let root = TempDir::new().unwrap();
    let foreign = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let mut other = Repository::init(foreign.path()).unwrap();
    let head = capture(&mut other, "foreign", 0, &delta([]));
    let before = snapshot(&raw(&root));
    assert!(matches!(
        repo.publish_layer(
            &op("foreign"),
            &head,
            &PublishAttribution::Human,
            |_, _| panic!("foreign guard")
        ),
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(snapshot(&raw(&root)), before);
}

#[test]
fn failed_storage_commit_rolls_back_all_publication_state_and_allows_retry() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let a = regular(&mut repo, b"A");
    let head = capture(&mut repo, "layer", 0, &delta([("a", Some(a))]));
    let db = raw(&root);
    let before = snapshot(&db);
    db.execute_batch("CREATE TRIGGER refuse_publication BEFORE INSERT ON publications BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
    assert!(matches!(
        repo.publish_layer(&op("retry"), &head, &PublishAttribution::Human, |_, _| Ok(
            ()
        )),
        Err(Error::Storage(_))
    ));
    assert_eq!(snapshot(&db), before);
    db.execute_batch("DROP TRIGGER refuse_publication;")
        .unwrap();
    published(&publish(&mut repo, "retry", &head));
}

#[test]
fn format_two_refusal_preserves_closed_store_bytes() {
    let root = TempDir::new().unwrap();
    let repo = Repository::init(root.path()).unwrap();
    drop(repo);
    let db = raw(&root);
    db.execute_batch("PRAGMA user_version=2;").unwrap();
    drop(db);
    let directory = root.path().join(".javelin");
    let before = std::fs::read(directory.join("repository.sqlite3")).unwrap();
    for result in [Repository::open(root.path()), Repository::init(root.path())] {
        assert!(matches!(result, Err(Error::UnsupportedFormat(2))));
    }
    assert_eq!(
        std::fs::read(directory.join("repository.sqlite3")).unwrap(),
        before
    );
    assert_eq!(std::fs::read_dir(directory).unwrap().count(), 1);
}

#[test]
fn publication_constraints_and_immutability_are_preserved() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let head = capture(&mut repo, "layer", 0, &delta([]));
    publish(&mut repo, "pub", &head);
    let db = raw(&root);
    for table in [
        "objects",
        "versions",
        "attempts",
        "layer_heads",
        "layer_attempts",
        "publications",
    ] {
        assert!(
            db.execute(&format!("DELETE FROM {table}"), []).is_err(),
            "{table}"
        );
        assert!(
            db.execute(&format!("UPDATE {table} SET rowid=rowid"), [])
                .is_err(),
            "{table}"
        );
    }
    assert!(db.execute("INSERT INTO publications SELECT 'duplicate',layer,expected,checkpoint,against,observed,observed_publication,attribution,context,allow_missing_trace,outcome,result,'duplicate',receipt FROM publications",[]).is_err());
    repo.check().unwrap();
}

#[test]
fn tampered_receipt_relations_composition_and_accepted_graph_never_fall_back() {
    for damage in [
        "receipt-hash",
        "receipt-canonical",
        "receipt-semantic",
        "checkpoint",
        "against",
        "observed",
        "outcome",
        "context-kind",
        "world-changeset",
        "old-blob",
        "world-tail",
        "publication-tail-event",
    ] {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let a = regular(&mut repo, b"A");
        let b = regular(&mut repo, b"B");
        advance(&mut repo, "base", [("base", a.clone())]);
        let head = capture(&mut repo, "layer", 1, &delta([("file", Some(b.clone()))]));
        let receipt = publish(&mut repo, "pub", &head);
        let alternate = repo
            .put_manifest(&Manifest::new([(path("other"), b)]).unwrap())
            .unwrap();
        let db = raw(&root);
        db.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA ignore_check_constraints=ON; DROP TRIGGER objects_no_UPDATE; DROP TRIGGER publications_no_UPDATE; DROP TRIGGER versions_no_UPDATE; DROP TRIGGER versions_no_DELETE; DROP TRIGGER attempts_no_UPDATE;").unwrap();
        match damage {
            "receipt-hash" => {
                db.execute("UPDATE objects SET bytes=x'00' WHERE id=(SELECT receipt FROM publications WHERE id='pub')",[]).unwrap();
            }
            "receipt-canonical" | "receipt-semantic" => {
                let mut bytes = serde_json::to_vec(&receipt).unwrap();
                if damage == "receipt-canonical" {
                    bytes.push(b' ');
                } else {
                    let mut value = serde_json::to_value(&receipt).unwrap();
                    value["warnings"] = serde_json::json!(["missing_agent_trace"]);
                    bytes = serde_json::to_vec(&value).unwrap();
                }
                let id = ObjectId::for_blob(&bytes);
                db.execute(
                    "INSERT INTO objects(id,kind,bytes) VALUES(?1,1,?2)",
                    params![id.to_string(), bytes],
                )
                .unwrap();
                db.execute(
                    "UPDATE publications SET receipt=?1 WHERE id='pub'",
                    [id.to_string()],
                )
                .unwrap();
            }
            "checkpoint" => {
                db.execute("UPDATE publications SET checkpoint=(SELECT checkpoint FROM layer_heads WHERE revision=0) WHERE id='pub'",[]).unwrap();
            }
            "against" => {
                db.execute("UPDATE publications SET against=0 WHERE id='pub'", [])
                    .unwrap();
            }
            "observed" => {
                db.execute("UPDATE publications SET observed=0 WHERE id='pub'", [])
                    .unwrap();
            }
            "outcome" => {
                db.execute("UPDATE publications SET outcome=2,result=NULL,world_operation=NULL WHERE id='pub'",[]).unwrap();
            }
            "context-kind" => {
                db.execute(
                    "UPDATE publications SET attribution=1,context=?1 WHERE id='pub'",
                    [alternate.to_string()],
                )
                .unwrap();
            }
            "world-changeset" => {
                db.execute_batch("DROP TRIGGER changes_no_UPDATE;").unwrap();
                db.execute(
                    "UPDATE changes SET content=?1 WHERE version=2",
                    [a.content().to_string()],
                )
                .unwrap();
            }
            "old-blob" => {
                db.execute(
                    "UPDATE objects SET bytes=x'00' WHERE id=?1",
                    [a.content().to_string()],
                )
                .unwrap();
            }
            "world-tail" => {
                db.execute("DELETE FROM versions WHERE number=2", [])
                    .unwrap();
            }
            "publication-tail-event" => {
                db.execute("INSERT INTO layer_attempts(id,layer,expected,checkpoint,discard,accepted,result) VALUES('illicit','layer',1,?1,1,1,2)",[head.checkpoint().object().to_string()]).unwrap();
                db.execute("INSERT INTO layer_heads(layer,revision,checkpoint,discarded,operation) VALUES('layer',2,?1,1,'illicit')",[head.checkpoint().object().to_string()]).unwrap();
            }
            _ => unreachable!(),
        }
        let before = snapshot(&db);
        assert!(repo.history().is_err(), "{damage}");
        assert!(repo.check().is_err(), "{damage}");
        assert!(Repository::init(root.path()).is_err(), "{damage}");
        // Commands verify the receipt they read, not unrelated World content: a
        // damaged base blob outside the Layer's change is check()'s to find, as
        // is a success relabelled as failed, which no longer marks the Layer.
        if damage != "old-blob" {
            assert!(repo.publication(&op("pub")).is_err(), "{damage}");
        }
        if !["old-blob", "outcome"].contains(&damage) {
            assert!(repo.layer_state(head.layer()).is_err(), "{damage}");
            assert!(
                repo.publish_layer(
                    &op("fresh"),
                    &head,
                    &PublishAttribution::Human,
                    |_, _| panic!("corrupt guard")
                )
                .is_err(),
                "{damage}"
            );
        }
        assert_eq!(snapshot(&db), before, "{damage}");
    }
}

#[test]
fn supplied_corrupt_context_is_never_a_missing_trace_override() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let head = capture(&mut repo, "layer", 0, &delta([]));
    let trace = repo.put_blob(b"unused context").unwrap();
    let db = raw(&root);
    db.execute_batch("DROP TRIGGER objects_no_UPDATE;").unwrap();
    db.execute(
        "UPDATE objects SET bytes=x'00' WHERE id=?1",
        [trace.to_string()],
    )
    .unwrap();
    let before = snapshot(&db);
    assert!(
        matches!(repo.publish_layer(&op("trace"),&head,&PublishAttribution::Agent {context:Some(trace),allow_missing_trace:true},|_,_|panic!("bad trace reached guard")),Err(Error::CorruptObject(id)) if id==trace)
    );
    assert_eq!(snapshot(&db), before);
    assert!(repo.publication(&op("trace")).unwrap().is_none());
    published(&publish(&mut repo, "trace", &head));
}

#[test]
fn failed_receipt_paths_and_observed_publication_are_bound_to_original_records() {
    for damage in [
        "conflict-paths",
        "conflict-against",
        "stale-event",
        "stale-publication",
    ] {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let a = regular(&mut repo, b"A");
        let head = capture(&mut repo, "layer", 0, &delta([("a", Some(a.clone()))]));
        let receipt = if damage.starts_with("conflict") {
            advance(&mut repo, "world", [("a", a)]);
            publish(&mut repo, "failed", &head)
        } else if damage == "stale-event" {
            repo.capture_layer(&op("later-capture"), &head, &delta([]))
                .unwrap();
            publish(&mut repo, "failed", &head)
        } else {
            publish(&mut repo, "success", &head);
            publish(&mut repo, "failed", &head)
        };
        let db = raw(&root);
        db.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA ignore_check_constraints=ON; DROP TRIGGER publications_no_UPDATE;").unwrap();
        match damage {
            "conflict-paths" => {
                let mut value = serde_json::to_value(&receipt).unwrap();
                value["outcome"]["paths"] = serde_json::json!(["unrelated"]);
                let bytes = serde_json::to_vec(&value).unwrap();
                let id = ObjectId::for_blob(&bytes);
                db.execute(
                    "INSERT INTO objects(id,kind,bytes) VALUES(?1,1,?2)",
                    params![id.to_string(), bytes],
                )
                .unwrap();
                db.execute(
                    "UPDATE publications SET receipt=?1 WHERE id='failed'",
                    [id.to_string()],
                )
                .unwrap();
            }
            "conflict-against" => {
                db.execute("UPDATE publications SET against=0 WHERE id='failed'", [])
                    .unwrap();
            }
            "stale-event" => {
                db.execute("UPDATE publications SET observed=1 WHERE id='failed'", [])
                    .unwrap();
            }
            "stale-publication" => {
                db.execute(
                    "UPDATE publications SET observed_publication=NULL WHERE id='failed'",
                    [],
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(repo.publication(&op("failed")).is_err(), "{damage}");
        assert!(repo.check().is_err(), "{damage}");
        assert!(repo.history().is_err(), "{damage}");
        // A fresh Publish does not read an unrelated failed receipt.
    }
}
