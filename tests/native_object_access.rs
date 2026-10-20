use javelin::{
    Error, FileEntry, Manifest, ObjectId, OperationId, RepoPath, Repository, VersionNumber,
};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn raw(root: &TempDir) -> Connection {
    Connection::open(root.path().join(".javelin/repository.sqlite3")).unwrap()
}

fn manifest(entry: FileEntry) -> Manifest {
    Manifest::new([(RepoPath::parse("file").unwrap(), entry)]).unwrap()
}

fn regular(content: ObjectId) -> FileEntry {
    FileEntry::Regular {
        content,
        executable: false,
    }
}

fn insert_manifest(db: &Connection, manifest: &Manifest) -> ObjectId {
    let id = ObjectId::for_manifest(manifest);
    db.execute(
        "INSERT INTO objects(id,kind,bytes) VALUES(?1,2,?2)",
        params![id.to_string(), manifest.canonical_bytes()],
    )
    .unwrap();
    id
}

#[test]
fn public_ids_match_stable_storage_hashes_and_raw_bytes() {
    for (bytes, expected) in [
        (
            b"".as_slice(),
            "66d90723d82b8489d0cfc2a960be2f2f0d05d9989fd4f37d68685185494b123c",
        ),
        (
            b"abc".as_slice(),
            "e11027666bfaee1d18ab5189966d70fa8e888c53ec3ec66a6c5139c1a6708589",
        ),
        (
            &[0, 255],
            "0ba8ef36ac42135b50f27ee23d4314e780affff6dced6f99f7c79fd1bf314ec6",
        ),
    ] {
        let id = ObjectId::for_blob(bytes);
        assert_eq!(id.to_string(), expected);
        assert_eq!(ObjectId::from_bytes(*id.as_bytes()), id);
        assert_eq!(
            serde_json::to_string(&id).unwrap(),
            format!("\"{expected}\"")
        );
    }
    let empty = Manifest::new([]).unwrap();
    let id = ObjectId::for_manifest(&empty);
    assert_eq!(
        id.to_string(),
        "2169214789bd2b417d915c470c6d8d4de146200334bd19930a24ebce709e763e"
    );
    assert_ne!(id, ObjectId::for_blob(b"{}"));
    for start in [0_u8, 32, 64, 96, 128, 160, 192, 224] {
        let bytes = std::array::from_fn(|i| start + i as u8);
        assert_eq!(ObjectId::from_bytes(bytes).as_bytes(), &bytes);
    }
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    assert_eq!(repo.put_manifest(&empty).unwrap(), id);
    assert_eq!(repo.put_blob(b"abc").unwrap(), ObjectId::for_blob(b"abc"));
}

// A downstream receiver needs only public APIs to refuse a claim before storage.
fn receive_blob(
    repo: &mut Repository,
    claimed: [u8; 32],
    bytes: &[u8],
) -> javelin::Result<ObjectId> {
    let id = ObjectId::from_bytes(claimed);
    if ObjectId::for_blob(bytes) != id {
        return Err(Error::InvalidInput(
            "claimed ID differs from content".into(),
        ));
    }
    repo.put_blob(bytes)
}

#[test]
fn downstream_caller_checks_claims_before_storing_and_uses_typed_availability() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let claimed = ObjectId::for_blob(b"expected");
    assert!(receive_blob(&mut repo, *claimed.as_bytes(), b"different").is_err());
    assert!(!repo.has_blob(ObjectId::for_blob(b"different")).unwrap());
    assert_eq!(repo.check().unwrap().objects, 1); // the empty World tree
    assert_eq!(
        receive_blob(&mut repo, *claimed.as_bytes(), b"expected").unwrap(),
        claimed
    );
    let m = manifest(regular(claimed));
    let manifest_id = ObjectId::for_manifest(&m);
    let wrong_claim = ObjectId::from_bytes([0; 32]);
    assert_ne!(manifest_id, wrong_claim);
    assert!(!repo.has_manifest(wrong_claim).unwrap());
    assert_eq!(repo.check().unwrap().objects, 2);
    assert_eq!(repo.put_manifest(&m).unwrap(), manifest_id);
    let requested = [claimed.as_bytes(), manifest_id.as_bytes()];
    assert!(repo.has_blob(ObjectId::from_bytes(*requested[0])).unwrap());
    assert!(
        repo.has_manifest(ObjectId::from_bytes(*requested[1]))
            .unwrap()
    );
    assert_eq!(repo.read_blob_bounded(claimed, 8).unwrap(), b"expected");
    assert_eq!(
        repo.manifest_bytes_bounded(manifest_id, 1024).unwrap(),
        m.canonical_bytes()
    );
}

