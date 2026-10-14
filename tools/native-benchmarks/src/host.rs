//! Host, disk and memory probes. Every probe records failure as null instead of guessing.
use std::{fs, path::Path, process::Command};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn run(program: &str, args: &[&str]) -> Value {
    match Command::new(program).args(args).output() {
        Ok(out) if out.status.success() => {
            json!(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
        Ok(out) => json!({"error": format!("exit {:?}", out.status.code())}),
        Err(e) => json!({"error": e.to_string()}),
    }
}

fn read(path: &str) -> Value {
    fs::read_to_string(path)
        .map(|s| json!(s))
        .unwrap_or(Value::Null)
}

fn grep_first(text: Value, key: &str) -> Value {
    text.as_str()
        .and_then(|t| t.lines().find(|l| l.starts_with(key)))
        .map(|l| json!(l.trim()))
        .unwrap_or(Value::Null)
}

/// The one mount-table line for the device that holds the work directory.
fn mount_entry(df: &Value, mounts: Value) -> Value {
    let device = df
        .as_str()
        .and_then(|t| t.lines().nth(1))
        .and_then(|l| l.split_whitespace().next());
    match (device, mounts.as_str()) {
        (Some(device), Some(table)) => table
            .lines()
            .find(|l| l.starts_with(&format!("{device} ")))
            .map(|l| json!(l))
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

pub fn executable_sha256() -> String {
    let exe = std::env::current_exe().expect("current executable");
    sha256_hex(&fs::read(exe).expect("read executable"))
}

pub fn host_info(workdir: &Path) -> Value {
    let dir = workdir.to_string_lossy().to_string();
    let mut info = json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "family": std::env::consts::FAMILY,
        "logical_cpus": std::thread::available_parallelism().map(|n| n.get()).ok(),
        "rustc": env!("NB_RUSTC_VERBOSE"),
        "rustc_on_path_at_run": run("rustc", &["-Vv"]),
        "cargo": run("cargo", &["-V"]),
        "workdir": dir,
    });
    let map = info.as_object_mut().unwrap();
    if cfg!(target_os = "macos") {
        map.insert("os_version".into(), run("sw_vers", &[]));
        map.insert("kernel".into(), run("uname", &["-srm"]));
        map.insert(
            "cpu_model".into(),
            run("sysctl", &["-n", "machdep.cpu.brand_string"]),
        );
        map.insert("hw_model".into(), run("sysctl", &["-n", "hw.model"]));
        map.insert("memory_bytes".into(), run("sysctl", &["-n", "hw.memsize"]));
        map.insert("filesystem_df".into(), run("df", &["-P", "-k", &dir]));
        map.insert(
            "mount_entry".into(),
            mount_entry(&map["filesystem_df"], run("mount", &[])),
        );
    } else if cfg!(target_os = "linux") {
        map.insert("os_version".into(), read("/etc/os-release"));
        map.insert("kernel".into(), run("uname", &["-srm"]));
        map.insert(
            "cpu_model".into(),
            grep_first(read("/proc/cpuinfo"), "model name"),
        );
        map.insert(
            "memory".into(),
            grep_first(read("/proc/meminfo"), "MemTotal"),
        );
        map.insert(
            "filesystem_type".into(),
            run("stat", &["-f", "-c", "%T", &dir]),
        );
        map.insert("filesystem_df".into(), run("df", &["-P", "-k", &dir]));
        map.insert(
            "mount_entry".into(),
            mount_entry(&map["filesystem_df"], read("/proc/mounts")),
        );
    } else {
        map.insert(
            "note".into(),
            json!("host probes are not implemented for this OS; unmeasured"),
        );
    }
    info
}

pub struct Usage {
    pub physical: Option<u64>,
    pub logical: u64,
    pub files: u64,
}

/// Logical = sum of file lengths. Physical = allocated blocks (Unix only; None elsewhere).
pub fn disk_usage(dir: &Path) -> Usage {
    let mut usage = Usage {
        physical: if cfg!(unix) { Some(0) } else { None },
        logical: 0,
        files: 0,
    };
    walk(dir, &mut usage);
    usage
}

fn walk(dir: &Path, usage: &mut Usage) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.is_dir() {
            walk(&entry.path(), usage);
        } else if meta.is_file() {
            usage.logical += meta.len();
            usage.files += 1;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if let Some(p) = usage.physical.as_mut() {
                    *p += meta.blocks() * 512;
                }
            }
        }
    }
}

/// Peak resident set of this process. macOS reports bytes, Linux kilobytes.
/// Linux reads VmHWM: ru_maxrss survives exec and can carry the spawning parent's peak.
pub fn peak_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: getrusage fills the provided, properly sized struct.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
            return None;
        }
        let raw = usage.ru_maxrss.max(0) as u64;
        Some(raw)
    }
    #[cfg(not(unix))]
    {
        None
    }
}
