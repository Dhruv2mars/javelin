use javelin::{
    CheckpointId, Error, FileEntry, LayerBase, LayerHead, LayerId, LayerState, LayerUpdate,
    Manifest, OperationId, RepoPath, Repository, SparseChanges, VersionNumber,
};
use rusqlite::{Connection, params};
use tempfile::TempDir;

fn path(s: &str) -> RepoPath {
    RepoPath::parse(s).unwrap()
}
fn id(s: &str) -> LayerId {
    LayerId::parse(s).unwrap()
}
fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn version(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}
fn regular(repo: &mut Repository, bytes: &[u8]) -> FileEntry {
    FileEntry::Regular {
        content: repo.put_blob(bytes).unwrap(),
        executable: false,
    }
}
fn changes(entries: impl IntoIterator<Item = (&'static str, Option<FileEntry>)>) -> SparseChanges {
    SparseChanges::new(entries.into_iter().map(|(p, e)| (path(p), e))).unwrap()
}
fn applied(outcome: LayerUpdate) -> LayerHead {
    match outcome {
        LayerUpdate::Applied {
            state: LayerState::Active { head },
        } => head,
        other => panic!("expected applied active head, got {other:?}"),
    }
}
fn raw(root: &TempDir) -> Connection {
    Connection::open(root.path().join(".javelin/repository.sqlite3")).unwrap()
}
fn usage(db: &Connection) -> (i64, i64) {
    db.query_row("SELECT count(*),sum(length(bytes)) FROM objects", [], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })
    .unwrap()
}
fn fixture() -> (TempDir, Repository, FileEntry, FileEntry) {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let large = regular(&mut repo, &vec![42; 1024 * 1024]);
    let removed = regular(&mut repo, b"remove me");
    let manifest = Manifest::new([
        (path("keep"), large.clone()),
        (path("remove"), removed.clone()),
    ])
    .unwrap();
    let manifest = repo.put_manifest(&manifest).unwrap();
    repo.compare_and_advance(&op("world"), version(0), manifest)
        .unwrap();
    (root, repo, large, removed)
}

fn small_fixture() -> (TempDir, Repository, FileEntry) {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let entry = regular(&mut repo, b"tracked");
    let manifest = repo
        .put_manifest(&Manifest::new([(path("tracked"), entry.clone())]).unwrap())
        .unwrap();
    repo.compare_and_advance(&op("world"), version(0), manifest)
        .unwrap();
    (root, repo, entry)
}

fn insert_checkpoint_bytes(db: &Connection, bytes: &[u8]) -> CheckpointId {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"javelin-object-v1\0");
    hash.update([3]);
    hash.update(bytes);
    let encoded: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    db.execute(
        "INSERT INTO objects(id,kind,bytes) VALUES(?1,3,?2) ON CONFLICT(id) DO NOTHING",
        params![encoded, bytes],
    )
    .unwrap();
    CheckpointId::parse(&encoded).unwrap()
}

// External byte fixture: field order deliberately matches the format, independent
// of serde_json::Value's alphabetical object ordering.
fn ordered_checkpoint_bytes(base: &LayerBase, delta: &SparseChanges) -> Vec<u8> {
    #[derive(serde::Serialize)]
    struct Fixture<'a> {
        repository: String,
        version: u64,
        world: String,
        changes: &'a std::collections::BTreeMap<RepoPath, Option<FileEntry>>,
    }
    serde_json::to_vec(&Fixture {
        repository: serde_json::to_value(base).unwrap()["repository"]
            .as_str()
            .unwrap()
            .into(),
        version: base.version().get(),
        world: base.world().to_string(),
        changes: delta.entries(),
    })
    .unwrap()
}

