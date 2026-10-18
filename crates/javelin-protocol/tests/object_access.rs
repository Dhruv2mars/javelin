//! Native object-only access through actual protocol callers. Allocation counters
//! cover this thread's Rust allocator requests, excluding SQLite C memory and RSS.

mod support;

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    io::Cursor,
};

use javelin::{FileEntry, Manifest, ObjectId, OperationId, RepoPath, VersionNumber};
use javelin_protocol::{Kind, Message, codec, push};
use support::*;

#[derive(Clone, Copy, Debug, Default)]
struct Allocation {
    total: usize,
    largest: usize,
}

thread_local! {
    static COUNT: Cell<Option<Allocation>> = const { Cell::new(None) };
}

fn record(size: usize) {
    COUNT.with(|count| {
        if let Some(mut a) = count.get() {
            a.total += size;
            a.largest = a.largest.max(size);
            count.set(Some(a));
        }
    });
}

struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(p, layout, size) }
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn measure<T>(name: &str, f: impl FnOnce() -> T) -> (T, Allocation) {
    COUNT.with(|c| c.set(Some(Allocation::default())));
    let result = f();
    let a = COUNT.with(|c| c.replace(None).unwrap());
    println!(
        "MEMORY {name} rust_total={} rust_largest={} excludes_sqlite_c=true not_peak_rss=true",
        a.total, a.largest
    );
    (result, a)
}

fn small(a: Allocation) {
    assert!(a.total < 1 << 20, "unexpected Rust allocation: {a:?}");
    assert!(a.largest < 1 << 20, "payload-sized allocation: {a:?}");
}

fn regular(content: ObjectId) -> FileEntry {
    FileEntry::Regular {
        content,
        executable: false,
    }
}

fn want(kind: Kind, id: ObjectId) -> Vec<u8> {
    let mut input = hello();
    input.extend(msg(&Message::Want(vec![(kind, id)])));
    assert_eq!(input.len(), 55);
    input
}

fn publish(id: ObjectId) -> Vec<u8> {
    let mut input = hello();
    input.extend(msg(&Message::Publish {
        operation: OperationId::parse("graph-check").unwrap(),
        expected: VersionNumber::new(0).unwrap(),
        tree: id,
    }));
    input
}

#[test]
fn small_want_and_capped_sender_avoid_large_stored_payload_allocations() {
    let (_dir, mut repo) = repo();
    let data = vec![42; 32 << 20];
    let blob = repo.put_blob(&data).unwrap();
    drop(data);

    let (bytes, control) = measure("positive_control_read_32mib", || {
        repo.read_blob(blob).unwrap()
    });
    assert!(
        control.largest >= 32 << 20,
        "allocator control: {control:?}"
    );
    drop(bytes);
    let input = want(Kind::Blob, blob);
    let ((result, replies), a) = measure("want_55bytes_blob_32mib", || session(&mut repo, &input));
    result.unwrap();
    assert_eq!(replies.last(), Some(&Message::Missing(vec![])));
    small(a);

    // A tree naming the 32 MiB blob eight times: probing it never reads the blob.
    let manifest = Manifest::new((0..8).map(|i| {
        (
            RepoPath::parse(&format!("file-{i}")).unwrap(),
            regular(blob),
        )
    }))
    .unwrap();
    let root = repo.put_snapshot(&manifest).unwrap();
    let input = want(Kind::Tree, root);
    let ((result, replies), a) = measure("want_55bytes_tree_8refs_32mib", || {
        session(&mut repo, &input)
    });
    result.unwrap();
    assert_eq!(replies.last(), Some(&Message::Missing(vec![])));
    small(a);

    // Force the real sender down the missing-blob path. It must refuse at the cap
    // without a whole-payload blob allocation.
    let mut replies = msg(&Message::HelloOk { version: 2 });
    replies.extend(msg(&Message::Missing(vec![(Kind::Tree, root)])));
    replies.extend(msg(&Message::Missing(vec![(Kind::Blob, blob)])));
    let mut output = Vec::new();
    let (result, a) = measure("sender_missing_32mib_blob", || {
        push(
            Cursor::new(replies),
            &mut output,
            &repo,
            root,
            &OperationId::parse("oversize").unwrap(),
            VersionNumber::new(0).unwrap(),
        )
    });
    assert_eq!(result.unwrap_err().code, "object_too_large");
    small(a);
    let mut output = Cursor::new(output);
    assert!(matches!(
        codec::read_frame(&mut output).unwrap(),
        Some(Message::Hello { .. })
    ));
    for expected in [(Kind::Tree, root), (Kind::Blob, blob)] {
        assert_eq!(
            codec::read_frame(&mut output).unwrap(),
            Some(Message::Want(vec![expected]))
        );
    }
    assert_eq!(codec::read_frame(&mut output).unwrap(), None);
    assert_eq!(repo.history().unwrap().len(), 1);
}

#[test]
fn tree_availability_does_not_certify_its_contents() {
    let (dir, mut repo) = repo();
    let blob = repo.put_blob(b"referenced").unwrap();
    let manifest = Manifest::new([(RepoPath::parse("file").unwrap(), regular(blob))]).unwrap();
    let root = repo.put_snapshot(&manifest).unwrap();
    let conn = rusqlite::Connection::open(dir.path().join(".javelin/repository.sqlite3")).unwrap();
    // Fault injection only: normal writers cannot bypass immutable storage.
    conn.execute_batch("DROP TRIGGER objects_no_DELETE;")
        .unwrap();
    conn.execute("DELETE FROM objects WHERE id=?1", [blob.to_string()])
        .unwrap();
    assert!(repo.has_tree(root).unwrap());
    let (result, replies) = session(&mut repo, &want(Kind::Tree, root));
    result.unwrap();
    assert_eq!(replies.last(), Some(&Message::Missing(vec![])));
    // Publish validates the changed contents and refuses.
    refused(&mut repo, &publish(root), "missing_object");
}

#[test]
fn want_refuses_damaged_bytes_and_wrong_kinds_without_advancing() {
    let (dir, mut repo) = repo();
    let (root, blobs, _) = fixture(&mut repo);
    refused(&mut repo, &want(Kind::Blob, root), "wrong_object_kind");
    refused(
        &mut repo,
        &want(Kind::Tree, blobs[0].0),
        "wrong_object_kind",
    );
    let conn = rusqlite::Connection::open(dir.path().join(".javelin/repository.sqlite3")).unwrap();
    conn.execute_batch("DROP TRIGGER objects_no_UPDATE;")
        .unwrap();
    for (kind, target) in [(Kind::Blob, blobs[0].0), (Kind::Tree, root)] {
        conn.execute(
            "UPDATE objects SET bytes=?1 WHERE id=?2",
            rusqlite::params![b"damaged".as_slice(), target.to_string()],
        )
        .unwrap();
        refused(&mut repo, &want(kind, target), "corrupt_object");
    }
}
