use javelin::{
    AdvanceOutcome, Error, FileEntry, Manifest, ObjectId, OperationId, RepoPath, Repository,
    VersionNumber,
};
use rusqlite::{Connection, params};
use tempfile::TempDir;

fn version(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}
fn op(id: &str) -> OperationId {
    OperationId::parse(id).unwrap()
}
fn snapshot(repo: &mut Repository, bytes: &[u8]) -> (ObjectId, ObjectId) {
    let blob = repo.put_blob(bytes).unwrap();
    let manifest = Manifest::new([(
        RepoPath::parse("src/data.bin").unwrap(),
        FileEntry::Regular {
            content: blob,
            executable: true,
        },
    )])
    .unwrap();
    (blob, repo.put_manifest(&manifest).unwrap())
}
/// The derived tree root a manifest's World version records.
fn tree_of(repo: &Repository, manifest: ObjectId) -> ObjectId {
    let manifest = repo.read_manifest(manifest).unwrap();
    javelin::tree::Trees::new(javelin::tree::MemoryStore::default())
        .build(manifest.entries())
        .unwrap()
}
fn raw(root: &TempDir) -> Connection {
    Connection::open(root.path().join(".javelin/repository.sqlite3")).unwrap()
}

#[test]
fn immutable_bytes_and_manifests_survive_reopen() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let data: Vec<_> = (0..=255).cycle().take(1024 * 1024).collect();
    let (blob, manifest) = snapshot(&mut repo, &data);
    let empty = repo.put_blob(b"").unwrap();
    assert_ne!(empty, repo.version(version(0)).unwrap().id);
    let before = repo.manifest_bytes(manifest).unwrap();
    assert_eq!(repo.put_blob(&data).unwrap(), blob);
    repo.compare_and_advance(&op("first"), version(0), manifest)
        .unwrap();
    drop(repo);
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(repo.read_blob(blob).unwrap(), data);
    assert_eq!(repo.read_blob(empty).unwrap(), b"");
    assert_eq!(repo.manifest_bytes(manifest).unwrap(), before);
    assert_eq!(
        repo.read_manifest(manifest).unwrap().canonical_bytes(),
        before
    );
    assert_eq!(
        repo.history()
            .unwrap()
            .iter()
            .map(|v| v.number.get())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert!(matches!(
        repo.read_blob(manifest),
        Err(Error::WrongObjectKind(_))
    ));
    assert!(matches!(
        repo.read_manifest(blob),
        Err(Error::WrongObjectKind(_))
    ));
    assert_eq!(repo.check().unwrap().objects, 6); // with the empty and accepted trees
}

#[test]
fn path_and_manifest_boundaries() {
    for path in [
        "",
        "/a",
        "a/",
        "a//b",
        ".",
        "..",
        "a/../b",
        "a/./b",
        "C:/a",
        "a\\b",
        "a\0b",
        ".javelin/x",
        "A/.JAVELIN/x",
    ] {
        assert!(RepoPath::parse(path).is_err(), "accepted {path:?}");
    }
    assert!(RepoPath::parse("src/日本語 space.rs").is_ok());
    assert!(ObjectId::parse(&"f".repeat(63)).is_err());
    assert!(ObjectId::parse(&"F".repeat(64)).is_err());
    for id in ["", "a b", "a/b"] {
        assert!(OperationId::parse(id).is_err());
    }
    assert!(VersionNumber::new(u64::MAX).is_err());
    let entry = FileEntry::Regular {
        content: ObjectId::parse(&"0".repeat(64)).unwrap(),
        executable: false,
    };
    let path = RepoPath::parse("a").unwrap();
    assert!(Manifest::new([(path.clone(), entry.clone()), (path.clone(), entry.clone())]).is_err());
    assert!(
        Manifest::new([
            (path, entry.clone()),
            (RepoPath::parse("a/b").unwrap(), entry)
        ])
        .is_err()
    );
}