fn assert_ordered_fixture_matches_store(repo: &mut Repository, db: &Connection, base: &LayerBase) {
    let head = repo.create_layer(&id("fixture-control"), base).unwrap();
    let actual: Vec<u8> = db
        .query_row(
            "SELECT bytes FROM objects WHERE id=?1",
            [head.checkpoint().object().to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ordered_checkpoint_bytes(base, &changes([])), actual);
    let entry = repo
        .manifest_at(base.version())
        .unwrap()
        .entries()
        .values()
        .next()
        .unwrap()
        .clone();
    let delta = changes([("fixture-new", Some(entry))]);
    let next = applied(
        repo.capture_layer(&op("fixture-order"), &head, &delta)
            .unwrap(),
    );
    let actual: Vec<u8> = db
        .query_row(
            "SELECT bytes FROM objects WHERE id=?1",
            [next.checkpoint().object().to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ordered_checkpoint_bytes(base, &delta), actual);
}

#[test]
fn canonical_encoding_rejects_redundant_unchanged_delta() {
    let (root, mut repo, entry) = small_fixture();
    let base = repo.layer_base(version(1)).unwrap();
    let db = raw(&root);
    assert_ordered_fixture_matches_store(&mut repo, &db, &base);
    let checkpoint = insert_checkpoint_bytes(
        &db,
        &ordered_checkpoint_bytes(&base, &changes([("tracked", Some(entry))])),
    );
    assert!(
        matches!(repo.read_checkpoint(checkpoint),Err(Error::CorruptObject(id)) if id==checkpoint.object())
    );
}

#[test]
fn canonical_encoding_rejects_nonexistent_exact_path_tombstone() {
    let (root, mut repo, _) = small_fixture();
    let base = repo.layer_base(version(1)).unwrap();
    let db = raw(&root);
    assert_ordered_fixture_matches_store(&mut repo, &db, &base);
    let checkpoint = insert_checkpoint_bytes(
        &db,
        &ordered_checkpoint_bytes(&base, &changes([("missing", None)])),
    );
    assert!(
        matches!(repo.read_checkpoint(checkpoint),Err(Error::CorruptObject(id)) if id==checkpoint.object())
    );
}

#[test]
fn normalized_checkpoint_rejects_only_noncanonical_byte_encoding() {
    let (root, mut repo, _) = small_fixture();
    let base = repo.layer_base(version(1)).unwrap();
    let db = raw(&root);
    assert_ordered_fixture_matches_store(&mut repo, &db, &base);
    let delta = changes([]);
    let canonical = ordered_checkpoint_bytes(&base, &delta);
    let canonical_id = insert_checkpoint_bytes(&db, &canonical);
    assert_eq!(
        repo.read_checkpoint(canonical_id).unwrap().changes(),
        &delta
    );
    let decoded: serde_json::Value = serde_json::from_slice(&canonical).unwrap();
    let reordered = serde_json::to_vec(&decoded).unwrap();
    assert_ne!(canonical, reordered);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&reordered).unwrap(),
        decoded
    );
    let mut whitespace = canonical.clone();
    whitespace.push(b'\n');
    for bytes in [reordered, whitespace] {
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            decoded
        );
        let checkpoint = insert_checkpoint_bytes(&db, &bytes);
        assert!(
            matches!(repo.read_checkpoint(checkpoint),Err(Error::CorruptObject(id)) if id==checkpoint.object())
        );
    }
}

/// Commands read only their own rows; binding damage is caught by the Layer's
/// history read, `check()` and reopening through `init`.
fn assert_binding_damage_rejected(root: &TempDir, repo: &mut Repository, head: &LayerHead) {
    assert!(matches!(
        repo.layer_history(head.layer()),
        Err(Error::CorruptRepository(_))
    ));
    assert!(matches!(repo.check(), Err(Error::CorruptRepository(_))));
    assert!(matches!(
        Repository::init(root.path()),
        Err(Error::CorruptRepository(_))
    ));
}

#[test]
fn valid_stale_candidate_from_another_world_cannot_change_attempt_base_binding() {
    let (root, mut repo, entry) = small_fixture();
    let initial = repo
        .create_layer(&id("base-zero"), &repo.layer_base(version(0)).unwrap())
        .unwrap();
    let other = repo
        .create_layer(&id("base-one"), &repo.layer_base(version(1)).unwrap())
        .unwrap();
    let head = applied(
        repo.capture_layer(&op("accepted"), &initial, &changes([("new", Some(entry))]))
            .unwrap(),
    );
    repo.capture_layer(&op("stale"), &initial, &changes([]))
        .unwrap();
    let foreign_base = repo.read_checkpoint(other.checkpoint()).unwrap();
    let db = raw(&root);
    db.execute_batch("DROP TRIGGER layer_attempts_no_UPDATE;")
        .unwrap();
    db.execute(
        "UPDATE layer_attempts SET checkpoint=?1 WHERE id='stale'",
        [other.checkpoint().object().to_string()],
    )
    .unwrap();
    assert_eq!(
        repo.read_checkpoint(other.checkpoint()).unwrap(),
        foreign_base
    );
    assert_binding_damage_rejected(&root, &mut repo, &head);
}

