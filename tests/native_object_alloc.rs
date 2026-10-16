//! One test keeps operation measurements separate from concurrent test fixtures.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

use javelin::{Error, FileEntry, Manifest, ObjectId, RepoPath, Repository};
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

fn small(a: Allocation, limit: usize) {
    assert!(a.total < limit, "total Rust allocation {a:?}");
    assert!(a.largest < limit, "oversized Rust allocation {a:?}");
    assert!(a.peak < limit as isize, "Rust net live peak {a:?}");
}

fn regular(content: ObjectId) -> FileEntry {
    FileEntry::Regular {
        content,
        executable: false,
    }
}

fn report_process_peak() {
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        assert_eq!(
            unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
            0
        );
        let usage = unsafe { usage.assume_init() };
        let unit = if cfg!(target_os = "macos") {
            "bytes"
        } else {
            "KiB"
        };
        println!(
            "RSS cumulative_process_peak={} unit={unit} includes_fixture_setup=true",
            usage.ru_maxrss
        );
    }
    #[cfg(not(unix))]
    println!(
        "RSS unavailable_on_this_platform; Rust and SQLite C counters are not total process memory"
    );
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
    assert!(
        a.largest >= bytes.len(),
        "allocator must detect the old defect"
    );
    drop(bytes);

    // The public caller's request holds only kind and a raw 32-byte digest.
    let request = *id.as_bytes();
    let (present, a) = measure("blob_32mib_availability_32byte_id", || {
        repo.has_blob(ObjectId::from_bytes(request)).unwrap()
    });
    assert!(present);
    small(a, 4096);
    let (result, a) = measure("blob_32mib_limit_8mib", || {
        repo.read_blob_bounded(id, 8 * 1024 * 1024)
    });
    assert!(
        matches!(result, Err(Error::ObjectTooLarge { size, limit, .. }) if size == 32 * 1024 * 1024 && limit == 8 * 1024 * 1024)
    );
    small(a, 4096);

    let accepted = vec![0x5a; 8 * 1024 * 1024];
    let accepted_id = repo.put_blob(&accepted).unwrap();
    drop(accepted);
    let (bytes, a) = measure("blob_8mib_exact_limit", || {
        repo.read_blob_bounded(accepted_id, 8 * 1024 * 1024)
            .unwrap()
    });
    assert_eq!(bytes.len(), 8 * 1024 * 1024);
    assert!(bytes.iter().all(|b| *b == 0x5a));
    assert_eq!(a.largest, bytes.len());
    assert!(a.total < bytes.len() + 4096);
    drop(bytes);

    // Eight repeated references represent 256 MiB of graph-validation reads.
    // These public object-only operations must never visit that graph.
    let graph = Manifest::new(
        (0..8).map(|i| (RepoPath::parse(&format!("file-{i}")).unwrap(), regular(id))),
    )
    .unwrap();
    let graph_id = repo.put_manifest(&graph).unwrap();
    let graph_bytes = graph.canonical_bytes();
    let (_, a) = measure("legacy_manifest_8refs_32mib", || {
        repo.manifest_bytes(graph_id).unwrap()
    });
    assert!(a.total >= 8 * 32 * 1024 * 1024);
    assert!(a.largest >= 32 * 1024 * 1024);
    let (present, a) = measure("manifest_8refs_32mib_availability", || {
        repo.has_manifest(graph_id).unwrap()
    });
    assert!(present);
    small(a, 4096);
    let (bytes, a) = measure("manifest_8refs_32mib_exact_limit", || {
        repo.manifest_bytes_bounded(graph_id, graph_bytes.len())
            .unwrap()
    });
    assert_eq!(bytes, graph_bytes);
    small(a, 64 * 1024);
    let (result, a) = measure("manifest_8refs_32mib_under_limit", || {
        repo.manifest_bytes_bounded(graph_id, graph_bytes.len() - 1)
    });
    assert!(matches!(result, Err(Error::ObjectTooLarge { .. })));
    small(a, 4096);

    let huge = Manifest::new([(
        RepoPath::parse(&"x".repeat(32 * 1024 * 1024)).unwrap(),
        regular(accepted_id),
    )])
    .unwrap();
    let huge_id = repo.put_manifest(&huge).unwrap();
    drop(huge);
    let (present, a) = measure("manifest_over_32mib_availability", || {
        repo.has_manifest(huge_id).unwrap()
    });
    assert!(present);
    small(a, 4096);
    let (result, a) = measure("manifest_over_32mib_limit_8mib", || {
        repo.manifest_bytes_bounded(huge_id, 8 * 1024 * 1024)
    });
    assert!(matches!(result, Err(Error::ObjectTooLarge { size, .. }) if size > 32 * 1024 * 1024));
    small(a, 4096);

    let capped = Manifest::new([(
        RepoPath::parse(&"m".repeat(1024 * 1024)).unwrap(),
        regular(id),
    )])
    .unwrap();
    let capped_id = repo.put_manifest(&capped).unwrap();
    let encoded = capped.canonical_bytes();
    drop(capped);
    let (bytes, a) = measure("manifest_1mib_path_exact_limit_32mib_ref", || {
        repo.manifest_bytes_bounded(capped_id, encoded.len())
            .unwrap()
    });
    assert_eq!(bytes, encoded);
    // Canonical decoding has parser/map/re-encoding overhead beyond output size.
    small(a, 16 * encoded.len());
    report_process_peak();
}
