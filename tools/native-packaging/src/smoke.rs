//! Git-free smoke of an unpacked `javelin` binary, resolved through `PATH`.
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde::Serialize;
use serde_json::Value;

use crate::{Error, Result, exe_name, read, sha256_file, util::write};

pub const SHIM_MARKER_ENV: &str = "JAVELIN_PACK_GIT_SHIM_MARKER";
pub const SHIM_EXIT: i32 = 97;
const HELP_MARKER: &str = "Javelin native repository foundation";

#[derive(Serialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Serialize)]
pub struct Report {
    pub bin_dir: String,
    pub binary_sha256: String,
    pub checks: Vec<Check>,
    pub ok: bool,
}

struct Smoke {
    checks: Vec<Check>,
}

impl Smoke {
    fn check(&mut self, name: &str, ok: bool, detail: impl Into<String>) -> bool {
        self.checks.push(Check { name: name.into(), ok, detail: detail.into() });
        ok
    }
}

/// First `name` found on `path`, the way a shell resolves it.
pub fn which(name: &str, path: &OsString) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(exe_name(name)))
        .find(|candidate| candidate.is_file())
}

fn unique_dir(label: &str) -> Result<PathBuf> {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
    let dir = std::env::temp_dir().join(format!("javelin-smoke-{label}-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&dir)?;
    Ok(fs::canonicalize(dir)?)
}

fn run(path: &OsString, marker: &Path, cwd: &Path, args: &[&str]) -> Result<Output> {
    Ok(Command::new("javelin")
        .args(args)
        .current_dir(cwd)
        .env("PATH", path)
        .env(SHIM_MARKER_ENV, marker)
        .output()?)
}

fn json(output: &Output) -> Option<Value> {
    serde_json::from_slice(&output.stdout).ok()
}

pub fn smoke(bin_dir: &Path, expected_sha256: Option<&str>) -> Result<Report> {
    let bin_dir = fs::canonicalize(bin_dir)?;
    let binary = bin_dir.join(exe_name("javelin"));
    let binary_sha256 = sha256_file(&binary)?;
    let mut s = Smoke { checks: Vec::new() };
    if let Some(expected) = expected_sha256 {
        s.check("binary hash matches expected", binary_sha256 == expected, format!("{binary_sha256} vs {expected}"));
    }

    let work = unique_dir("work")?;
    let project = work.join("project");
    fs::create_dir(&project)?;
    let git_ancestor = project.ancestors().find(|a| a.join(".git").exists());
    s.check("project is outside any Git worktree", git_ancestor.is_none(), format!("{git_ancestor:?}"));

    // Shim directory: any call to `git` fails and leaves a marker.
    let shim_dir = work.join("shim");
    let pre_dir = work.join("preexisting");
    fs::create_dir_all(&shim_dir)?;
    fs::create_dir_all(&pre_dir)?;
    let me = std::env::current_exe()?;
    fs::copy(&me, shim_dir.join(exe_name("git")))?;
    fs::copy(&me, pre_dir.join(exe_name("javelin")))?;
    let pre_bytes = read(&pre_dir.join(exe_name("javelin")))?;
    let marker = work.join("git-was-called");

    let path = std::env::join_paths([&shim_dir, &bin_dir, &pre_dir])?;
    // The shim must be able to detect a Git call, or the later check proves nothing.
    let probe = Command::new("git").arg("--version").env("PATH", &path).env(SHIM_MARKER_ENV, &marker).output();
    let shim_works = probe.map(|o| o.status.code() == Some(SHIM_EXIT)).unwrap_or(false) && marker.exists();
    s.check("git shim detects a Git call", shim_works, "");
    let _ = fs::remove_file(&marker);

    let resolved = which("javelin", &path);
    s.check(
        "javelin resolves to the unpacked binary through PATH",
        resolved.as_deref().map(|p| fs::canonicalize(p).ok()) == Some(Some(binary.clone())),
        format!("{resolved:?}"),
    );
    let help = run(&path, &marker, &project, &["--help"])?;
    s.check(
        "javelin --help by name is the Javelin CLI",
        help.status.success() && String::from_utf8_lossy(&help.stdout).contains(HELP_MARKER),
        String::from_utf8_lossy(&help.stdout).lines().next().unwrap_or("").to_string(),
    );

    let existing = [0u8, 255, 42, b'\n'];
    write(&project.join("existing.bin"), &existing)?;
    let init = run(&path, &marker, &project, &["init", "--json"])?;
    let init_json = json(&init);
    let init_ok = init.status.success()
        && init.stderr.is_empty()
        && init_json.as_ref().is_some_and(|v| {
            v["ok"] == true && v["command"] == "init" && v["initial"]["number"] == 0 && v["tracked_files"] == 0
        });
    s.check("init --json succeeds in a new non-Git project", init_ok, String::from_utf8_lossy(&init.stdout).trim().to_string());
    s.check(
        "init leaves existing project files untouched",
        read(&project.join("existing.bin"))? == existing,
        "",
    );
    let again = run(&path, &marker, &project, &["init", "--json"])?;
    s.check("repeated init is idempotent", again.status.success() && again.stdout == init.stdout, "");

    let history = run(&path, &marker, &project, &["history", "--json"])?;
    let history_json = json(&history);
    s.check(
        "history --json lists World 0",
        history.status.success()
            && history_json.as_ref().is_some_and(|v| {
                v["ok"] == true
                    && v["versions"].as_array().is_some_and(|a| a.len() == 1)
                    && init_json.as_ref().is_some_and(|i| v["versions"][0] == i["initial"])
            }),
        String::from_utf8_lossy(&history.stdout).trim().to_string(),
    );
    let check = run(&path, &marker, &project, &["check", "--json"])?;
    let check_json = json(&check);
    s.check(
        "check --json reports integrity OK",
        check.status.success()
            && check_json.as_ref().is_some_and(|v| v["ok"] == true && v["integrity"]["versions"] == 1),
        String::from_utf8_lossy(&check.stdout).trim().to_string(),
    );
    let plain = run(&path, &marker, &project, &["check"])?;
    s.check(
        "plain check prints success text",
        plain.status.success() && String::from_utf8_lossy(&plain.stdout).starts_with("Integrity OK"),
        "",
    );

    // Corruption: damage the storage file's data pages, expect a structured nonzero failure, then restore.
    let db = project.join(".javelin").join("repository.sqlite3");
    let original = read(&db)?;
    let mut damaged = original.clone();
    let start = 4096.min(damaged.len());
    for byte in &mut damaged[start..] {
        *byte = !*byte;
    }
    write(&db, &damaged)?;
    let broken = run(&path, &marker, &project, &["check", "--json"])?;
    let broken_err: Option<Value> = serde_json::from_slice(&broken.stderr).ok();
    s.check(
        "check --json on damaged storage exits 1 with a structured error and no success output",
        broken.status.code() == Some(1)
            && broken.stdout.is_empty()
            && broken_err.as_ref().is_some_and(|v| v["ok"] == false && v["error"]["code"].is_string()),
        String::from_utf8_lossy(&broken.stderr).trim().to_string(),
    );
    write(&db, &original)?;
    let restored = run(&path, &marker, &project, &["check", "--json"])?;
    s.check("check passes again after restoring the storage bytes", restored.status.success(), "");

    // Resolution follows PATH order: with the preexisting binary first, that one runs, untouched.
    let reordered = std::env::join_paths([&shim_dir, &pre_dir, &bin_dir])?;
    let other = which("javelin", &reordered);
    s.check(
        "PATH order selects the preexisting binary when it comes first",
        other.as_deref().map(|p| fs::canonicalize(p).ok()) == Some(Some(fs::canonicalize(pre_dir.join(exe_name("javelin")))?)),
        format!("{other:?}"),
    );
    let pre_help = run(&reordered, &marker, &project, &["--help"])?;
    s.check(
        "preexisting binary, not the package, runs when first on PATH",
        !String::from_utf8_lossy(&pre_help.stdout).contains(HELP_MARKER),
        "",
    );
    s.check(
        "preexisting binary bytes unchanged",
        read(&pre_dir.join(exe_name("javelin")))? == pre_bytes,
        "",
    );
    s.check(
        "unpacked binary bytes unchanged by the run",
        sha256_file(&binary)? == binary_sha256,
        "",
    );
    s.check("Git was never invoked", !marker.exists(), "");

    let _ = fs::remove_dir_all(&work);
    let ok = s.checks.iter().all(|c| c.ok);
    Ok(Report { bin_dir: bin_dir.display().to_string(), binary_sha256, checks: s.checks, ok })
}

/// Called by `main` when this executable was copied to a `git` name as a shim.
pub fn run_as_git_shim() -> Option<i32> {
    let me = std::env::current_exe().ok()?;
    if me.file_stem()?.to_string_lossy().to_lowercase() != "git" {
        return None;
    }
    if let Some(marker) = std::env::var_os(SHIM_MARKER_ENV) {
        let _ = fs::write(marker, b"git invoked");
    }
    Some(SHIM_EXIT)
}

pub fn fail_unless_ok(report: &Report) -> Result<()> {
    if report.ok {
        return Ok(());
    }
    let failed: Vec<_> = report.checks.iter().filter(|c| !c.ok).map(|c| c.name.as_str()).collect();
    Err(Error(format!("smoke failed: {}", failed.join("; "))))
}
