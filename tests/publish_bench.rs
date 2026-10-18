//! One-file Publish cost against repository size and World history length.
//! Ignored by default. Run in release mode:
//!
//! ```text
//! cargo test --release --test publish_bench -- --ignored --nocapture
//! JAVELIN_BENCH_CASES=1000x10,100000x1000 JAVELIN_BENCH_SAMPLES=20 cargo test ...
//! ```
//!
//! Each case builds World 1 with `files` paths, then `history - 1` one-file
//! publishes, then measures `samples` more one-file publishes. Blob storage is
//! outside the measured step. It reports the median `publish_layer` latency, the
//! mean database growth, and the mean bytes of 4 KiB database pages the publish
//! rewrote (journal writes are not counted). It uses only APIs that exist both
//! before and after World changesets, so it runs unchanged on either.
use std::time::{Duration, Instant};

use javelin::{
    FileEntry, LayerId, LayerState, LayerUpdate, OperationId, PublishAttribution, PublishOutcome,
    RepoPath, Repository, SparseChanges, VersionNumber,
};
use tempfile::TempDir;

const PAGE: usize = 4096;

fn path(i: usize) -> RepoPath {
    RepoPath::parse(&format!("src/m{}/f{i}.rs", i / 1000)).unwrap()
}

struct Bench {
    repo: Repository,
    database: std::path::PathBuf,
    head: u64,
    step: usize,
}

impl Bench {
    /// Captures `changes` in a new Layer on the current head; returns its head.
    fn prepare(&mut self, changes: SparseChanges) -> javelin::LayerHead {
        self.step += 1;
        let layer = LayerId::parse(&format!("layer-{}", self.step)).unwrap();
        let base = self
            .repo
            .layer_base(VersionNumber::new(self.head).unwrap())
            .unwrap();
        let empty = self.repo.create_layer(&layer, &base).unwrap();
        let operation = OperationId::parse(&format!("capture-{}", self.step)).unwrap();
        match self
            .repo
            .capture_layer(&operation, &empty, &changes)
            .unwrap()
        {
            LayerUpdate::Applied {
                state: LayerState::Active { head },
            } => head,
            other => panic!("{other:?}"),
        }
    }

    fn publish(&mut self, head: &javelin::LayerHead) {
        let operation = OperationId::parse(&format!("publish-{}", self.step)).unwrap();
        let receipt = self
            .repo
            .publish_layer(&operation, head, &PublishAttribution::Human, |_, _| Ok(()))
            .unwrap();
        match receipt.outcome {
            PublishOutcome::Published { version } => self.head = version.number.get(),
            other => panic!("{other:?}"),
        }
    }

    fn one_file(&mut self, index: usize, content: &[u8]) -> javelin::LayerHead {
        let entry = FileEntry::Regular {
            content: self.repo.put_blob(content).unwrap(),
            executable: false,
        };
        self.prepare(SparseChanges::new([(path(index), Some(entry))]).unwrap())
    }
}

fn changed_pages(before: &[u8], after: &[u8]) -> usize {
    let pages = after.len().div_ceil(PAGE);
    (0..pages)
        .filter(|&p| {
            let range = p * PAGE..((p + 1) * PAGE).min(after.len());
            before.get(range.clone()) != Some(&after[range])
        })
        .count()
}

fn run(files: usize, history: usize, samples: usize) {
    let root = TempDir::new().unwrap();
    let repo = Repository::init(root.path()).unwrap();
    let database = root.path().join(".javelin/repository.sqlite3");
    let mut bench = Bench {
        repo,
        database,
        head: 0,
        step: 0,
    };
    let started = Instant::now();
    let blobs: Vec<_> = (0..64)
        .map(|i| {
            bench
                .repo
                .put_blob(format!("blob {i}\n").as_bytes())
                .unwrap()
        })
        .collect();
    let import = SparseChanges::new((0..files).map(|i| {
        let entry = FileEntry::Regular {
            content: blobs[i % blobs.len()],
            executable: false,
        };
        (path(i), Some(entry))
    }))
    .unwrap();
    let head = bench.prepare(import);
    bench.publish(&head);
    for step in 1..history {
        let head = bench.one_file(step % files, format!("history {step}\n").as_bytes());
        bench.publish(&head);
    }
    let setup = started.elapsed();
    let (mut latencies, mut growth, mut rewritten) = (Vec::new(), 0, 0);
    for sample in 0..samples {
        let head = bench.one_file(files / 2 + sample, format!("sample {sample}\n").as_bytes());
        let before = std::fs::read(&bench.database).unwrap();
        let start = Instant::now();
        bench.publish(&head);
        latencies.push(start.elapsed());
        let after = std::fs::read(&bench.database).unwrap();
        growth += after.len() as i64 - before.len() as i64;
        rewritten += changed_pages(&before, &after) * PAGE;
    }
    latencies.sort();
    let median: Duration = latencies[latencies.len() / 2];
    println!(
        "{}",
        serde_json::json!({
            "files": files,
            "history": history,
            "samples": samples,
            "head": bench.head,
            "median_publish_ms": median.as_secs_f64() * 1e3,
            "max_publish_ms": latencies.last().unwrap().as_secs_f64() * 1e3,
            "mean_db_growth_bytes": growth / samples as i64,
            "mean_rewritten_page_bytes": rewritten / samples,
            "db_bytes": std::fs::metadata(&bench.database).unwrap().len(),
            "setup_s": setup.as_secs_f64(),
        })
    );
}

#[test]
#[ignore = "benchmark; run explicitly in release mode"]
fn one_file_publish_cost() {
    let cases = std::env::var("JAVELIN_BENCH_CASES")
        .unwrap_or_else(|_| "1000x10,1000x1000,100000x10,100000x1000".into());
    let samples = std::env::var("JAVELIN_BENCH_SAMPLES")
        .map(|s| s.parse().unwrap())
        .unwrap_or(20);
    for case in cases.split(',') {
        let (files, history) = case.split_once('x').unwrap();
        run(files.parse().unwrap(), history.parse().unwrap(), samples);
    }
}
