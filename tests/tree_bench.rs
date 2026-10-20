//! Derived tree costs at repository scale. Ignored by default. Run in release mode:
//!
//! ```text
//! cargo test --release --test tree_bench -- --ignored --nocapture
//! JAVELIN_BENCH_FILES=100000,1000000 JAVELIN_BENCH_SAMPLES=50 cargo test ...
//! ```
//!
//! For each repository size it builds the tree once, then measures applying 1-file
//! and 100-file changesets to it: median time and mean bytes of new chunks (what a
//! Publish writes). It also measures one-entry edits in a 50k-entry directory and
//! snapshot reads. Chunks live in memory; `publish_bench` covers SQLite end to end.
use std::time::{Duration, Instant};

use javelin::{
    FileEntry, ObjectId, RepoPath, SparseChanges,
    tree::{MemoryStore, Trees},
};

/// 1,000 files per directory under `src/m<k>/`, like `publish_bench`.
fn path(i: usize) -> RepoPath {
    RepoPath::parse(&format!("src/m{}/f{i}.rs", i / 1000)).unwrap()
}
fn blob(n: u64) -> FileEntry {
    FileEntry::Regular {
        content: ObjectId::for_blob(&n.to_le_bytes()),
        executable: false,
    }
}

struct Rng(u64);
impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// Median time and mean bytes written of applying `make(sample)` to `root`.
fn measure(
    trees: &Trees<MemoryStore>,
    root: ObjectId,
    samples: usize,
    mut make: impl FnMut(usize) -> SparseChanges,
) -> (f64, usize) {
    let (mut times, mut bytes) = (Vec::new(), 0);
    for sample in 0..samples {
        let changes = make(sample);
        // A cold decoded cache, as in a fresh Publish; the store keeps every chunk.
        let fresh = Trees::new(Shared(trees.store()));
        let before = trees.store().bytes();
        let start = Instant::now();
        fresh.apply(root, &changes).unwrap();
        times.push(start.elapsed());
        bytes += trees.store().bytes() - before;
    }
    times.sort();
    (ms(times[samples / 2]), bytes / samples)
}

/// Shares one store between cache-cold tree handles.
struct Shared<'a>(&'a MemoryStore);
impl javelin::tree::Store for Shared<'_> {
    fn load(&self, id: ObjectId) -> javelin::Result<Vec<u8>> {
        self.0.load(id)
    }
    fn save(&self, id: ObjectId, bytes: &[u8]) -> javelin::Result<()> {
        self.0.save(id, bytes)
    }
}

fn run(files: usize, samples: usize) {
    let trees = Trees::new(MemoryStore::default());
    let start = Instant::now();
    let snapshot: Vec<_> = (0..files).map(|i| (path(i), blob(i as u64))).collect();
    let root = trees.build(snapshot.iter().map(|(p, e)| (p, e))).unwrap();
    let build = start.elapsed();
    let (tree_bytes, chunks) = (trees.store().bytes(), trees.store().len());
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let one = measure(&trees, root, samples, |s| {
        let i = rng.below(files);
        SparseChanges::new([(path(i), Some(blob((files + s) as u64)))]).unwrap()
    });
    let hundred = measure(&trees, root, samples, |s| {
        // 100 distinct files spread over the repository.
        let start = rng.below(files);
        SparseChanges::new((0..100).map(|k| {
            let i = (start + k * (files / 100)) % files;
            (path(i), Some(blob((files * (k + 2) + s) as u64)))
        }))
        .unwrap()
    });
    let add_delete = measure(&trees, root, samples, |s| {
        let i = rng.below(files);
        SparseChanges::new([
            (path(i), None),
            (
                RepoPath::parse(&format!("src/m{}/new{s}.rs", i / 1000)).unwrap(),
                Some(blob(s as u64)),
            ),
        ])
        .unwrap()
    });
    let fresh = Trees::new(Shared(trees.store()));
    let start = Instant::now();
    let entries = fresh.entries(root).unwrap();
    let walk = start.elapsed();
    assert_eq!(entries.len(), files);
    println!(
        "{}",
        serde_json::json!({
            "files": files,
            "build_s": build.as_secs_f64(),
            "tree_bytes": tree_bytes,
            "chunks": chunks,
            "one_file_median_ms": one.0,
            "one_file_written_bytes": one.1,
            "hundred_files_median_ms": hundred.0,
            "hundred_files_written_bytes": hundred.1,
            "add_and_delete_median_ms": add_delete.0,
            "add_and_delete_written_bytes": add_delete.1,
            "snapshot_walk_ms": ms(walk),
        })
    );
}

fn wide(samples: usize) {
    let trees = Trees::new(MemoryStore::default());
    let name = |n: usize| RepoPath::parse(&format!("dir/file-{n:06}.rs")).unwrap();
    let snapshot: Vec<_> = (0..50_000).map(|n| (name(n * 2), blob(n as u64))).collect();
    let root = trees.build(snapshot.iter().map(|(p, e)| (p, e))).unwrap();
    let listing = trees.store().bytes();
    let mut rng = Rng(42);
    let edit = measure(&trees, root, samples, |s| {
        SparseChanges::new([(
            name(rng.below(50_000) * 2),
            Some(blob(1_000_000 + s as u64)),
        )])
        .unwrap()
    });
    let insert = measure(&trees, root, samples, |_| {
        SparseChanges::new([(name(rng.below(50_000) * 2 + 1), Some(blob(7)))]).unwrap()
    });
    let delete = measure(&trees, root, samples, |_| {
        SparseChanges::new([(name(rng.below(50_000) * 2), None)]).unwrap()
    });
    println!(
        "{}",
        serde_json::json!({
            "directory_entries": 50_000,
            "listing_bytes": listing,
            "edit_median_ms": edit.0,
            "edit_written_bytes": edit.1,
            "insert_median_ms": insert.0,
            "insert_written_bytes": insert.1,
            "delete_median_ms": delete.0,
            "delete_written_bytes": delete.1,
        })
    );
}

#[test]
#[ignore = "benchmark; run explicitly in release mode"]
fn tree_derivation_cost() {
    let files = std::env::var("JAVELIN_BENCH_FILES").unwrap_or_else(|_| "100000,1000000".into());
    let samples = std::env::var("JAVELIN_BENCH_SAMPLES")
        .map(|s| s.parse().unwrap())
        .unwrap_or(50);
    for n in files.split(',') {
        run(n.parse().unwrap(), samples);
    }
    wide(samples);
}
