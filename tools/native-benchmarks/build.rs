//! Embeds Git-free source provenance: SHA-256 over the measured javelin sources and
//! over this tool, so a result names exact bytes without invoking any VCS.
use std::{fs, io, path::Path};

use sha2::{Digest, Sha256};

fn collect(dir: &Path, base: &Path, out: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, base, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(
                path.strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    Ok(())
}

fn digest(root: &Path, dirs: &[&str], files: &[&str]) -> String {
    let mut names = Vec::new();
    for dir in dirs {
        collect(&root.join(dir), root, &mut names).unwrap();
    }
    names.extend(files.iter().map(|f| f.to_string()));
    names.sort();
    let mut hash = Sha256::new();
    for name in &names {
        let bytes = fs::read(root.join(name)).unwrap();
        hash.update(format!("{name}\0{}\0", bytes.len()));
        hash.update(&bytes);
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn main() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = here.join("../..");
    let javelin = digest(&root, &["src"], &["Cargo.toml"]);
    let tool = digest(here, &["src"], &["Cargo.toml", "Cargo.lock", "build.rs"]);
    println!("cargo:rustc-env=NB_JAVELIN_SOURCE_SHA256={javelin}");
    println!("cargo:rustc-env=NB_TOOL_SOURCE_SHA256={tool}");
    println!(
        "cargo:rustc-env=NB_TARGET={}",
        std::env::var("TARGET").unwrap()
    );
    println!(
        "cargo:rustc-env=NB_PROFILE={}",
        std::env::var("PROFILE").unwrap()
    );
    println!("cargo:rerun-if-changed=../../src");
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=build.rs");
}
