use sha2::{Digest, Sha256};
use std::{fs, path::Path, process::Command};

fn fingerprint(root: &Path, names: &[&str]) -> String {
    let mut hash = Sha256::new();
    for name in names {
        let bytes = fs::read(root.join(name)).unwrap();
        println!("cargo:rerun-if-changed={}", root.join(name).display());
        hash.update(format!("{name}\0{}\0", bytes.len()));
        hash.update(bytes);
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn main() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let native = fingerprint(
        &here.join("../.."),
        &[
            "Cargo.lock",
            "Cargo.toml",
            "src/domain.rs",
            "src/layer.rs",
            "src/lib.rs",
            "src/main.rs",
            "src/repository.rs",
        ],
    );
    let tool = fingerprint(
        here,
        &["Cargo.lock", "Cargo.toml", "build.rs", "src/main.rs"],
    );
    println!("cargo:rustc-env=NP_NATIVE={native}");
    println!("cargo:rustc-env=NP_TOOL={tool}");
    let output = Command::new(std::env::var("RUSTC").unwrap())
        .arg("-Vv")
        .output()
        .unwrap();
    println!(
        "cargo:rustc-env=NP_RUSTC={}",
        String::from_utf8(output.stdout)
            .unwrap()
            .replace('\n', "; ")
    );
    for key in [
        "PROFILE",
        "OPT_LEVEL",
        "DEBUG",
        "TARGET",
        "CARGO_ENCODED_RUSTFLAGS",
    ] {
        println!(
            "cargo:rustc-env=NP_{key}={}",
            std::env::var(key).unwrap_or_default()
        );
        println!("cargo:rerun-if-env-changed={key}");
    }
}
