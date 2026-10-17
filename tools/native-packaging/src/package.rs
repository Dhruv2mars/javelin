//! Archive, checksum, and manifest for one built development binary.
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bail;
use crate::{
    Result,
    build::{BuildRecord, RECORD_FILE, source_files_hash},
    exe_name, host_triple, is_hex, read, sha256_hex,
    util::{capture, write},
};

pub const MANIFEST_FILE: &str = "PACKAGE.json";
/// Strings that only exist when test-only process hooks were compiled in.
pub const FORBIDDEN_MARKERS: &[&str] = &["JAVELIN_TEST_", "process_tests"];

#[derive(Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub channel: String,
    pub pre_ga: bool,
    pub notice: String,
    pub name: String,
    pub cargo_version: String,
    pub source: SourceInfo,
    pub toolchain: Toolchain,
    pub platform: Platform,
    pub binary: BinaryInfo,
    pub dependencies: Vec<Dependency>,
}

#[derive(Serialize, Deserialize)]
pub struct SourceInfo {
    /// Supplied by the developer or CI; this tool never reads Git.
    pub sha: String,
    pub cargo_lock_sha256: String,
    /// SHA-256 over build inputs, including root and CLI sources (path and content hashes).
    pub source_files_sha256: String,
    pub source_files: usize,
}

#[derive(Serialize, Deserialize)]
pub struct Toolchain {
    pub rustc_verbose: String,
    pub cargo: String,
    pub build_command: Vec<String>,
    pub rustflags: Vec<String>,
    pub source_date_epoch: u64,
}

#[derive(Serialize, Deserialize)]
pub struct Platform {
    pub known: KnownPlatform,
    pub minimum_os_version: String,
    pub other_architectures: String,
}

#[derive(Serialize, Deserialize)]
pub struct KnownPlatform {
    pub os: String,
    pub arch: String,
    pub target_triple: String,
}

