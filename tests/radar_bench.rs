//! Collision radar cost against the number of live Layers. Ignored by default.
//! Run in release mode:
//!
//! ```text
//! cargo test --release --test radar_bench -- --ignored --nocapture
//! JAVELIN_BENCH_CASES=100000x10,100000x1000 cargo test --release --test radar_bench ...
//! ```
//!
//! Each case publishes World 1 with `files` paths, then creates `layers` live
//! Layers that each change `PER_LAYER` pseudo-random files. It reports the median
//! radar latency over sample Layers, the median capture latency (which maintains
//! the index), and the same with one hot file that every Layer also changes, where
//! the report itself grows with the number of colliding Layers.
use std::time::{Duration, Instant};

use javelin::{
    FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, OperationId, PublishAttribution,
    PublishOutcome, RepoPath, Repository, SparseChanges, VersionNumber,
};
use tempfile::TempDir;

const PER_LAYER: usize = 10;
const SAMPLES: usize = 50;

fn path(i: usize) -> RepoPath {
    RepoPath::parse(&format!("src/m{}/f{i}.rs", i / 1000)).unwrap()
}

fn layer(repo: &mut Repository, name: &str, changes: &SparseChanges) -> (LayerHead, Duration) {
    let base = repo.layer_base(VersionNumber::new(1).unwrap()).unwrap();
    let empty = repo
        .create_layer(&LayerId::parse(name).unwrap(), &base)
        .unwrap();
    let operation = OperationId::parse(&format!("capture-{name}")).unwrap();
    let start = Instant::now();
    let update = repo.capture_layer(&operation, &empty, changes).unwrap();
    let elapsed = start.elapsed();
    match update {
        LayerUpdate::Applied {
            state: LayerState::Active { head },
        } => (head, elapsed),
        other => panic!("{other:?}"),
    }
}

fn median(mut samples: Vec<Duration>) -> f64 {
    samples.sort();
    samples[samples.len() / 2].as_secs_f64() * 1e3
}

fn run(files: usize, layers: usize, hot: bool) {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let blobs: Vec<_> = (0..64)
        .map(|i| repo.put_blob(format!("blob {i}\n").as_bytes()).unwrap())
        .collect();
    let entry = |i: usize| FileEntry::Regular {
        content: blobs[i % blobs.len()],
        executable: false,
    };
    let import = SparseChanges::new((0..files).map(|i| (path(i), Some(entry(i))))).unwrap();
    let base = repo.layer_base(VersionNumber::new(0).unwrap()).unwrap();
    let seed = repo
        .create_layer(&LayerId::parse("seed").unwrap(), &base)
        .unwrap();
    let LayerUpdate::Applied {
        state: LayerState::Active { head },
    } = repo
        .capture_layer(&OperationId::parse("seed").unwrap(), &seed, &import)
        .unwrap()
    else {
        panic!("seed capture")
    };
    let receipt = repo
        .publish_layer(
            &OperationId::parse("publish-seed").unwrap(),
            &head,
            &PublishAttribution::Human,
            |_, _| Ok(()),
        )
        .unwrap();
    assert!(matches!(receipt.outcome, PublishOutcome::Published { .. }));

    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state as usize
    };
    let (mut heads, mut captures) = (Vec::new(), Vec::new());
    for n in 0..layers {
        let mut picked: Vec<_> = (0..PER_LAYER).map(|_| next() % files).collect();
        if hot {
            picked[0] = 0;
        }
        let changes = SparseChanges::new(
            picked
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .map(|i| (path(i), Some(entry(i + 1)))),
        )
        .unwrap();
        let (head, elapsed) = layer(&mut repo, &format!("l{n}"), &changes);
        heads.push(head);
        captures.push(elapsed);
    }
    let mut radars = Vec::new();
    let mut collisions = 0;
    for head in heads
        .iter()
        .step_by((layers / SAMPLES).max(1))
        .take(SAMPLES)
    {
        let start = Instant::now();
        let radar = repo.radar(head.layer()).unwrap();
        radars.push(start.elapsed());
        collisions += radar.layers.len();
    }
    println!(
        "{}",
        serde_json::json!({
            "files": files,
            "live_layers": layers,
            "paths_per_layer": PER_LAYER,
            "hot_file": hot,
            "median_radar_ms": median(radars.clone()),
            "max_radar_ms": radars.iter().max().unwrap().as_secs_f64() * 1e3,
            "mean_colliding_layers": collisions as f64 / radars.len() as f64,
            "median_capture_ms": median(captures),
        })
    );
}

#[test]
#[ignore = "benchmark; run explicitly in release mode"]
fn radar_cost() {
    let cases =
        std::env::var("JAVELIN_BENCH_CASES").unwrap_or_else(|_| "100000x10,100000x1000".into());
    for case in cases.split(',') {
        let (files, layers) = case.split_once('x').unwrap();
        for hot in [false, true] {
            run(files.parse().unwrap(), layers.parse().unwrap(), hot);
        }
    }
}