#[test]
fn missing_corrupt_objects_are_rejected_without_advancing() {
    for corrupt in [false, true] {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let (blob, manifest) = snapshot(&mut repo, &[0, 255, 1, 0]);
        let db = raw(&root);
        if corrupt {
            db.execute_batch("DROP TRIGGER objects_no_UPDATE;").unwrap();
            db.execute(
                "UPDATE objects SET bytes=?1 WHERE id=?2",
                params![b"bad".as_slice(), blob.to_string()],
            )
            .unwrap();
        } else {
            db.execute_batch("DROP TRIGGER objects_no_DELETE;").unwrap();
            db.execute("DELETE FROM objects WHERE id=?1", [blob.to_string()])
                .unwrap();
        }
        assert_eq!(
            repo.read_blob(blob).unwrap_err().code(),
            if corrupt {
                "corrupt_object"
            } else {
                "missing_object"
            }
        );
        assert!(repo.read_manifest(manifest).is_err());
        assert!(
            repo.compare_and_advance(&op("bad"), version(0), manifest)
                .is_err()
        );
        assert_eq!(repo.history().unwrap().len(), 1);
        assert!(repo.check().is_err());
        assert_eq!(repo.put_blob(&[0, 255, 1, 0]).is_err(), corrupt);
    }
}

#[test]
fn history_corruption_fails_closed_and_mutations_are_rejected() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let (_, manifest) = snapshot(&mut repo, b"old");
    repo.compare_and_advance(&op("first"), version(0), manifest)
        .unwrap();
    let db = raw(&root);
    assert!(
        db.execute(
            "UPDATE versions SET manifest=?1 WHERE number=1",
            [manifest.to_string()]
        )
        .is_err()
    );
    assert!(db.execute("DELETE FROM attempts", []).is_err());
    db.execute_batch("PRAGMA foreign_keys=OFF; DROP TRIGGER versions_no_DELETE; DELETE FROM versions WHERE number=0;")
        .unwrap();
    assert!(repo.history().is_err());
    assert!(repo.check().is_err());
    assert!(repo.check().is_err());
}