#[test]
fn valid_same_base_checkpoint_cannot_replace_stale_discard_candidate() {
    let (root, mut repo, entry) = small_fixture();
    let initial = repo
        .create_layer(
            &id("discard-binding"),
            &repo.layer_base(version(1)).unwrap(),
        )
        .unwrap();
    let head = applied(
        repo.capture_layer(&op("accepted"), &initial, &changes([("new", Some(entry))]))
            .unwrap(),
    );
    repo.discard_layer(&op("stale-discard"), &initial).unwrap();
    assert_eq!(
        repo.read_checkpoint(initial.checkpoint()).unwrap().base(),
        repo.read_checkpoint(head.checkpoint()).unwrap().base()
    );
    assert_ne!(initial.checkpoint(), head.checkpoint());
    let db = raw(&root);
    db.execute_batch("DROP TRIGGER layer_attempts_no_UPDATE;")
        .unwrap();
    db.execute(
        "UPDATE layer_attempts SET checkpoint=?1 WHERE id='stale-discard'",
        [head.checkpoint().object().to_string()],
    )
    .unwrap();
    repo.read_checkpoint(head.checkpoint()).unwrap();
    assert_binding_damage_rejected(&root, &mut repo, &head);
}

#[test]
fn valid_head_and_accepted_candidate_cannot_switch_layer_world_base() {
    let (root, mut repo, entry) = small_fixture();
    let initial = repo
        .create_layer(&id("base-zero"), &repo.layer_base(version(0)).unwrap())
        .unwrap();
    let other = repo
        .create_layer(&id("base-one"), &repo.layer_base(version(1)).unwrap())
        .unwrap();
    let head = applied(
        repo.capture_layer(&op("accepted"), &initial, &changes([("new", Some(entry))]))
            .unwrap(),
    );
    let db = raw(&root);
    db.execute_batch("DROP TRIGGER layer_heads_no_UPDATE; DROP TRIGGER layer_attempts_no_UPDATE;")
        .unwrap();
    db.execute(
        "UPDATE layer_heads SET checkpoint=?1 WHERE layer='base-zero' AND revision=1",
        [other.checkpoint().object().to_string()],
    )
    .unwrap();
    db.execute(
        "UPDATE layer_attempts SET checkpoint=?1 WHERE id='accepted'",
        [other.checkpoint().object().to_string()],
    )
    .unwrap();
    repo.read_checkpoint(other.checkpoint()).unwrap();
    assert!(repo.history().is_err());
    assert_binding_damage_rejected(&root, &mut repo, &head);
}