#[derive(Serialize, Deserialize)]
pub struct BinaryInfo {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub test_hook_markers_absent: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Dependency {
    pub name: String,
    pub version: String,
    pub source: Option<String>,
    pub checksum: Option<String>,
    pub license: Option<String>,
}

pub struct Packaged {
    pub archive: PathBuf,
    pub checksum_file: PathBuf,
    pub archive_sha256: String,
    pub top_dir: String,
}

struct Entry {
    path: String,
    data: Vec<u8>,
    executable: bool,
}

pub fn package(build_dir: &Path, source: &Path, source_sha: &str, out: &Path) -> Result<Packaged> {
    if !is_hex(source_sha, 40) {
        bail!("--source-sha must be 40 lowercase hex characters, got {source_sha:?}");
    }
    let record: BuildRecord = serde_json::from_slice(&read(&build_dir.join(RECORD_FILE))?)?;
    let target = host_triple()?;
    if record.host_triple != target {
        bail!(
            "build record is for {}, this host is {target}",
            record.host_triple
        );
    }
    let binary_path = build_dir.join(&record.binary);
    let binary = read(&binary_path)?;
    if sha256_hex(&binary) != record.binary_sha256 {
        bail!("binary does not match its build record");
    }
    for marker in FORBIDDEN_MARKERS {
        if binary.windows(marker.len()).any(|w| w == marker.as_bytes()) {
            bail!("binary contains test-only marker {marker:?}; refusing to package");
        }
    }
    // A real binary must answer --help, not just have the right name.
    let help = capture(Command::new(&binary_path).arg("--help"))?;
    if !help.contains("Javelin native repository foundation") {
        bail!("binary --help does not identify the Javelin CLI");
    }

    let cargo_version = crate_version(source)?;
    let (graph, lock_sha) = dependency_graph(source, &target)?;
    let (files_hash, files_count) = source_files_hash(source)?;
    if files_hash != record.source_files_sha256 {
        bail!("--source does not match the sources this binary was built from");
    }
    let version_sha12 = &source_sha[..12];
    let top_dir = format!("javelin-development-{cargo_version}-{version_sha12}-{target}");

    let manifest = Manifest {
        schema: 1,
        channel: "development".into(),
        pre_ga: true,
        notice: "Development build of the native Javelin CLI (init, history, check). \
                 Not a release; the full Layer CLI does not exist yet."
            .into(),
        name: "javelin".into(),
        cargo_version: cargo_version.clone(),
        source: SourceInfo {
            sha: source_sha.into(),
            cargo_lock_sha256: lock_sha,
            source_files_sha256: files_hash,
            source_files: files_count,
        },
        toolchain: Toolchain {
            rustc_verbose: record.rustc_verbose.clone(),
            cargo: record.cargo_version.clone(),
            build_command: record.command.clone(),
            rustflags: record.rustflags.clone(),
            source_date_epoch: record.source_date_epoch,
        },
        platform: Platform {
            known: KnownPlatform {
                os: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
                target_triple: target.clone(),
            },
            minimum_os_version: "unconfirmed".into(),
            other_architectures: "unconfirmed; not built or tested".into(),
        },
        binary: BinaryInfo {
            path: format!("bin/{}", exe_name("javelin")),
            sha256: record.binary_sha256.clone(),
            size: binary.len() as u64,
            test_hook_markers_absent: FORBIDDEN_MARKERS.iter().map(|s| s.to_string()).collect(),
        },
        dependencies: graph.iter().map(|(d, _)| d.clone()).collect(),
    };

    let mut entries = vec![
        Entry {
            path: format!("bin/{}", exe_name("javelin")),
            data: binary,
            executable: true,
        },
        Entry {
            path: "LICENSE".into(),
            data: read(&source.join("LICENSE"))?,
            executable: false,
        },
        Entry {
            path: "README.txt".into(),
            data: README.as_bytes().to_vec(),
            executable: false,
        },
        Entry {
            path: "THIRD-PARTY-NOTICES.md".into(),
            data: notices(&graph).into_bytes(),
            executable: false,
        },
        Entry {
            path: MANIFEST_FILE.into(),
            data: serde_json::to_vec_pretty(&manifest)?,
            executable: false,
        },
    ];
    for (dep, files) in &graph {
        for (file_name, data) in files {
            entries.push(Entry {
                path: format!("licenses/{}-{}/{file_name}", dep.name, dep.version),
                data: data.clone(),
                executable: false,
            });
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    fs::create_dir_all(out)?;
    let zip = target.contains("windows");
    let archive = out.join(format!("{top_dir}.{}", if zip { "zip" } else { "tar.gz" }));
    let bytes = if zip {
        write_zip(&top_dir, &entries, record.source_date_epoch)?
    } else {
        write_tar_gz(&top_dir, &entries, record.source_date_epoch)?
    };
    write(&archive, &bytes)?;
    let archive_sha256 = sha256_hex(&bytes);
    let checksum_file = out.join(format!(
        "{}.sha256",
        archive.file_name().unwrap().to_string_lossy()
    ));
    write(
        &checksum_file,
        format!(
            "{archive_sha256}  {}\n",
            archive.file_name().unwrap().to_string_lossy()
        )
        .as_bytes(),
    )?;
    Ok(Packaged {
        archive,
        checksum_file,
        archive_sha256,
        top_dir,
    })
}

const README: &str = "Javelin native CLI, DEVELOPMENT build (pre-GA).\n\n\
This is not a release. The CLI supports init, history, and check only; Layers and\n\
the complete Layer Publish workflow are not implemented.\n\n\
Run bin/javelin from a project directory. Existing project files are not ingested.\n\
Nothing here installs itself or changes PATH. See PACKAGE.json for the exact source,\n\
toolchain, and dependency checksums, and LICENSE plus THIRD-PARTY-NOTICES.md for licenses.\n";

fn crate_version(source: &Path) -> Result<String> {
    let manifest: toml::Table =
        toml::from_str(&String::from_utf8(read(&source.join("Cargo.toml"))?)?)?;
    let package = manifest
        .get("package")
        .and_then(|p| p.as_table())
        .ok_or_else(|| crate::Error("Cargo.toml has no [package]".into()))?;
    if package.get("name").and_then(|n| n.as_str()) != Some("javelin") {
        bail!("source crate is not named javelin");
    }
    Ok(package
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| crate::Error("Cargo.toml has no package.version".into()))?
        .to_string())
}

type LicenseFiles = BTreeMap<String, Vec<u8>>;

/// Normal-dependency closure of `javelin-cli` for `target`, with lock checksums and license texts.
fn dependency_graph(
    source: &Path,
    target: &str,
) -> Result<(Vec<(Dependency, LicenseFiles)>, String)> {
    let lock_bytes = read(&source.join("Cargo.lock"))?;
    let lock: toml::Table = toml::from_str(&String::from_utf8(lock_bytes.clone())?)?;
    let mut checksums = BTreeMap::new();
    for pkg in lock
        .get("package")
        .and_then(|p| p.as_array())
        .into_iter()
        .flatten()
    {
        let (Some(name), Some(version)) = (
            pkg.get("name").and_then(|v| v.as_str()),
            pkg.get("version").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        if let Some(sum) = pkg.get("checksum").and_then(|v| v.as_str()) {
            checksums.insert((name.to_string(), version.to_string()), sum.to_string());
        }
    }

    let metadata: Value = serde_json::from_str(&capture(
        Command::new("cargo")
            .args([
                "metadata",
                "--locked",
                "--format-version",
                "1",
                "--filter-platform",
                target,
            ])
            .arg("--manifest-path")
            .arg(source.join("crates/javelin-cli/Cargo.toml")),
    )?)?;
    let packages: BTreeMap<&str, &Value> = metadata["packages"]
        .as_array()
        .ok_or_else(|| crate::Error("metadata has no packages".into()))?
        .iter()
        .map(|p| (p["id"].as_str().unwrap_or_default(), p))
        .collect();
    let nodes: BTreeMap<&str, &Value> = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or_else(|| crate::Error("metadata has no resolve graph".into()))?
        .iter()
        .map(|n| (n["id"].as_str().unwrap_or_default(), n))
        .collect();
    let root = metadata["resolve"]["root"]
        .as_str()
        .ok_or_else(|| crate::Error("no resolve root".into()))?;
    let mut seen = std::collections::BTreeSet::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        for dep in nodes[id]["deps"].as_array().into_iter().flatten() {
            let normal = dep["dep_kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|k| k["kind"].is_null());
            if normal {
                stack.push(dep["pkg"].as_str().unwrap_or_default());
            }
        }
    }

    let mut out = Vec::new();
    for id in seen.into_iter().filter(|id| *id != root) {
        let pkg = packages[id];
        let name = pkg["name"].as_str().unwrap_or_default().to_string();
        let version = pkg["version"].as_str().unwrap_or_default().to_string();
        let dir = Path::new(pkg["manifest_path"].as_str().unwrap_or_default())
            .parent()
            .ok_or_else(|| crate::Error("bad manifest path".into()))?
            .to_path_buf();
        let mut files = LicenseFiles::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let file = entry.file_name().to_string_lossy().into_owned();
            let lower = file.to_lowercase();
            let wanted = ["licen", "copying", "unlicense", "notice", "copyright"]
                .iter()
                .any(|p| lower.starts_with(p));
            if wanted && entry.file_type()?.is_file() {
                files.insert(file, read(&entry.path())?);
            }
        }
        if files.is_empty() {
            bail!(
                "no license text found for {name} {version} in {}",
                dir.display()
            );
        }
        let checksum = checksums.get(&(name.clone(), version.clone())).cloned();
        out.push((
            Dependency {
                name,
                version,
                source: pkg["source"].as_str().map(str::to_string),
                checksum,
                license: pkg["license"].as_str().map(str::to_string),
            },
            files,
        ));
    }
    out.sort_by(|a, b| (&a.0.name, &a.0.version).cmp(&(&b.0.name, &b.0.version)));
    Ok((out, sha256_hex(&lock_bytes)))
}

fn notices(graph: &[(Dependency, LicenseFiles)]) -> String {
    let mut text = String::from(
        "# Third-party notices\n\n\
Javelin itself is MIT licensed; see `LICENSE`. This development build links the\n\
crates below (the normal-dependency closure for this target, a conservative superset\n\
that also lists compile-time proc-macro crates). Each crate's own license text is in\n\
`licenses/<crate>-<version>/`.\n\n\
The `rusqlite` dependency builds the bundled SQLite amalgamation (via `libsqlite3-sys`).\n\
SQLite's source states that its author disclaimed copyright and dedicates it to the\n\
public domain; `libsqlite3-sys` and `rusqlite` license their own wrapper code under MIT.\n\n\
| Crate | Version | License expression | Cargo.lock checksum |\n| --- | --- | --- | --- |\n",
    );
    for (d, _) in graph {
        text.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            d.name,
            d.version,
            d.license.as_deref().unwrap_or("(none declared)"),
            d.checksum.as_deref().unwrap_or("(none)")
        ));
    }
    text
}

