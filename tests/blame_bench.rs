//! Trace blame cost against a path's history length. Ignored by default. Run in
//! release mode:
//!
//! ```text
//! cargo test --release --test blame_bench -- --ignored --nocapture
//! JAVELIN_BENCH_CASES=1000x10,1000x1000 cargo test --release --test blame_bench ...
//! ```
//!
//! Each case `lines`x`versions` publishes one file of `lines` lines, then
//! `versions - 1` more Publishes that each edit one line of it, each in its own
//! Layer and session, alongside an unrelated file so the World also grows
//! elsewhere. It reports the median path-level and line-level blame latency.
use std::time::{Duration, Instant};

use javelin::{
    Actor, FileEntry, LayerId, LayerState, LayerUpdate, OperationId, Provenance,
    PublishAttribution, PublishOutcome, RepoPath, Repository, SparseChanges,
};
use tempfile::TempDir;

fn publish(repo: &mut Repository, step: usize, files: &[(&str, String)]) {
    let changes = SparseChanges::new(files.iter().map(|(path, text)| {
        let entry = FileEntry::Regular {
            content: repo.put_blob(text.as_bytes()).unwrap(),
            executable: false,
        };
        (RepoPath::parse(path).unwrap(), Some(entry))
    }))
    .unwrap();
    let base = repo.layer_base(repo.head().unwrap().number).unwrap();
    let layer = LayerId::parse(&format!("l{step}")).unwrap();
    let empty = repo.create_layer(&layer, &base).unwrap();
    repo.set_provenance(Some(
        Provenance::new(
            Actor::Agent,
            Some("bench".into()),
            Some(format!("s{step}")),
            None,
        )
        .unwrap(),
    ));
    let operation = OperationId::parse(&format!("c{step}")).unwrap();
    let LayerUpdate::Applied {
        state: LayerState::Active { head },
    } = repo.capture_layer(&operation, &empty, &changes).unwrap()
    else {
        panic!("capture")
    };
    let operation = OperationId::parse(&format!("p{step}")).unwrap();
    let receipt = repo
        .publish_layer(&operation, &head, &PublishAttribution::Human, |_, _| Ok(()))
        .unwrap();
    assert!(matches!(receipt.outcome, PublishOutcome::Published { .. }));
}

fn median(mut samples: Vec<Duration>) -> f64 {
    samples.sort();
    samples[samples.len() / 2].as_secs_f64() * 1e3
}

fn run(lines: usize, versions: usize) {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let mut text: Vec<String> = (0..lines)
        .map(|i| format!("line {i} of the file"))
        .collect();
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let started = Instant::now();
    for step in 0..versions {
        if step > 0 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            text[state as usize % lines] = format!("edited in version {step}");
        }
        let body = text.join("\n") + "\n";
        publish(
            &mut repo,
            step,
            &[("src/hot.rs", body), ("src/other.rs", format!("{step}\n"))],
        );
    }
    let setup = started.elapsed();
    let path = RepoPath::parse("src/hot.rs").unwrap();
    let paths: Vec<_> = (0..20)
        .map(|_| {
            let start = Instant::now();
            repo.blame(&path, None).unwrap().unwrap();
            start.elapsed()
        })
        .collect();
    let mut origins = 0;
    let line_samples: Vec<_> = (0..5)
        .map(|_| {
            let start = Instant::now();
            let blame = repo.blame_lines(&path, None).unwrap().unwrap();
            origins = blame.origins.len();
            start.elapsed()
        })
        .collect();
    println!(
        "{}",
        serde_json::json!({
            "lines": lines,
            "versions_of_path": versions,
            "median_path_blame_ms": median(paths),
            "median_line_blame_ms": median(line_samples),
            "distinct_line_origins": origins,
            "setup_s": setup.as_secs_f64(),
        })
    );
}

#[test]
#[ignore = "benchmark; run explicitly in release mode"]
fn blame_cost() {
    let cases = std::env::var("JAVELIN_BENCH_CASES")
        .unwrap_or_else(|_| "1000x10,1000x100,1000x1000".into());
    for case in cases.split(',') {
        let (lines, versions) = case.split_once('x').unwrap();
        run(lines.parse().unwrap(), versions.parse().unwrap());
    }
}