#[test]
fn missing_tail_world_is_corruption_when_any_outcome_retains_its_anchor() {
    for retain_accepted in [false, true] {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let (_, manifest) = snapshot(&mut repo, b"acknowledged");
        repo.compare_and_advance(&op("accepted"), version(0), manifest)
            .unwrap();
        if !retain_accepted {
            assert_eq!(
                repo.compare_and_advance(&op("stale"), version(0), manifest)
                    .unwrap(),
                AdvanceOutcome::Stale { actual: version(1) }
            );
        }
        let db = raw(&root);
        db.execute_batch("PRAGMA foreign_keys=OFF; DROP TRIGGER versions_no_DELETE; DELETE FROM versions WHERE number=1;").unwrap();
        if !retain_accepted {
            db.execute_batch(
                "DROP TRIGGER attempts_no_DELETE; DELETE FROM attempts WHERE id='accepted';",
            )
            .unwrap();
        }
        drop(repo);
        let repo = Repository::open(root.path()).unwrap();
        assert!(
            matches!(repo.history(), Err(Error::CorruptRepository(_))),
            "retained accepted anchor: {retain_accepted}"
        );
        assert!(matches!(repo.check(), Err(Error::CorruptRepository(_))));
        assert!(matches!(
            Repository::init(root.path()),
            Err(Error::CorruptRepository(_))
        ));
        assert!(matches!(repo.check(), Err(Error::CorruptRepository(_))));
        assert_eq!(
            db.query_row("SELECT count(*) FROM versions", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM attempts WHERE id='new'", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn repeated_init_preserves_incidental_files_in_a_valid_store() {
    let root = TempDir::new().unwrap();
    let repo = Repository::init(root.path()).unwrap();
    let old = repo.history().unwrap();
    let incidental = root.path().join(".javelin/.DS_Store");
    std::fs::write(&incidental, b"incidental metadata").unwrap();
    let repo = Repository::init(root.path()).unwrap();
    assert_eq!(repo.history().unwrap(), old);
    assert_eq!(std::fs::read(incidental).unwrap(), b"incidental metadata");
}

#[test]
fn empty_interrupted_database_is_not_initialized_and_can_resume() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join(".javelin")).unwrap();
    drop(raw(&root));
    assert!(matches!(
        Repository::open(root.path()),
        Err(Error::NotInitialized(_))
    ));
    let repo = Repository::init(root.path()).unwrap();
    assert_eq!(repo.history().unwrap().len(), 1);
}

#[test]
fn durable_accepted_and_stale_outcomes_bind_exact_input() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let (_, a) = snapshot(&mut repo, b"a");
    let (_, b) = snapshot(&mut repo, b"b");
    let winner = repo
        .compare_and_advance(&op("winner"), version(0), a)
        .unwrap();
    assert!(matches!(winner, AdvanceOutcome::Accepted { ref version } if version.number.get()==1));
    let loser = repo
        .compare_and_advance(&op("loser"), version(0), b)
        .unwrap();
    assert_eq!(loser, AdvanceOutcome::Stale { actual: version(1) });
    repo.compare_and_advance(&op("later"), version(1), b)
        .unwrap();
    drop(repo);
    let mut repo = Repository::open(root.path()).unwrap();
    assert_eq!(
        repo.compare_and_advance(&op("winner"), version(0), a)
            .unwrap(),
        winner
    );
    assert_eq!(
        repo.compare_and_advance(&op("loser"), version(0), b)
            .unwrap(),
        loser
    );
    for (id, expected, candidate) in [
        ("winner", version(1), a),
        ("winner", version(0), b),
        ("loser", version(1), b),
    ] {
        assert!(matches!(
            repo.compare_and_advance(&op(id), expected, candidate),
            Err(Error::OperationIdReused(_))
        ));
    }
    assert_eq!(repo.history().unwrap().len(), 3);
    assert_eq!(repo.check().unwrap().attempts, 3);
}

