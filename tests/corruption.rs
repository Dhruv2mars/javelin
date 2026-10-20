//! Corruption detection. Bytes are flipped in stored blobs, manifests and trees, and
//! SQLite rows and whole files are altered. Each case must fail with a named error,
//! and every read must either fail or return exactly the original content.
//! docs/robustness.md states the contract.
use std::{fs, path::Path};

use javelin::{
    AdvanceOutcome, FileEntry, Manifest, OperationId, RepoPath, Repository, VersionNumber,
};
use rusqlite::Connection;
use tempfile::TempDir;

/// Every version's files as (path, content), oldest first.
type Snapshot = Vec<(u64, Vec<(String, Vec<u8>)>)>;

fn version(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}

fn op(id: &str) -> OperationId {
    OperationId::parse(id).unwrap()
}

fn publish(repo: &mut Repository, expected: u64, files: &[(&str, &[u8])]) {
    let manifest = Manifest::new(files.iter().map(|(path, bytes)| {
        (
            RepoPath::parse(path).unwrap(),
            FileEntry::Regular {
                content: repo.put_blob(bytes).unwrap(),
                executable: false,
            },
        )
    }))
    .unwrap();
    let id = repo.put_manifest(&manifest).unwrap();
    let outcome = repo
        .compare_and_advance(&op(&format!("publish-{expected}")), version(expected), id)
        .unwrap();
    assert!(matches!(outcome, AdvanceOutcome::Accepted { .. }));
}

/// Version 0 (empty), then two publishes that change, add and keep files, so the
/// trees and changesets differ between versions.
fn fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    let mut repo = Repository::init(dir.path()).unwrap();
    publish(
        &mut repo,
        0,
        &[
            ("a.txt", b"alpha alpha alpha alpha"),
            ("dir/b.txt", b"beta beta beta beta"),
        ],
    );
    publish(
        &mut repo,
        1,
        &[
            ("a.txt", b"alpha, edited in version two"),
            ("dir/b.txt", b"beta beta beta beta"),
            ("c.txt", b"gamma gamma"),
        ],
    );
    dir
}

/// Reads every version's files through the public API. Any corruption on the way
/// surfaces as the error.
fn read_everything(root: &Path) -> javelin::Result<Snapshot> {
    let repo = Repository::open(root)?;
    let mut out = Vec::new();
    for world in repo.history()? {
        let manifest = repo.manifest_at(world.number)?;
        let mut files = Vec::new();
        for (path, entry) in manifest.entries() {
            if let FileEntry::Regular { content, .. } = entry {
                files.push((path.as_str().to_owned(), repo.read_blob(*content)?));
            }
        }
        out.push((world.number.get(), files));
    }
    Ok(out)
}

fn original(root: &Path) -> Snapshot {
    read_everything(root).unwrap()
}

fn raw(root: &Path) -> Connection {
    Connection::open(root.join(".javelin/repository.sqlite3")).unwrap()
}

/// Mutations go through raw SQL, so the immutability triggers come off first.
fn unlock(conn: &Connection) {
    let triggers: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='trigger'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for name in triggers {
        conn.execute_batch(&format!("DROP TRIGGER \"{name}\""))
            .unwrap();
    }
}

/// The full-graph check and the content read must both refuse the mutated repository,
/// with the error code `code`.
fn assert_detected(root: &Path, code: &str, case: &str) {
    let check = Repository::open(root).and_then(|repo| repo.check());
    assert_eq!(
        check.as_ref().err().map(javelin::Error::code),
        Some(code),
        "{case}: check() must refuse it"
    );
    let read = read_everything(root);
    assert_eq!(
        read.as_ref().err().map(javelin::Error::code),
        Some(code),
        "{case}: reading must refuse it"
    );
}

