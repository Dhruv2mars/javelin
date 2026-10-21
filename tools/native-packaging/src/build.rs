//! Locked release build of the `javelin` CLI with a controlled environment.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};

use crate::{
    Result, bail, capture, exe_name, host_triple, list_files, rustc_verbose, sha256_file,
    sha256_hex, util::write,
};

pub const RECORD_FILE: &str = "build-record.json";

#[derive(Serialize, Deserialize)]
pub struct BuildRecord {
    pub command: Vec<String>,
    pub rustflags: Vec<String>,
    pub extra_env: Vec<(String, String)>,
    pub removed_env_prefixes: Vec<String>,
    pub source_date_epoch: u64,
    pub host_triple: String,
    pub rustc_verbose: String,
    pub cargo_version: String,
    /// Binary path relative to the build output directory.
    pub binary: String,
    pub binary_sha256: String,
    /// Fingerprint of the sources this binary was built from.
    pub source_files_sha256: String,
    pub source_files: usize,
}

const REMOVED_ENV: &[&str] = &[
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_",
    "CARGO_PROFILE_",
    "CARGO_TARGET_DIR",
    "CFLAGS",
    "CXXFLAGS",
    "RUSTC",
    "CC",
    "CXX",
    "TARGET_CFLAGS",
    "HOST_CFLAGS",
    "CFLAGS_",
    "CXXFLAGS_",
    "CC_",
];

fn cargo_home() -> PathBuf {
    if let Some(home) = std::env::var_os("CARGO_HOME") {
        return home.into();
    }
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap_or_default();
    PathBuf::from(home).join(".cargo")
}

