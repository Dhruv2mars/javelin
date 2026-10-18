//! Measures the real daemonless capture path (docs/workspace-approach.md, "Capture").
//!
//! cargo run --release --example capture_bench -- --dir SCRATCH [--files 100000] [--reps 5]
//!
//! Unlike tools/layer-bench's metadata-only rescan, this runs `capture_directory`: the
//! stat cache, the racy rule, stable reads, SHA-256 hashing, blob storage and the
//! checkpoint commit. Edits are single-byte appends plus same-size rewrites.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use javelin::{
    LayerId, LayerState, ObjectId, OperationId, Repository, VersionNumber,
    capture::{RACY_MARGIN_NS, StatCache, scan},
};
use sha2::{Digest, Sha256};

/// The same deterministic repository-shaped tree as tools/layer-bench.
fn fixture(root: &Path, files: usize, mut seed: u64) -> (Vec<PathBuf>, u64) {
    let mut next = || {
        seed ^= seed >> 12;
        seed ^= seed << 25;
        seed ^= seed >> 27;
        seed.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    let unit = |next: &mut dyn FnMut() -> u64| ((next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64;
    let (mut dirs, mut paths, mut bytes, mut buf) = (vec![root.to_path_buf()], vec![], 0, vec![]);
    for i in 0..files {
        if i % 20 == 0 {
            let recent = &dirs[dirs.len().saturating_sub(50)..];
            let parent = recent[next() as usize % recent.len()].clone();
            let depth = parent.strip_prefix(root).unwrap().components().count();
            let dir = if depth < 4 {
                parent
            } else {
                root.to_path_buf()
            }
            .join(format!("d{}", i / 20));
            fs::create_dir_all(&dir).unwrap();
            dirs.push(dir);
        }
        let (a, b) = (unit(&mut next), unit(&mut next));
        let z = (-2.0 * a.ln()).sqrt() * (2.0 * std::f64::consts::PI * b).cos();
        let size = ((8.0 + 1.3 * z).exp() as usize).min(2_000_000);
        buf.clear();
        while buf.len() < size {
            buf.extend_from_slice(&next().to_le_bytes());
        }
        buf.truncate(size);
        let path = dirs.last().unwrap().join(format!("f{i}.txt"));
        fs::write(&path, &buf).unwrap();
        paths.push(path);
        bytes += size as u64;
    }
    (paths, bytes)
}

/// tools/layer-bench's metadata-only walk, for a same-machine baseline.
fn metadata_walk(root: &Path, count: &mut usize) {
    for e in fs::read_dir(root).unwrap() {
        let e = e.unwrap();
        let m = e.metadata().unwrap();
        if m.is_dir() {
            metadata_walk(&e.path(), count);
        } else {
            std::hint::black_box((m.len(), m.modified().unwrap()));
            *count += 1;
        }
    }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn settle() {
    std::thread::sleep(Duration::from_nanos(RACY_MARGIN_NS as u64) + Duration::from_millis(500));
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .map(|i| args[i + 1].clone())
    };
    let dir = PathBuf::from(flag("--dir").expect("--dir SCRATCH"));
    let files: usize = flag("--files").map_or(100_000, |n| n.parse().unwrap());
    let reps: usize = flag("--reps").map_or(5, |n| n.parse().unwrap());
    let _ = fs::remove_dir_all(&dir);
    let (store, root) = (dir.join("repo"), dir.join("layer"));
    fs::create_dir_all(&store).unwrap();
    let mut repo = Repository::init(&store).unwrap();
    let (paths, bytes) = fixture(&root, files, files as u64);
    println!("fixture: {files} files, {:.0} MB", bytes as f64 / 1e6);

    // Seed World 1 with the tree through a first capture, then open a Layer over it.
    let seed = LayerId::parse("seed").unwrap();
    repo.create_layer(
        &seed,
        &repo.layer_base(VersionNumber::new(0).unwrap()).unwrap(),
    )
    .unwrap();
    let t = Instant::now();
    let first = repo.capture_directory(&seed, &root).unwrap();
    println!(
        "first capture (hash + store {files} blobs + commit): {:.0} ms",
        ms(t)
    );
    let LayerState::Active { head } = repo.layer_state(&seed).unwrap() else {
        panic!()
    };
    assert_eq!(head.revision(), 1, "{:?}", first.update);
    let manifest = repo.checkpoint_manifest(head.checkpoint()).unwrap();
    let manifest = repo.put_manifest(&manifest).unwrap();
    let world = OperationId::parse("world-1").unwrap();
    repo.compare_and_advance(&world, VersionNumber::new(0).unwrap(), manifest)
        .unwrap();
    let work = LayerId::parse("work").unwrap();
    repo.create_layer(
        &work,
        &repo.layer_base(VersionNumber::new(1).unwrap()).unwrap(),
    )
    .unwrap();

    let t = Instant::now();
    let cold = repo.capture_directory(&work, &root).unwrap();
    println!(
        "cold rebuild, no cache (hash all, no commit): {:.0} ms, hashed {}",
        ms(t),
        cold.hashed
    );
    assert!(cold.update.is_none() && cold.rebuilt);
    settle();
    let t = Instant::now();
    let clean = repo.capture_directory(&work, &root).unwrap();
    println!(
        "clean rescan (no edits): {:.0} ms, hashed {}",
        ms(t),
        clean.hashed
    );

    let mut walks = vec![];
    for _ in 0..reps {
        let (t, mut count) = (Instant::now(), 0);
        metadata_walk(&root, &mut count);
        walks.push(ms(t));
        assert_eq!(count, files);
    }
    println!(
        "metadata-only walk, layer-bench baseline (median of {reps}): {:.0} ms",
        median(&mut walks)
    );

    let cache_path = store
        .join(".javelin/capture")
        .join(ObjectId::from_bytes(Sha256::digest(b"work").into()).to_string());
    let (mut scans, mut totals, mut hashed) = (vec![], vec![], vec![]);
    for rep in 0..reps {
        let LayerState::Active { head } = repo.layer_state(&work).unwrap() else {
            panic!()
        };
        for (i, path) in paths[rep * 10..rep * 10 + 10].iter().enumerate() {
            if i.is_multiple_of(2) {
                fs::OpenOptions::new()
                    .append(true)
                    .open(path)
                    .unwrap()
                    .write_all(b"x")
                    .unwrap();
            } else {
                // Same-size rewrite: flip the first byte in place.
                let mut bytes = fs::read(path).unwrap();
                bytes[0] ^= 0xff;
                fs::write(path, bytes).unwrap();
            }
        }
        // Scan alone (stat walk, racy rule, stable reads, hashing), cache left unsaved.
        let prior = repo.checkpoint_manifest(head.checkpoint()).unwrap();
        let mut cache = StatCache::load(&cache_path);
        assert!(!cache.rebuilt());
        let t = Instant::now();
        let only = scan(&root, &mut cache, &prior, &mut |b| {
            Ok(ObjectId::for_blob(b))
        })
        .unwrap();
        scans.push(ms(t));
        // The whole capture: scan, blobs, cache save and the checkpoint commit.
        let t = Instant::now();
        let capture = repo.capture_directory(&work, &root).unwrap();
        totals.push(ms(t));
        assert!(capture.update.is_some() && capture.uncertain.is_empty());
        assert_eq!(capture.changes.entries().len(), rep * 10 + 10);
        hashed.push(only.hashed as f64);
        settle();
    }
    let (scan_ms, total_ms) = (median(&mut scans), median(&mut totals));
    println!(
        "rescan after 10 edits (median of {reps}): scan {scan_ms:.0} ms, hashed {:.0}; full capture {total_ms:.0} ms; commit ~{:.0} ms",
        median(&mut hashed),
        total_ms - scan_ms
    );
    let _ = fs::remove_dir_all(&dir);
}