fn write_tar_gz(top: &str, entries: &[Entry], epoch: u64) -> Result<Vec<u8>> {
    let gz = flate2::GzBuilder::new().write(Vec::new(), flate2::Compression::best());
    let mut tar = tar::Builder::new(gz);
    let mut dirs = std::collections::BTreeSet::from([top.to_string()]);
    for entry in entries {
        let mut path = format!("{top}/{}", entry.path);
        while let Some((parent, _)) = path.rsplit_once('/') {
            dirs.insert(parent.to_string());
            path = parent.to_string();
        }
    }
    for dir in &dirs {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_mode(0o755);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(epoch);
        header.set_size(0);
        tar.append_data(&mut header, format!("{dir}/"), std::io::empty())?;
    }
    for entry in entries {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(if entry.executable { 0o755 } else { 0o644 });
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(epoch);
        header.set_size(entry.data.len() as u64);
        tar.append_data(
            &mut header,
            format!("{top}/{}", entry.path),
            entry.data.as_slice(),
        )?;
    }
    Ok(tar.into_inner()?.finish()?)
}

fn write_zip(top: &str, entries: &[Entry], epoch: u64) -> Result<Vec<u8>> {
    let (y, mo, d, h, mi, s) = civil(epoch);
    let time = zip::DateTime::from_date_and_time(y, mo, d, h, mi, s - s % 2).map_err(|_| {
        crate::Error(format!(
            "SOURCE_DATE_EPOCH {epoch} is outside the zip date range"
        ))
    })?;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(time);
    let mut dirs = std::collections::BTreeSet::from([top.to_string()]);
    for entry in entries {
        let mut path = format!("{top}/{}", entry.path);
        while let Some((parent, _)) = path.rsplit_once('/') {
            dirs.insert(parent.to_string());
            path = parent.to_string();
        }
    }
    for dir in &dirs {
        writer.add_directory(format!("{dir}/"), options.unix_permissions(0o755))?;
    }
    for entry in entries {
        writer.start_file(
            format!("{top}/{}", entry.path),
            options.unix_permissions(if entry.executable { 0o755 } else { 0o644 }),
        )?;
        writer.write_all(&entry.data)?;
    }
    Ok(writer.finish()?.into_inner())
}

/// UTC civil date of a Unix timestamp, clamped to the zip range (1980..=2107).
fn civil(epoch: u64) -> (u16, u8, u8, u8, u8, u8) {
    let (days, secs) = ((epoch / 86_400) as i64, (epoch % 86_400) as u32);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as u16;
    if year < 1980 {
        return (1980, 1, 1, 0, 0, 0);
    }
    (
        year.min(2107),
        month,
        day,
        (secs / 3600) as u8,
        (secs % 3600 / 60) as u8,
        (secs % 60) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::civil;

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1980, 1, 1, 0, 0, 0));
        assert_eq!(civil(1_000_000_000), (2001, 9, 9, 1, 46, 40));
        assert_eq!(civil(1_781_000_000), (2026, 6, 9, 10, 13, 20));
    }
}
