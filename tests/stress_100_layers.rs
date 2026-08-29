use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_javelin"))
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
    let temp = tempfile::tempdir().unwrap();
    let world = temp.path().join("world");
    let initialized = Command::new(binary())
        .arg("init")
        .arg(&world)
        .output()
        .unwrap();
    assert!(initialized.status.success());

    let mut layers = Vec::new();
    for index in 0..100 {
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
    }

    let checkpoint_workers = layers
        .iter()
        .map(|name| {
            let name = name.clone();
            let world = world.clone();
            thread::spawn(move || {
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
            })
        })
        .collect::<Vec<_>>();
    for worker in checkpoint_workers {
        worker.join().unwrap();
    }

    let refresh_workers = layers
        .iter()
        .map(|name| {
            let name = name.clone();
            let world = world.clone();
            thread::spawn(move || {
                let output = command(&world, vec!["refresh".into(), name]);
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            })
        })
        .collect::<Vec<_>>();
    for worker in refresh_workers {
        worker.join().unwrap();
    }

    let publish_workers = layers
        .iter()
        .map(|name| {
            let name = name.clone();
            let world = world.clone();
            thread::spawn(move || {
                let output = command(
                    &world,
                    vec![
                        "publish".into(),
                        name.clone(),
                        "--idempotency-key".into(),
                        format!("stress-{name}"),
                    ],
                );
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            })
        })
        .collect::<Vec<_>>();
    for worker in publish_workers {
        worker.join().unwrap();
    }

    let current: Value =
        serde_json::from_str(&success(&world, &["world", "current", "--json"])).unwrap();
    assert_eq!(current["result"]["id"], "v101");
    let tree: Value = serde_json::from_str(&success(&world, &["show", "world", "--json"])).unwrap();
    assert_eq!(
        tree["result"]["tree"]["entries"].as_array().unwrap().len(),
        402
    );
    success(&world, &["fsck"]);
}
