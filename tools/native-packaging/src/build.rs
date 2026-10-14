//! Locked release build of the `javelin` CLI with a controlled environment.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};

use crate::{
    Result, bail, capture, exe_name, host_triple, rustc_verbose, sha256_file, util::write,
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
    let source = std::fs::canonicalize(source)?;
    fs::create_dir_all(out)?;
    let out = std::fs::canonicalize(out)?;
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
                cargo_home.display(),
                source.display(),
                target_dir.display()
            ),
        ));
    }

    let command = [
        "cargo",
        "build",
        "--locked",
        "--release",
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
        .args(&command[1..])
        .arg("--manifest-path")
        .arg(source.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("CARGO_ENCODED_RUSTFLAGS", rustflags.join("\u{1f}"));
    for (key, value) in &extra_env {
        cargo.env(key, value);
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
    };
    write(&out.join(RECORD_FILE), &serde_json::to_vec_pretty(&record)?)?;
    Ok(record)
}
