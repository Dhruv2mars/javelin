//! Real Cargo regression for building a source tree outside the caller's cwd.
use std::{fs, process::Command};

use javelin_native_packaging::{build, exe_name};

#[test]
fn build_uses_source_cargo_config_instead_of_caller_config() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let caller = temp.path().join("caller");
    let out = temp.path().join("output");
    for (dir, value) in [(&source, "source"), (&caller, "caller")] {
        fs::create_dir_all(dir.join(".cargo")).unwrap();
        fs::write(
            dir.join(".cargo/config.toml"),
            format!(
                "[env]\nJAVELIN_PACKAGING_CONFIG_ORIGIN = {{ value = '{value}', force = true }}\n"
            ),
        )
        .unwrap();
    }
    fs::create_dir(source.join("src")).unwrap();
    fs::write(
        source.join("Cargo.toml"),
        "[package]\nname = 'javelin-cli'\nversion = '0.1.0'\nedition = '2024'\n\
         [workspace]\n\
         [[bin]]\nname = 'javelin'\npath = 'src/main.rs'\n",
    )
    .unwrap();
    fs::write(
        source.join("Cargo.lock"),
        "version = 4\n[[package]]\nname = 'javelin-cli'\nversion = '0.1.0'\n",
    )
    .unwrap();
    fs::write(
        source.join("src/main.rs"),
        "fn main() { println!(\"{}\", env!(\"JAVELIN_PACKAGING_CONFIG_ORIGIN\")); }\n",
    )
    .unwrap();
    fs::write(source.join("LICENSE"), "fixture license\n").unwrap();
    // Supply the other inputs required by the production source fingerprint.
    for package in [
        "javelin-cli",
        "javelin-collaboration",
        "javelin-merge",
        "javelin-node",
        "javelin-protocol",
        "javelin-stack",
    ] {
        let dir = source.join("crates").join(package);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("Cargo.toml"), "# fingerprint fixture\n").unwrap();
        fs::write(dir.join("LICENSE"), "fixture license\n").unwrap();
    }
    let before = build::source_files_hash(&source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_javelin-pack"))
        .current_dir(&caller)
        .arg("build")
        // Exercise relative input paths as well as an unrelated caller cwd.
        .args(["--source", "../source", "--out", "../output"])
        .args(["--source-date-epoch", "1781000000"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let record: build::BuildRecord =
        serde_json::from_slice(&fs::read(out.join(build::RECORD_FILE)).unwrap()).unwrap();
    assert_eq!((record.source_files_sha256, record.source_files), before);
    let binary = Command::new(out.join("target/release").join(exe_name("javelin")))
        .output()
        .unwrap();
    assert!(binary.status.success());
    assert_eq!(String::from_utf8(binary.stdout).unwrap().trim(), "source");
}