#[test]
fn missing_kind_and_cap_boundaries_are_explicit() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let missing = ObjectId::from_bytes([0; 32]);
    assert!(!repo.has_blob(missing).unwrap());
    assert!(!repo.has_manifest(missing).unwrap());
    assert!(
        matches!(repo.read_blob_bounded(missing, 0), Err(Error::MissingObject(id)) if id == missing)
    );
    assert!(
        matches!(repo.manifest_bytes_bounded(missing, 0), Err(Error::MissingObject(id)) if id == missing)
    );
    let empty = repo.put_blob(b"").unwrap();
    assert!(repo.has_blob(empty).unwrap());
    assert_eq!(repo.read_blob_bounded(empty, 0).unwrap(), b"");
    let blob = repo.put_blob(b"abc").unwrap();
    assert_eq!(repo.read_blob_bounded(blob, 3).unwrap(), b"abc");
    assert_eq!(repo.read_blob_bounded(blob, usize::MAX).unwrap(), b"abc");
    for limit in [0, 2] {
        let error = repo.read_blob_bounded(blob, limit).unwrap_err();
        assert_eq!(error.code(), "object_too_large");
        assert!(
            matches!(error, Error::ObjectTooLarge { id, size: 3, limit: actual } if id == blob && actual == limit)
        );
    }
    let m = repo.put_manifest(&Manifest::new([]).unwrap()).unwrap();
    assert_eq!(repo.manifest_bytes_bounded(m, 2).unwrap(), b"{}");
    for limit in [0, 1] {
        assert!(matches!(
            repo.manifest_bytes_bounded(m, limit),
            Err(Error::ObjectTooLarge { size: 2, .. })
        ));
    }
    assert!(matches!(repo.has_blob(m), Err(Error::WrongObjectKind(id)) if id == m));
    assert!(matches!(repo.has_manifest(blob), Err(Error::WrongObjectKind(id)) if id == blob));
    assert!(matches!(repo.read_blob_bounded(m, 2), Err(Error::WrongObjectKind(id)) if id == m));
    assert!(
        matches!(repo.manifest_bytes_bounded(blob, 3), Err(Error::WrongObjectKind(id)) if id == blob)
    );
    assert_eq!(
        raw(&root)
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        7
    );
}

#[test]
fn hash_damage_is_an_error_even_when_the_requested_kind_is_wrong() {
    for damage_manifest in [false, true] {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let id = if damage_manifest {
            repo.put_manifest(&Manifest::new([]).unwrap()).unwrap()
        } else {
            repo.put_blob(b"abc").unwrap()
        };
        let db = raw(&root);
        db.execute_batch("DROP TRIGGER objects_no_UPDATE;").unwrap();
        db.execute(
            "UPDATE objects SET bytes=x'00' WHERE id=?1",
            [id.to_string()],
        )
        .unwrap();
        for result in [repo.has_blob(id), repo.has_manifest(id)] {
            assert!(matches!(result, Err(Error::CorruptObject(actual)) if actual == id));
        }
        for result in [
            repo.read_blob_bounded(id, 1),
            repo.manifest_bytes_bounded(id, 1),
        ] {
            assert!(matches!(result, Err(Error::CorruptObject(actual)) if actual == id));
        }
        assert!(matches!(
            repo.read_blob_bounded(id, 0),
            Err(Error::ObjectTooLarge { .. })
        ));
        assert!(repo.check().is_err());
    }
}

