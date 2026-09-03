use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::Instant;

fn binary() -> PathBuf {
    std::env::var_os("JAVELIN_TEST_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_javelin")))
}

fn command(world: &Path, args: Vec<String>) -> Output {
    Command::new(binary())
        .arg("--project")
        .arg(world)
        .args(args)
        .output()
        .unwrap()
}

fn success(world: &Path, args: &[&str]) -> String {
    let output = command(world, args.iter().map(|value| value.to_string()).collect());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

#[test]
fn one_hundred_layers_checkpoint_refresh_and_publish_without_loss() {
    let overall_started = Instant::now();
    let temp = tempfile::tempdir().unwrap();
    let world = temp.path().join("world");
    let initialized = Command::new(binary())
        .arg("init")
        .arg(&world)
        .output()
        .unwrap();
    assert!(initialized.status.success());

    let mut layers = Vec::new();
    let mut create_ms = Vec::new();
    for index in 0..100 {
        let started = Instant::now();
        let name = format!("layer-{index:03}");
        let path = success(&world, &["layer", "create", &name, "--from", "world"]);
        for file in 0..4 {
            fs::write(
                Path::new(&path).join(format!("{name}-{file}.txt")),
                format!("{name}:{file}\n"),
            )
            .unwrap();
        }
        layers.push(name);
        create_ms.push(started.elapsed().as_millis());
    }

    let checkpoint_workers = layers
        .iter()
        .map(|name| {
            let name = name.clone();
            let world = world.clone();
            thread::spawn(move || {
                let started = Instant::now();
                let output = Command::new(binary())
                    .arg("--project")
                    .arg(world.join(".javelin/views").join(name))
                    .args(["checkpoint", "--reason", "stress"])
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                started.elapsed().as_millis()
            })
        })
        .collect::<Vec<_>>();
    eprintln!("STRESS_PROGRESS created 100 layers; checkpoint workers started");
    let checkpoint_ms = checkpoint_workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    eprintln!("STRESS_PROGRESS checkpoint workers complete");

    let refresh_workers = layers
        .iter()
        .map(|name| {
            let name = name.clone();
            let world = world.clone();
            thread::spawn(move || {
                let started = Instant::now();
                let output = command(&world, vec!["refresh".into(), name]);
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                started.elapsed().as_millis()
            })
        })
        .collect::<Vec<_>>();
    let refresh_ms = refresh_workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    eprintln!("STRESS_PROGRESS refresh workers complete");

    let publish_workers = layers
        .iter()
        .map(|name| {
            let name = name.clone();
            let world = world.clone();
            thread::spawn(move || {
                let started = Instant::now();
                let output = command(
                    &world,
                    vec![
                        "publish".into(),
                        name.clone(),
                        "--idempotency-key".into(),
                        format!("stress-{name}"),
                        "--json".into(),
                    ],
                );
                (name, started.elapsed().as_millis(), output)
            })
        })
        .collect::<Vec<_>>();
    let publish_results = publish_workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    eprintln!("STRESS_PROGRESS publish workers complete");
    for (name, _, output) in &publish_results {
        assert!(
            output.status.success(),
            "Publish {name} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let publish_ms = publish_results
        .into_iter()
        .map(|(_, duration, _)| duration)
        .collect::<Vec<_>>();

    let current: Value =
        serde_json::from_str(&success(&world, &["world", "current", "--json"])).unwrap();
    assert_eq!(current["result"]["id"], "v101");
    let tree: Value = serde_json::from_str(&success(&world, &["show", "world", "--json"])).unwrap();
    assert_eq!(
        tree["result"]["tree"]["entries"].as_array().unwrap().len(),
        402
    );
    success(&world, &["fsck"]);
    eprintln!(
        "STRESS_RESULT {}",
        serde_json::json!({
            "layers": 100,
            "files_per_layer": 4,
            "world_version": "v101",
            "total_ms": overall_started.elapsed().as_millis(),
            "create": percentiles(&create_ms),
            "checkpoint_concurrent": percentiles(&checkpoint_ms),
            "refresh_concurrent": percentiles(&refresh_ms),
            "publish_concurrent": percentiles(&publish_ms),
        })
    );
}

fn percentiles(values: &[u128]) -> Value {
    let mut values = values.to_vec();
    values.sort_unstable();
    let at = |percent: usize| {
        let index = ((values.len() * percent).div_ceil(100)).saturating_sub(1);
        values[index]
    };
    serde_json::json!({
        "p50_ms": at(50),
        "p95_ms": at(95),
        "p99_ms": at(99),
        "max_ms": *values.last().unwrap(),
    })
}