#[test]
fn stale_only_candidate_can_seed_an_independent_recovery_layer() {
    let (root, mut repo, entry) = small_fixture();
    let base = repo.layer_base(version(1)).unwrap();
    let initial = repo.create_layer(&id("original"), &base).unwrap();
    let head = applied(
        repo.capture_layer(
            &op("winner"),
            &initial,
            &changes([("accepted-private", Some(entry))]),
        )
        .unwrap(),
    );
    let private = regular(&mut repo, b"stale-only-private");
    assert!(matches!(
        repo.capture_layer(
            &op("stale-only"),
            &initial,
            &changes([("recovered", Some(private.clone()))])
        )
        .unwrap(),
        LayerUpdate::Stale { .. }
    ));
    let db = raw(&root);
    let encoded: String = db
        .query_row(
            "SELECT checkpoint FROM layer_attempts WHERE id='stale-only'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM layer_heads WHERE checkpoint=?1",
            [&encoded],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    let checkpoint = CheckpointId::parse(&encoded).unwrap();
    let before = usage(&db);
    drop(repo);
    let mut repo = Repository::open(root.path()).unwrap();
    let recovered = repo.clone_layer(&id("recovery"), checkpoint).unwrap();
    assert_eq!(usage(&db), before);
    assert_eq!(repo.layer_diff(&recovered).unwrap()[0].after, Some(private));
    assert_eq!(repo.read_checkpoint(checkpoint).unwrap().base(), &base);
    assert_eq!(
        repo.layer_state(head.layer()).unwrap(),
        LayerState::Active { head }
    );
    assert_eq!(repo.history().unwrap().len(), 2);
    repo.check().unwrap();
}

#[test]
fn directory_prefix_tombstones_do_not_substitute_for_exact_file_deletions() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let entry = regular(&mut repo, b"child");
    let target = repo.put_blob(b"../file").unwrap();
    let manifest = repo
        .put_manifest(
            &Manifest::new([
                (path("dir/file"), entry.clone()),
                (path("dir/nested/link"), FileEntry::Symlink { target }),
            ])
            .unwrap(),
        )
        .unwrap();
    repo.compare_and_advance(&op("world"), version(0), manifest)
        .unwrap();
    let head = repo
        .create_layer(&id("exact-deletes"), &repo.layer_base(version(1)).unwrap())
        .unwrap();
    for delta in [
        changes([("dir", None)]),
        changes([("dir", None), ("dir/file", None), ("dir/nested/link", None)]),
        changes([
            ("private-dir", None),
            ("private-dir/new", Some(entry.clone())),
        ]),
    ] {
        assert!(matches!(
            repo.capture_layer(&op("bad-prefix"), &head, &delta),
            Err(Error::InvalidInput(_))
        ));
        assert_eq!(
            repo.layer_state(head.layer()).unwrap(),
            LayerState::Active { head: head.clone() }
        );
    }
    let removed = applied(
        repo.capture_layer(
            &op("bad-prefix"),
            &head,
            &changes([("dir/file", None), ("dir/nested/link", None)]),
        )
        .unwrap(),
    );
    assert!(
        repo.checkpoint_manifest(removed.checkpoint())
            .unwrap()
            .entries()
            .is_empty()
    );
    assert_eq!(repo.layer_diff(&removed).unwrap().len(), 2);
    assert_eq!(repo.history().unwrap().len(), 2);
    repo.check().unwrap();
}

#[test]
fn opaque_layer_ids_do_not_become_host_paths_or_case_folded_identities() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let base = repo.layer_base(version(0)).unwrap();
    for name in [".", "..", "CON", "con", "A", "a"] {
        let layer = id(name);
        let head = repo.create_layer(&layer, &base).unwrap();
        assert_eq!(head.layer().as_str(), name);
        assert_eq!(
            repo.layer_state(&layer).unwrap(),
            LayerState::Active { head }
        );
    }
    let entries: Vec<_> = std::fs::read_dir(root.path())
        .unwrap()
        .map(|p| p.unwrap().file_name())
        .collect();
    assert_eq!(entries, [".javelin"]);
    assert_eq!(repo.check().unwrap().layers, 6);
}

