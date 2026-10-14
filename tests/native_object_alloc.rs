//! One test keeps operation measurements separate from concurrent test fixtures.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

use javelin::Repository;
use tempfile::TempDir;

#[derive(Clone, Copy, Debug, Default)]
struct Allocation {
    total: usize,
    largest: usize,
    live: isize,
    peak: isize,
}

thread_local! {
    static COUNT: Cell<Option<Allocation>> = const { Cell::new(None) };
}

fn record(allocated: usize, freed: usize) {
    let _ = COUNT.try_with(|count| {
        if let Some(mut a) = count.get() {
            a.total += allocated;
            a.largest = a.largest.max(allocated);
            a.live += allocated as isize - freed as isize;
            a.peak = a.peak.max(a.live);
            count.set(Some(a));
        }
    });
}

struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            record(layout.size(), 0);
        }
        ptr
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            record(layout.size(), 0);
        }
        ptr
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !ptr.is_null() {
            record(new_size, layout.size());
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        record(0, layout.size());
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn measure<T>(name: &str, f: impl FnOnce() -> T) -> (T, Allocation) {
    // SQLite uses its C allocator, outside Rust's GlobalAlloc. No other test in
    // this executable resets its process-wide counters.
    let sqlite_before = unsafe { rusqlite::ffi::sqlite3_memory_used() };
    unsafe { rusqlite::ffi::sqlite3_memory_highwater(1) };
    COUNT.with(|count| count.set(Some(Allocation::default())));
    let result = f();
    let a = COUNT.with(|count| count.replace(None).unwrap());
    let sqlite_peak = unsafe { rusqlite::ffi::sqlite3_memory_highwater(0) };
    println!(
        "MEMORY {name} rust_total={} rust_largest={} rust_net_peak={} sqlite_before={sqlite_before} sqlite_peak={sqlite_peak}",
        a.total, a.largest, a.peak
    );
    (result, a)
}

#[test]
fn stored_payload_allocation_evidence() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let data = vec![42; 32 * 1024 * 1024];
    let id = repo.put_blob(&data).unwrap();
    drop(data);
    let (bytes, a) = measure("legacy_blob_32mib", || repo.read_blob(id).unwrap());
    assert_eq!(bytes.len(), 32 * 1024 * 1024);
    assert!(a.largest >= bytes.len(), "allocator must detect the old defect");
}