#[test]
fn init_preserves_existing_files_and_never_clobbers_a_store() {
    let root = TempDir::new().unwrap();
    std::fs::write(root.path().join("data"), [0, 255, 42]).unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    assert!(repo.manifest_at(version(0)).unwrap().entries().is_empty());
    let (_, manifest) = snapshot(&mut repo, b"new");
    repo.compare_and_advance(&op("accepted"), version(0), manifest)
        .unwrap();
    let old = repo.history().unwrap();
    drop(repo);
    let repo = Repository::init(root.path()).unwrap();
    assert_eq!(repo.history().unwrap(), old);
    assert_eq!(
        std::fs::read(root.path().join("data")).unwrap(),
        [0, 255, 42]
    );
    let bad = TempDir::new().unwrap();
    std::fs::create_dir(bad.path().join(".javelin")).unwrap();
    let db = Connection::open(bad.path().join(".javelin/repository.sqlite3")).unwrap();
    db.execute_batch(
        "CREATE TABLE foreign_data(value TEXT); INSERT INTO foreign_data VALUES('keep');",
    )
    .unwrap();
    assert!(Repository::init(bad.path()).is_err());
    assert_eq!(
        db.query_row("SELECT value FROM foreign_data", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "keep"
    );
}

#[test]
fn accepted_graph_damage_never_falls_back_to_an_older_world() {
    for damage_tree in [false, true] {
        for delete in [false, true] {
            let root = TempDir::new().unwrap();
            let mut repo = Repository::init(root.path()).unwrap();
            let (blob, manifest) = snapshot(&mut repo, b"accepted bytes");
            repo.compare_and_advance(&op("first"), version(0), manifest)
                .unwrap();
            let damaged = if damage_tree {
                tree_of(&repo, manifest)
            } else {
                blob
            };
            let db = raw(&root);
            db.execute_batch("PRAGMA foreign_keys=OFF; DROP TRIGGER objects_no_UPDATE; DROP TRIGGER objects_no_DELETE;").unwrap();
            if delete {
                db.execute("DELETE FROM objects WHERE id=?1", [damaged.to_string()])
                    .unwrap();
            } else {
                db.execute(
                    "UPDATE objects SET bytes=x'00' WHERE id=?1",
                    [damaged.to_string()],
                )
                .unwrap();
            }
            assert!(repo.history().is_err());
            assert!(repo.check().is_err());
            let versions = |db: &Connection| {
                db.query_row("SELECT count(*) FROM versions", [], |r| r.get::<_, i64>(0))
                    .unwrap()
            };
            if damage_tree && delete {
                // Trees are derived: rebuilding the candidate's tree restores the
                // missing chunk with verified bytes, and the World stays valid.
                repo.compare_and_advance(&op("retry"), version(1), manifest)
                    .unwrap();
                repo.check().unwrap();
                assert_eq!(versions(&db), 3);
                continue;
            }
            assert!(
                repo.compare_and_advance(&op("retry"), version(1), manifest)
                    .is_err()
            );
            assert!(repo.check().is_err());
            assert!(Repository::init(root.path()).is_err());
            assert_eq!(versions(&db), 2);
        }
    }
}

#[test]
fn valid_hash_does_not_make_a_malformed_manifest_valid() {
    use sha2::{Digest, Sha256};
    for bytes in [
        b"{ }".as_slice(),
        b"{\"../x\":{\"content\":\"bad\",\"executable\":false}}".as_slice(),
        b"{}\n".as_slice(),
        b"[]".as_slice(),
    ] {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let mut hash = Sha256::new();
        hash.update(b"javelin-object-v1\0");
        hash.update([2]);
        hash.update(bytes);
        let encoded: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        let id = ObjectId::parse(&encoded).unwrap();
        raw(&root)
            .execute(
                "INSERT INTO objects(id,kind,bytes) VALUES(?1,2,?2)",
                params![id.to_string(), bytes],
            )
            .unwrap();
        assert!(matches!(
            repo.read_manifest(id),
            Err(Error::CorruptObject(_))
        ));
        assert!(
            repo.compare_and_advance(&op("invalid"), version(0), id)
                .is_err()
        );
        assert_eq!(repo.history().unwrap().len(), 1);
        assert!(repo.check().is_err());
    }
}

#[test]
fn manifest_preparation_rejects_missing_or_wrong_kind_references() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let initial = repo.put_manifest(&Manifest::new([]).unwrap()).unwrap();
    for content in [initial, ObjectId::parse(&"0".repeat(64)).unwrap()] {
        let manifest = Manifest::new([(
            RepoPath::parse("file").unwrap(),
            FileEntry::Regular {
                content,
                executable: false,
            },
        )])
        .unwrap();
        assert!(repo.put_manifest(&manifest).is_err());
    }
    assert!(
        repo.compare_and_advance(&op("future"), version(1), initial)
            .is_err()
    );
    assert_eq!(repo.check().unwrap().objects, 2); // with the empty World tree
    assert_eq!(repo.check().unwrap().attempts, 0);
}