#[test]
fn sparse_capture_clone_diff_and_discard_share_objects_and_preserve_worlds() {
    let (root, mut repo, large, removed) = fixture();
    let world = repo.history().unwrap();
    let base = repo.layer_base(version(1)).unwrap();
    let initial = repo.create_layer(&id("human-and-agent"), &base).unwrap();
    let db = raw(&root);
    let before = usage(&db);
    let added = regular(&mut repo, &[0, 255, 17]);
    let head = applied(
        repo.capture_layer(
            &op("capture"),
            &initial,
            &changes([
                ("new", Some(added.clone())),
                ("remove", None),
                ("keep", Some(large.clone())),
                ("absent", None),
            ]),
        )
        .unwrap(),
    );
    let frozen = repo.read_checkpoint(head.checkpoint()).unwrap();
    assert_eq!(frozen.base(), &base);
    assert_eq!(frozen.changes().entries().len(), 2);
    let snapshot = repo.checkpoint_manifest(frozen.id()).unwrap();
    assert_eq!(snapshot.entries().get(&path("keep")), Some(&large));
    assert!(!snapshot.entries().contains_key(&path("remove")));
    let captured = usage(&db);
    assert_eq!(captured.0 - before.0, 2); // one new blob and one sparse checkpoint
    assert!(captured.1 - before.1 < 1024);
    let checkpoint_bytes: i64 = db
        .query_row(
            "SELECT length(bytes) FROM objects WHERE id=?1",
            [head.checkpoint().object().to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(checkpoint_bytes < 1024);
    let siblings: Vec<_> = (0..24)
        .map(|i| {
            repo.clone_layer(&id(&format!("child-{i}")), head.checkpoint())
                .unwrap()
        })
        .collect();
    assert_eq!(usage(&db), captured);
    let clone_metadata: (i64, i64) = db.query_row(
        "SELECT count(*),sum(length(layer)+length(checkpoint)) FROM layer_heads WHERE layer LIKE 'child-%'",
        [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(clone_metadata.0, 24);
    assert!(clone_metadata.1 <= 24 * 80); // logical ID/reference bytes, excluding SQLite row overhead
    assert_eq!(
        db.query_row("SELECT count(*) FROM layer_attempts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(siblings.iter().all(|h| h.checkpoint() == head.checkpoint()));
    let diff = repo.layer_diff(&siblings[0]).unwrap();
    assert_eq!(diff.len(), 2);
    assert_eq!(
        (&diff[0].path, &diff[0].before, &diff[0].after),
        (&path("new"), &None, &Some(added))
    );
    assert_eq!(
        (&diff[1].path, &diff[1].before, &diff[1].after),
        (&path("remove"), &Some(removed), &None)
    );
    let discard = repo.discard_layer(&op("discard"), &head).unwrap();
    assert!(matches!(
        discard,
        LayerUpdate::Applied {
            state: LayerState::Discarded { .. }
        }
    ));
    assert_eq!(repo.discard_layer(&op("discard"), &head).unwrap(), discard);
    let history = repo.layer_history(head.layer()).unwrap();
    assert_eq!(history.len(), 3);
    assert_eq!(
        history[0],
        LayerState::Active {
            head: initial.clone()
        }
    );
    assert_eq!(history[1], LayerState::Active { head: head.clone() });
    assert!(matches!(history[2], LayerState::Discarded { .. }));
    assert!(matches!(
        repo.capture_layer(&op("after-discard"), &head, &changes([]))
            .unwrap(),
        LayerUpdate::Stale {
            state: LayerState::Discarded { .. }
        }
    ));
    assert_eq!(repo.history().unwrap(), world);
    assert_eq!(repo.read_checkpoint(head.checkpoint()).unwrap(), frozen);
    let recovered = repo
        .clone_layer(&id("recover-after-discard"), frozen.id())
        .unwrap();
    assert_eq!(recovered.checkpoint(), frozen.id());
    for sibling in siblings {
        assert_eq!(
            repo.layer_state(sibling.layer()).unwrap(),
            LayerState::Active { head: sibling }
        );
    }
    assert_eq!(repo.check().unwrap().layers, 26);
    assert!(
        db.execute("UPDATE layer_heads SET revision=100", [])
            .is_err()
    );
    assert!(db.execute("DELETE FROM layer_attempts", []).is_err());
    assert!(db.execute("DELETE FROM repository_identity", []).is_err());
}

#[test]
fn frozen_reads_and_sibling_heads_survive_restart_world_advance_and_later_capture() {
    let (root, mut repo, _, _) = fixture();
    let base = repo.layer_base(version(1)).unwrap();
    let head0 = repo.create_layer(&id("parent"), &base).unwrap();
    let a = regular(&mut repo, b"a");
    let head1 = applied(
        repo.capture_layer(&op("a"), &head0, &changes([("new", Some(a.clone()))]))
            .unwrap(),
    );
    let frozen = repo.read_checkpoint(head1.checkpoint()).unwrap();
    let sibling = repo.clone_layer(&id("child"), frozen.id()).unwrap();
    let next_world = repo
        .put_manifest(&Manifest::new([(path("world-only"), a)]).unwrap())
        .unwrap();
    repo.compare_and_advance(&op("advance-world"), version(1), next_world)
        .unwrap();
    let b = regular(&mut repo, b"b");
    let head2 = applied(
        repo.capture_layer(&op("b"), &head1, &changes([("new", Some(b.clone()))]))
            .unwrap(),
    );
    let child1 = applied(
        repo.capture_layer(
            &op("child-write"),
            &sibling,
            &changes([("child-only", Some(b))]),
        )
        .unwrap(),
    );
    assert_eq!(repo.layer_diff(&head0).unwrap().len(), 0);
    assert_eq!(repo.read_checkpoint(head1.checkpoint()).unwrap(), frozen);
    drop(repo);
    let mut repo = Repository::open(root.path()).unwrap();
    assert_eq!(
        repo.layer_state(&id("parent")).unwrap(),
        LayerState::Active { head: head2 }
    );
    assert_eq!(
        repo.layer_state(&id("child")).unwrap(),
        LayerState::Active { head: child1 }
    );
    assert_eq!(repo.read_checkpoint(frozen.id()).unwrap(), frozen);
    assert_eq!(repo.create_layer(&id("parent"), &base).unwrap(), head0);
    assert!(
        repo.create_layer(&id("parent"), &repo.layer_base(version(2)).unwrap())
            .is_err()
    );
    assert_eq!(
        repo.read_checkpoint(sibling.checkpoint())
            .unwrap()
            .base()
            .version(),
        version(1)
    );
    repo.check().unwrap();
}

#[test]
fn retries_bind_exact_layer_head_checkpoint_and_action_and_retain_stale_outcome() {
    let (root, mut repo, _, _) = fixture();
    let base = repo.layer_base(version(1)).unwrap();
    let first = repo.create_layer(&id("first"), &base).unwrap();
    let second = repo.create_layer(&id("second"), &base).unwrap();
    let entry = regular(&mut repo, b"new");
    let delta = changes([("new", Some(entry))]);
    let winner = repo.capture_layer(&op("winner"), &first, &delta).unwrap();
    let head = applied(winner.clone());
    let loser = repo
        .capture_layer(&op("loser"), &first, &changes([]))
        .unwrap();
    assert!(matches!(loser, LayerUpdate::Stale { .. }));
    repo.discard_layer(&op("discard"), &head).unwrap();
    drop(repo);
    let mut repo = Repository::open(root.path()).unwrap();
    assert_eq!(
        repo.capture_layer(&op("winner"), &first, &delta).unwrap(),
        winner
    );
    assert_eq!(
        repo.capture_layer(&op("loser"), &first, &changes([]))
            .unwrap(),
        loser
    );
    for error in [
        repo.capture_layer(&op("winner"), &second, &delta)
            .unwrap_err(),
        repo.capture_layer(&op("winner"), &head, &delta)
            .unwrap_err(),
        repo.capture_layer(&op("winner"), &first, &changes([]))
            .unwrap_err(),
        repo.discard_layer(&op("winner"), &first).unwrap_err(),
    ] {
        assert!(matches!(error, Error::OperationIdReused(_)));
    }
    assert_eq!(
        repo.layer_state(second.layer()).unwrap(),
        LayerState::Active { head: second }
    );
}

#[test]
fn foreign_bases_heads_and_imported_checkpoint_objects_are_rejected() {
    let (root, mut first, _, _) = fixture();
    let other = TempDir::new().unwrap();
    let mut second = Repository::init(other.path()).unwrap();
    let base = first.layer_base(version(0)).unwrap();
    assert!(second.create_layer(&id("foreign"), &base).is_err());
    let head = first.create_layer(&id("original"), &base).unwrap();
    assert!(
        second
            .capture_layer(&op("foreign"), &head, &changes([]))
            .is_err()
    );
    let source = raw(&root);
    let bytes: Vec<u8> = source
        .query_row(
            "SELECT bytes FROM objects WHERE id=?1",
            [head.checkpoint().object().to_string()],
            |r| r.get(0),
        )
        .unwrap();
    raw(&other)
        .execute(
            "INSERT INTO objects(id,kind,bytes) VALUES(?1,3,?2)",
            params![head.checkpoint().object().to_string(), bytes],
        )
        .unwrap();
    assert!(
        second
            .clone_layer(&id("foreign"), head.checkpoint())
            .is_err()
    );
    assert!(second.read_checkpoint(head.checkpoint()).is_err());
    assert!(second.check().is_err());
    let manifest = first.put_manifest(&Manifest::new([]).unwrap()).unwrap();
    assert!(
        first
            .clone_layer(
                &id("wrong-kind"),
                CheckpointId::parse(&manifest.to_string()).unwrap()
            )
            .is_err()
    );
}

#[test]
fn structural_changes_validate_the_whole_resolved_tree_and_modes_and_symlinks() {
    let (root, mut repo, large, _) = fixture();
    let head = repo
        .create_layer(&id("tree"), &repo.layer_base(version(1)).unwrap())
        .unwrap();
    let nested = regular(&mut repo, b"nested");
    assert!(
        repo.capture_layer(
            &op("collision"),
            &head,
            &changes([("keep/child", Some(nested.clone()))])
        )
        .is_err()
    );
    assert_eq!(
        repo.layer_state(head.layer()).unwrap(),
        LayerState::Active { head: head.clone() }
    );
    let repaired = applied(
        repo.capture_layer(
            &op("collision"),
            &head,
            &changes([("keep", None), ("keep/child", Some(nested))]),
        )
        .unwrap(),
    );
    assert_eq!(repo.layer_diff(&repaired).unwrap().len(), 2);
    let target = repo.put_blob(b"../keep").unwrap();
    let link = FileEntry::Symlink { target };
    let mode = FileEntry::Regular {
        content: large.content(),
        executable: true,
    };
    let head2 = applied(
        repo.capture_layer(
            &op("links"),
            &repaired,
            &changes([("keep", Some(mode.clone())), ("remove", Some(link.clone()))]),
        )
        .unwrap(),
    );
    let diff = repo.layer_diff(&head2).unwrap();
    assert_eq!(diff[0].after, Some(mode));
    assert_eq!(diff[1].after, Some(link.clone()));
    let manifest = repo.checkpoint_manifest(head2.checkpoint()).unwrap();
    let manifest_id = repo.put_manifest(&manifest).unwrap();
    assert_eq!(repo.read_manifest(manifest_id).unwrap(), manifest);
    assert_eq!(repo.read_blob(link.content()).unwrap(), b"../keep");
    let bytes = repo.manifest_bytes(manifest_id).unwrap();
    assert!(
        String::from_utf8(bytes)
            .unwrap()
            .contains("\"kind\":\"symlink\"")
    );
    for target in [b"".as_slice(), b"bad\0target"] {
        let target = repo.put_blob(target).unwrap();
        assert!(
            repo.capture_layer(
                &op("invalid-link"),
                &head2,
                &changes([("link", Some(FileEntry::Symlink { target }))])
            )
            .is_err()
        );
    }
    drop(repo);
    Repository::open(root.path()).unwrap().check().unwrap();
}

#[test]
fn malformed_or_missing_checkpoint_graphs_and_head_history_never_fall_back() {
    for damage in [
        "checkpoint",
        "content",
        "head-gap",
        "tail",
        "stale-order",
        "stale-checkpoint",
        "stale-content",
    ] {
        let (root, mut repo, _, _) = fixture();
        let initial = repo
            .create_layer(&id("damaged"), &repo.layer_base(version(1)).unwrap())
            .unwrap();
        let changed = regular(&mut repo, b"private");
        let head = applied(
            repo.capture_layer(
                &op("accepted"),
                &initial,
                &changes([("new", Some(changed.clone()))]),
            )
            .unwrap(),
        );
        let stale_content = regular(&mut repo, b"stale-only");
        repo.capture_layer(
            &op("stale"),
            &initial,
            &changes([("new", Some(stale_content.clone()))]),
        )
        .unwrap();
        let db = raw(&root);
        db.execute_batch("PRAGMA foreign_keys=OFF; DROP TRIGGER objects_no_UPDATE; DROP TRIGGER layer_heads_no_DELETE; DROP TRIGGER layer_attempts_no_UPDATE; PRAGMA ignore_check_constraints=ON;").unwrap();
        match damage {
            "stale-checkpoint" => {
                db.execute("UPDATE objects SET bytes=x'00' WHERE id=(SELECT checkpoint FROM layer_attempts WHERE id='stale')",[]).unwrap();
            }
            "stale-content" => {
                db.execute(
                    "UPDATE objects SET bytes=x'00' WHERE id=?1",
                    [stale_content.content().to_string()],
                )
                .unwrap();
            }
            "checkpoint" => {
                db.execute(
                    "UPDATE objects SET bytes=x'00' WHERE id=?1",
                    [head.checkpoint().object().to_string()],
                )
                .unwrap();
            }
            "content" => {
                db.execute(
                    "UPDATE objects SET bytes=x'00' WHERE id=?1",
                    [changed.content().to_string()],
                )
                .unwrap();
            }
            "head-gap" => {
                db.execute("DELETE FROM layer_heads WHERE revision=0", [])
                    .unwrap();
            }
            "tail" => {
                db.execute("DELETE FROM layer_heads WHERE revision=1", [])
                    .unwrap();
            }
            "stale-order" => {
                db.execute("UPDATE layer_attempts SET result=0 WHERE id='stale'", [])
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(repo.layer_history(head.layer()).is_err(), "{damage}");
        // Commands verify what they read: the head checkpoint and its content.
        if ["checkpoint", "content"].contains(&damage) {
            assert!(
                repo.capture_layer(&op("new-op"), &head, &changes([]))
                    .is_err(),
                "{damage}"
            );
            assert!(
                repo.clone_layer(&id("clone"), head.checkpoint()).is_err(),
                "{damage}"
            );
        }
        assert!(repo.check().is_err(), "{damage}");
        assert!(Repository::init(root.path()).is_err(), "{damage}");
        if !["checkpoint", "content"].contains(&damage) {
            continue;
        }
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM layer_attempts WHERE id='new-op'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}

#[test]
fn storage_failure_rolls_back_checkpoint_head_and_outcome_then_exact_retry_succeeds() {
    let (root, mut repo, _, _) = fixture();
    let head = repo
        .create_layer(&id("rollback"), &repo.layer_base(version(1)).unwrap())
        .unwrap();
    let changed = regular(&mut repo, b"new");
    let delta = changes([("new", Some(changed))]);
    let db = raw(&root);
    let before = usage(&db);
    db.execute_batch("CREATE TRIGGER refuse_head BEFORE INSERT ON layer_heads WHEN NEW.revision=1 BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
    assert!(matches!(
        repo.capture_layer(&op("retry"), &head, &delta),
        Err(Error::Storage(_))
    ));
    assert_eq!(usage(&db), before);
    assert_eq!(
        repo.layer_state(head.layer()).unwrap(),
        LayerState::Active { head: head.clone() }
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM layer_attempts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    db.execute_batch("DROP TRIGGER refuse_head;").unwrap();
    drop(repo);
    let mut repo = Repository::open(root.path()).unwrap();
    let next = applied(repo.capture_layer(&op("retry"), &head, &delta).unwrap());
    assert_eq!(next.revision(), 1);
}

#[test]
fn external_boundaries_and_unshipped_old_format_are_rejected_without_rewrite() {
    for invalid in ["", "../x", "space key", "nonascii-λ"] {
        assert!(LayerId::parse(invalid).is_err());
    }
    assert!(CheckpointId::parse("bad").is_err());
    assert!(SparseChanges::new([(path("x"), None), (path("x"), None)]).is_err());
    let root = TempDir::new().unwrap();
    drop(Repository::init(root.path()).unwrap());
    let db = raw(&root);
    db.execute_batch("PRAGMA user_version=1;").unwrap();
    assert!(matches!(
        Repository::open(root.path()),
        Err(Error::UnsupportedFormat(1))
    ));
    assert!(matches!(
        Repository::init(root.path()),
        Err(Error::UnsupportedFormat(1))
    ));
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM versions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn valid_hash_checkpoint_must_have_exact_base_canonical_sparse_paths_and_valid_graph() {
    let (root, mut repo, large, _) = fixture();
    let base = repo.layer_base(version(1)).unwrap();
    let identity = serde_json::to_value(&base).unwrap()["repository"].clone();
    let original =
        serde_json::json!({"repository":identity,"version":1,"world":base.world(),"changes":{}});
    let mut invalid_path = original.clone();
    invalid_path["changes"] = serde_json::json!({"../bad":null});
    let mut wrong_base = original.clone();
    wrong_base["version"] = serde_json::json!(0);
    let mut collision = original.clone();
    collision["changes"] = serde_json::json!({"keep/child":large});
    let mut missing = original.clone();
    missing["changes"] =
        serde_json::json!({"new":{"kind":"regular","content":"0".repeat(64),"executable":false}});
    let mut unknown = original.clone();
    unknown["unexpected"] = serde_json::json!(true);
    let db = raw(&root);
    for invalid in [invalid_path, wrong_base, collision, missing, unknown] {
        let bytes = serde_json::to_vec(&invalid).unwrap();
        let checkpoint = insert_checkpoint_bytes(&db, &bytes);
        assert!(repo.read_checkpoint(checkpoint).is_err());
        assert!(repo.clone_layer(&id("invalid"), checkpoint).is_err());
        assert_eq!(
            db.query_row("SELECT count(*) FROM layer_heads", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert!(repo.check().is_err());
}