#[test]
fn object_only_success_never_certifies_a_broken_graph_or_advances_world() {
    for defect in [
        "missing",
        "damaged",
        "wrong_kind",
        "empty_target",
        "nul_target",
    ] {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let db = raw(&root);
        let reference = match defect {
            "wrong_kind" => repo.put_manifest(&Manifest::new([]).unwrap()).unwrap(),
            "empty_target" => repo.put_blob(b"").unwrap(),
            "nul_target" => repo.put_blob(b"../outside\0").unwrap(),
            _ => repo.put_blob(b"valid").unwrap(),
        };
        let m = manifest(if defect.ends_with("target") {
            FileEntry::Symlink { target: reference }
        } else {
            regular(reference)
        });
        let id = insert_manifest(&db, &m);
        if defect == "missing" {
            db.execute_batch("DROP TRIGGER objects_no_DELETE;").unwrap();
            db.execute("DELETE FROM objects WHERE id=?1", [reference.to_string()])
                .unwrap();
        } else if defect == "damaged" {
            db.execute_batch("DROP TRIGGER objects_no_UPDATE;").unwrap();
            db.execute(
                "UPDATE objects SET bytes=x'00' WHERE id=?1",
                [reference.to_string()],
            )
            .unwrap();
        }
        assert!(repo.has_manifest(id).unwrap(), "{defect}");
        assert_eq!(
            repo.manifest_bytes_bounded(id, 1024).unwrap(),
            m.canonical_bytes()
        );
        assert!(repo.read_manifest(id).is_err(), "{defect}");
        assert!(repo.manifest_bytes(id).is_err(), "{defect}");
        assert!(repo.check().is_err(), "{defect}");
        assert!(
            repo.compare_and_advance(
                &OperationId::parse("refuse").unwrap(),
                VersionNumber::new(0).unwrap(),
                id
            )
            .is_err(),
            "{defect}"
        );
        assert_eq!(repo.history().unwrap().len(), 1);
        assert_eq!(
            db.query_row("SELECT count(*) FROM attempts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn accepted_graph_damage_remains_visible_after_object_only_success() {
    for delete in [false, true] {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let blob = repo.put_blob(b"acknowledged").unwrap();
        let m = manifest(regular(blob));
        let id = repo.put_manifest(&m).unwrap();
        repo.compare_and_advance(
            &OperationId::parse("first").unwrap(),
            VersionNumber::new(0).unwrap(),
            id,
        )
        .unwrap();
        let db = raw(&root);
        db.execute_batch(
            "PRAGMA foreign_keys=OFF; DROP TRIGGER objects_no_UPDATE; DROP TRIGGER objects_no_DELETE;",
        )
        .unwrap();
        if delete {
            db.execute("DELETE FROM objects WHERE id=?1", [blob.to_string()])
                .unwrap();
        } else {
            db.execute(
                "UPDATE objects SET bytes=x'00' WHERE id=?1",
                [blob.to_string()],
            )
            .unwrap();
        }
        assert!(repo.has_manifest(id).unwrap());
        assert_eq!(
            repo.manifest_bytes_bounded(id, 1024).unwrap(),
            m.canonical_bytes()
        );
        assert!(repo.read_manifest(id).is_err());
        assert!(repo.manifest_bytes(id).is_err());
        assert!(repo.history().is_err());
        assert!(repo.check().is_err());
        assert!(Repository::init(root.path()).is_err());
        assert!(
            repo.compare_and_advance(
                &OperationId::parse("later").unwrap(),
                VersionNumber::new(1).unwrap(),
                id
            )
            .is_err()
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM versions", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM attempts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}

#[test]
fn valid_hash_noncanonical_manifests_have_bytes_but_canonical_reads_refuse() {
    // Adversarial fixture hashing deliberately bypasses the public canonical
    // manifest constructor. Production callers use ObjectId::for_manifest.
    for bytes in [
        b"{ }".as_slice(),
        b"{}\n".as_slice(),
        b"[]".as_slice(),
        b"{\"../x\":{}}".as_slice(),
    ] {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let mut hash = Sha256::new();
        hash.update(b"javelin-object-v1\0");
        hash.update([2]);
        hash.update(bytes);
        let id = ObjectId::from_bytes(hash.finalize().into());
        raw(&root)
            .execute(
                "INSERT INTO objects(id,kind,bytes) VALUES(?1,2,?2)",
                params![id.to_string(), bytes],
            )
            .unwrap();
        assert!(repo.has_manifest(id).unwrap());
        assert!(
            matches!(repo.manifest_bytes_bounded(id, bytes.len()), Err(Error::CorruptObject(actual)) if actual == id)
        );
        assert!(repo.read_manifest(id).is_err());
        assert!(repo.check().is_err());
        assert!(
            repo.compare_and_advance(
                &OperationId::parse("refuse").unwrap(),
                VersionNumber::new(0).unwrap(),
                id
            )
            .is_err()
        );
        assert_eq!(repo.history().unwrap().len(), 1);
    }
}

#[test]
fn symlink_target_bytes_are_read_without_following_the_target() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let outside = TempDir::new().unwrap();
    let target = outside.path().join("secret");
    std::fs::write(&target, b"never read through this link").unwrap();
    let target_bytes = target.to_str().unwrap().as_bytes();
    let blob = repo.put_blob(target_bytes).unwrap();
    let m = manifest(FileEntry::Symlink { target: blob });
    let id = repo.put_manifest(&m).unwrap();
    assert!(repo.has_blob(blob).unwrap());
    assert!(repo.has_manifest(id).unwrap());
    assert_eq!(
        repo.read_blob_bounded(blob, target_bytes.len()).unwrap(),
        target_bytes
    );
    assert_eq!(
        repo.manifest_bytes_bounded(id, 1024).unwrap(),
        m.canonical_bytes()
    );
    assert_eq!(repo.read_manifest(id).unwrap(), m);
}

#[test]
fn concurrent_row_replacement_never_mixes_metadata_and_payload() {
    use std::{
        sync::{Arc, Barrier},
        time::Duration,
    };
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let bytes = vec![0x42; 256 * 1024];
    let id = repo.put_blob(&bytes).unwrap();
    let path = root.path().join(".javelin/repository.sqlite3");
    raw(&root)
        .execute_batch("DROP TRIGGER objects_no_UPDATE;")
        .unwrap();
    let start = Arc::new(Barrier::new(2));
    let writer_start = start.clone();
    let writer = std::thread::spawn(move || {
        let db = Connection::open(path).unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        let alternate = vec![0x53; 256 * 1024];
        let alternate_id = ObjectId::for_blob(&alternate);
        let original = vec![0x42_u8; 256 * 1024];
        writer_start.wait();
        for _ in 0..64 {
            db.execute(
                "UPDATE objects SET id=?1,bytes=?2 WHERE id=?3",
                params![alternate_id.to_string(), &alternate, id.to_string()],
            )
            .unwrap();
            db.execute(
                "UPDATE objects SET id=?1,bytes=?2 WHERE id=?3",
                params![id.to_string(), &original, alternate_id.to_string()],
            )
            .unwrap();
        }
    });
    start.wait();
    for _ in 0..128 {
        // Every committed state either has the exact object or lacks it.
        // A hash error would expose metadata from one state and another payload.
        assert!(repo.has_blob(id).is_ok());
        match repo.read_blob_bounded(id, bytes.len()) {
            Ok(actual) => assert_eq!(actual, bytes),
            Err(Error::MissingObject(missing)) => assert_eq!(missing, id),
            result => panic!("mixed snapshot: {result:?}"),
        }
    }
    writer.join().unwrap();
    assert!(repo.has_blob(id).unwrap());
}