#[test]
fn stale_only_candidate_graph_corruption_is_rejected_by_every_history_validator() {
    for damage_tree in [false, true] {
        for delete in [false, true] {
            let root = TempDir::new().unwrap();
            let mut repo = Repository::init(root.path()).unwrap();
            let (_, accepted) = snapshot(&mut repo, b"accepted");
            let (blob, stale) = snapshot(&mut repo, b"only stale");
            repo.compare_and_advance(&op("accepted"), version(0), accepted)
                .unwrap();
            repo.compare_and_advance(&op("stale"), version(0), stale)
                .unwrap();
            let db = raw(&root);
            db.execute_batch("PRAGMA foreign_keys=OFF; DROP TRIGGER objects_no_UPDATE; DROP TRIGGER objects_no_DELETE;").unwrap();
            let damaged = if damage_tree {
                tree_of(&repo, stale)
            } else {
                blob
            };
            if delete {
                db.execute("DELETE FROM objects WHERE id=?1", [damaged.to_string()])
                    .unwrap();
            } else {
                db.execute(
                    "UPDATE objects SET bytes=x'00' WHERE id=?1",
                    [damaged.to_string()],
                )
                .unwrap();
            }
            assert!(repo.history().is_err());
            assert!(repo.check().is_err());
            assert!(Repository::init(root.path()).is_err());
            if damage_tree && delete {
                // Replaying the stale attempt rebuilds its derived tree.
                repo.compare_and_advance(&op("stale"), version(0), stale)
                    .unwrap();
                repo.check().unwrap();
                continue;
            }
            assert!(
                repo.compare_and_advance(&op("stale"), version(0), stale)
                    .is_err()
            );
            assert!(repo.check().is_err());
            assert_eq!(
                db.query_row("SELECT count(*) FROM attempts", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                2
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM versions", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                2
            );
        }
    }
}

#[test]
fn invalid_stale_outcome_ordering_is_rejected_without_new_worlds_or_attempts() {
    for expected in [0, 1] {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let (_, manifest) = snapshot(&mut repo, b"data");
        repo.compare_and_advance(&op("accepted"), version(0), manifest)
            .unwrap();
        repo.compare_and_advance(&op("stale"), version(0), manifest)
            .unwrap();
        let db = raw(&root);
        db.execute_batch("DROP TRIGGER attempts_no_UPDATE;")
            .unwrap();
        db.execute(
            "UPDATE attempts SET expected=?1,result=?1 WHERE id='stale'",
            [expected],
        )
        .unwrap();
        assert!(matches!(repo.history(), Err(Error::CorruptRepository(_))));
        assert!(repo.check().is_err());
        assert!(Repository::init(root.path()).is_err());
        assert!(repo.check().is_err());
        assert_eq!(
            db.query_row("SELECT count(*) FROM attempts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM versions", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
}

#[test]
fn metadata_write_failure_rolls_back_the_attempt_and_allows_exact_retry() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let (_, manifest) = snapshot(&mut repo, b"candidate");
    let db = raw(&root);
    db.execute_batch("CREATE TRIGGER refuse_world BEFORE INSERT ON versions WHEN NEW.number=1 BEGIN SELECT RAISE(ABORT,'injected write failure'); END;").unwrap();
    assert!(matches!(
        repo.compare_and_advance(&op("write-failure"), version(0), manifest),
        Err(Error::Storage(_))
    ));
    let report = repo.check().unwrap();
    assert_eq!((report.versions, report.attempts), (1, 0));
    db.execute_batch("DROP TRIGGER refuse_world;").unwrap();
    drop(repo);
    let mut repo = Repository::open(root.path()).unwrap();
    assert!(matches!(
        repo.compare_and_advance(&op("write-failure"), version(0), manifest)
            .unwrap(),
        AdvanceOutcome::Accepted { .. }
    ));
    let report = repo.check().unwrap();
    assert_eq!((report.versions, report.attempts), (2, 1));
}

#[cfg(unix)]
#[test]
fn storage_symlinks_are_rejected_without_touching_the_target() {
    use std::os::unix::fs::symlink;
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    symlink(outside.path(), root.path().join(".javelin")).unwrap();
    assert!(Repository::init(root.path()).is_err());
    assert!(!outside.path().join("repository.sqlite3").exists());
    let second = TempDir::new().unwrap();
    std::fs::create_dir(second.path().join(".javelin")).unwrap();
    std::fs::write(outside.path().join("keep"), b"keep").unwrap();
    symlink(
        outside.path().join("keep"),
        second.path().join(".javelin/repository.sqlite3"),
    )
    .unwrap();
    assert!(Repository::init(second.path()).is_err());
    assert!(Repository::open(second.path()).is_err());
    assert_eq!(std::fs::read(outside.path().join("keep")).unwrap(), b"keep");
}