/// Build `<source>` into `<out>/target` and write `<out>/build-record.json`.
/// `extra_rustflags` exist so tests can produce a genuinely different binary.
pub fn build(
    source: &Path,
    out: &Path,
    source_date_epoch: u64,
    extra_rustflags: &[String],
) -> Result<BuildRecord> {
    let source = crate::simplify(std::fs::canonicalize(source)?);
    fs::create_dir_all(out)?;
    let out = crate::simplify(std::fs::canonicalize(out)?);
    let target_dir = out.join("target");
    let host = host_triple()?;
    let sysroot = capture(Command::new("rustc").args(["--print", "sysroot"]))?;
    let cargo_home = cargo_home();

    // Later remaps take precedence, so the narrowest prefix comes last.
    let mut rustflags = vec![
        format!("--remap-path-prefix={}=/cargo-home", cargo_home.display()),
        format!("--remap-path-prefix={}=/rust-sysroot", sysroot.trim()),
        format!("--remap-path-prefix={}=/javelin-source", source.display()),
        format!(
            "--remap-path-prefix={}=/javelin-target",
            target_dir.display()
        ),
    ];
    rustflags.extend(extra_rustflags.iter().cloned());

    let mut extra_env = vec![
        (
            "SOURCE_DATE_EPOCH".to_string(),
            source_date_epoch.to_string(),
        ),
        ("CARGO_INCREMENTAL".to_string(), "0".to_string()),
        ("TZ".to_string(), "UTC".to_string()),
        ("LC_ALL".to_string(), "C".to_string()),
    ];
    if !host.contains("msvc") {
        extra_env.push((
            "CFLAGS".to_string(),
            format!(
                "-ffile-prefix-map={}=/cargo-home -ffile-prefix-map={}=/javelin-source -ffile-prefix-map={}=/javelin-target",
                shell_quote(&cargo_home.display().to_string()),
                shell_quote(&source.display().to_string()),
                shell_quote(&target_dir.display().to_string())
            ),
        ));
        // Paths may contain spaces; make `cc` parse CFLAGS as shell-escaped words.
        extra_env.push(("CC_SHELL_ESCAPED_FLAGS".to_string(), "1".to_string()));
    }

    let command = [
        "cargo",
        "build",
        "--locked",
        "--release",
        "--package",
        "javelin-cli",
        "--bin",
        "javelin",
    ]
    .map(String::from)
    .to_vec();
    let mut cargo = Command::new("cargo");
    for (key, _) in std::env::vars() {
        if REMOVED_ENV
            .iter()
            .any(|name| key == *name || (name.ends_with('_') && key.starts_with(name)))
        {
            cargo.env_remove(key);
        }
    }
    cargo
        .current_dir(&source)
        .args(&command[1..])
        .arg("--manifest-path")
        .arg(source.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("CARGO_ENCODED_RUSTFLAGS", rustflags.join("\u{1f}"));
    for (key, value) in &extra_env {
        cargo.env(key, value);
    }
    let source_before = source_files_hash(&source)?;
    // A failed or interrupted rebuild must not leave a usable stale record.
    let record_path = out.join(RECORD_FILE);
    if record_path.exists() {
        fs::remove_file(&record_path)?;
    }
    let status = cargo.status()?;
    if !status.success() {
        bail!("cargo build failed: {status}");
    }

    let binary = format!("target/release/{}", exe_name("javelin"));
    let binary_path = out.join(&binary);
    if !binary_path.is_file() {
        bail!("expected binary missing: {}", binary_path.display());
    }
    let (source_files_sha256, source_files) = source_files_hash(&source)?;
    if (source_files_sha256.clone(), source_files) != source_before {
        bail!("source changed during cargo build; refusing build provenance");
    }
    let record = BuildRecord {
        command,
        rustflags,
        extra_env,
        removed_env_prefixes: REMOVED_ENV.iter().map(|s| s.to_string()).collect(),
        source_date_epoch,
        host_triple: host,
        rustc_verbose: rustc_verbose()?,
        cargo_version: capture(Command::new("cargo").arg("--version"))?
            .trim()
            .to_string(),
        binary,
        binary_sha256: sha256_file(&binary_path)?,
        source_files_sha256,
        source_files,
    };
    write(&out.join(RECORD_FILE), &serde_json::to_vec_pretty(&record)?)?;
    Ok(record)
}

/// Single-quote a word for shell-escaped parsing (`'` becomes `'\''`).
fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// The files that define a build: `Cargo.toml`, `Cargo.lock`, `LICENSE`, any
/// `.cargo/config*`, root `src/**`, CLI/merge/Stack manifests and sources, and
/// adapter licenses, as `/`-joined relative paths.
pub fn source_files(source: &Path) -> Result<Vec<String>> {
    let mut files = vec![
        "Cargo.toml".to_string(),
        "Cargo.lock".to_string(),
        "LICENSE".to_string(),
    ];
    for config in [".cargo/config.toml", ".cargo/config"] {
        if source.join(config).is_file() {
            files.push(config.to_string());
        }
    }
    files.extend(
        list_files(&source.join("src"))?
            .into_iter()
            .map(|f| format!("src/{f}")),
    );
    for package in [
        "javelin-cli",
        "javelin-collaboration",
        "javelin-control",
        "javelin-identity",
        "javelin-merge",
        "javelin-node",
        "javelin-protocol",
        "javelin-stack",
    ] {
        files.push(format!("crates/{package}/Cargo.toml"));
        if source.join(format!("crates/{package}/LICENSE")).is_file() {
            files.push(format!("crates/{package}/LICENSE"));
        }
        files.extend(
            list_files(&source.join(format!("crates/{package}/src")))?
                .into_iter()
                .map(|f| format!("crates/{package}/src/{f}")),
        );
    }
    Ok(files)
}

/// SHA-256 over [`source_files`] (path and content hashes, Git-free).
pub fn source_files_hash(source: &Path) -> Result<(String, usize)> {
    let files = source_files(source)?;
    let mut listing = String::new();
    for file in &files {
        listing.push_str(&format!("{file}\n{}\n", sha256_file(&source.join(file))?));
    }
    Ok((sha256_hex(listing.as_bytes()), files.len()))
}