/// Object rows of `kind` as (id, bytes).
fn objects_of_kind(conn: &Connection, kind: i64) -> Vec<(String, Vec<u8>)> {
    conn.prepare("SELECT id, bytes FROM objects WHERE kind=?1 ORDER BY id")
        .unwrap()
        .query_map([kind], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Flips every byte of every object of `kind`, one at a time. `check()` hashes every
/// stored object, so it must refuse each mutation with `code`, referenced or not. A read
/// goes only through reachable objects: it may succeed for an unreferenced one, but it
/// must then return the original content.
fn flip_every_byte_of_kind(kind: i64, code: &str, case: &str) {
    let dir = fixture();
    let expected = original(dir.path());
    let conn = raw(dir.path());
    unlock(&conn);
    let objects = objects_of_kind(&conn, kind);
    assert!(
        !objects.is_empty(),
        "{case}: fixture has no objects of kind {kind}"
    );
    for (id, bytes) in objects {
        for offset in 0..bytes.len() {
            let mut flipped = bytes.clone();
            flipped[offset] ^= 0xFF;
            conn.execute("UPDATE objects SET bytes=?1 WHERE id=?2", (&flipped, &id))
                .unwrap();
            let check = Repository::open(dir.path()).and_then(|repo| repo.check());
            assert_eq!(
                check.as_ref().err().map(javelin::Error::code),
                Some(code),
                "{case}: object {id} byte {offset}: check() did not refuse it"
            );
            match read_everything(dir.path()) {
                Ok(content) => assert_eq!(
                    content, expected,
                    "{case}: object {id} byte {offset} read back wrong content"
                ),
                Err(error) => assert_eq!(error.code(), code, "{case}: {error}"),
            }
            conn.execute("UPDATE objects SET bytes=?1 WHERE id=?2", (&bytes, &id))
                .unwrap();
        }
    }
    assert_eq!(
        original(dir.path()),
        expected,
        "{case}: restoring bytes must restore content"
    );
}

#[test]
fn every_flipped_blob_byte_is_detected_as_corrupt_object() {
    flip_every_byte_of_kind(1, "corrupt_object", "blob");
}

#[test]
fn every_flipped_manifest_byte_is_detected_as_corrupt_object() {
    flip_every_byte_of_kind(2, "corrupt_object", "manifest");
}

#[test]
fn every_flipped_tree_byte_is_detected_as_corrupt_object() {
    flip_every_byte_of_kind(4, "corrupt_object", "tree");
}

#[test]
fn a_blob_whose_bytes_are_replaced_by_another_valid_blob_is_detected() {
    // A different blob with a valid object row is still wrong content for its id.
    let dir = fixture();
    let conn = raw(dir.path());
    unlock(&conn);
    let (id, _) = objects_of_kind(&conn, 1).remove(0);
    let other = b"substituted content".to_vec();
    conn.execute("UPDATE objects SET bytes=?1 WHERE id=?2", (&other, &id))
        .unwrap();
    assert_detected(dir.path(), "corrupt_object", "substituted blob");
}

#[test]
fn an_altered_version_row_is_detected() {
    let cases: [(&str, &str); 4] = [
        // The version id is a digest of the chain; a changed id must not be accepted.
        (
            "UPDATE versions SET id=lower(hex(zeroblob(32))) WHERE number=2",
            "version id",
        ),
        // Pointing version 2 at version 1's tree would serve the old files as current.
        (
            "UPDATE versions SET tree=(SELECT tree FROM versions WHERE number=1) WHERE number=2",
            "version tree",
        ),
        (
            "UPDATE changes SET content=(SELECT content FROM changes WHERE version=1 AND path='a.txt') WHERE version=2 AND path='a.txt'",
            "changeset content",
        ),
        (
            "UPDATE changes SET path='renamed.txt' WHERE version=2 AND path='c.txt'",
            "changeset path",
        ),
    ];
    for (sql, case) in cases {
        let dir = fixture();
        let conn = raw(dir.path());
        unlock(&conn);
        conn.execute_batch(sql).unwrap();
        drop(conn);
        // Both the full check and every read start from history(), which re-derives each
        // World, so each altered row is refused on both paths.
        assert_detected(dir.path(), "corrupt_repository", case);
    }
}

#[test]
fn a_damaged_schema_is_corrupt_repository_not_storage_error() {
    let dir = fixture();
    let conn = raw(dir.path());
    // A bit flip in the stored schema: the column the queries name no longer exists.
    conn.execute_batch(
        "PRAGMA writable_schema=ON;
         UPDATE sqlite_master SET sql=replace(sql,'tree TEXT','uree TEXT') WHERE name='versions';
         PRAGMA writable_schema=OFF;",
    )
    .unwrap();
    drop(conn);
    assert_detected(dir.path(), "corrupt_repository", "damaged schema");
}

#[test]
fn a_stored_value_of_the_wrong_type_is_corrupt_repository_not_storage_error() {
    let dir = fixture();
    let conn = raw(dir.path());
    unlock(&conn);
    // Dropping STRICT lets a BLOB sit where the schema promises TEXT, which is the stored
    // type a bit flip in the record header produces.
    conn.execute_batch(
        "PRAGMA writable_schema=ON;
         UPDATE sqlite_master SET sql=replace(sql,'STRICT','') WHERE name='versions';
         PRAGMA writable_schema=OFF;",
    )
    .unwrap();
    drop(conn);
    let conn = raw(dir.path());
    conn.execute("UPDATE versions SET id=CAST(id AS BLOB) WHERE number=2", [])
        .unwrap();
    drop(conn);
    assert_detected(dir.path(), "corrupt_repository", "wrong stored type");
}

/// Every whole-file byte flip in the SQLite database is either detected or leaves the
/// content exactly as it was. The first 100 bytes (header) are always covered; the rest
/// is sampled on a stride so the sweep stays fast.
#[test]
fn every_sampled_sqlite_file_byte_flip_fails_or_returns_original_content() {
    let dir = fixture();
    let expected = original(dir.path());
    let db = dir.path().join(".javelin/repository.sqlite3");
    let pristine = fs::read(&db).unwrap();
    let stride = pristine.len() / 600 + 1;
    let offsets: Vec<usize> = (0..pristine.len())
        .filter(|offset| *offset < 100 || offset % stride == 0)
        .collect();
    let mut detected = 0;
    for offset in &offsets {
        let mut flipped = pristine.clone();
        flipped[*offset] ^= 0xFF;
        fs::write(&db, &flipped).unwrap();
        match read_everything(dir.path()) {
            Ok(content) => assert_eq!(
                content, expected,
                "byte {offset} flipped: content read back differs without an error"
            ),
            Err(error) => {
                // A damaged database is corruption, never a retryable storage failure.
                assert!(
                    matches!(
                        error.code(),
                        "corrupt_repository"
                            | "corrupt_object"
                            | "missing_object"
                            | "unsupported_format"
                    ),
                    "byte {offset}: {error}"
                );
                detected += 1;
            }
        }
    }
    fs::write(&db, &pristine).unwrap();
    assert!(
        detected > 0,
        "no flipped byte was detected in {} offsets",
        offsets.len()
    );
    assert_eq!(original(dir.path()), expected);
}
