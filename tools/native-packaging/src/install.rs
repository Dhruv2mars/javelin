//! Checksum-verified, owned-temp extraction into a fresh path.
use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use crate::{
    Error, Result, bail, exe_name, is_hex,
    package::{MANIFEST_FILE, Manifest},
    read, sha256_file, sha256_hex,
};

const MAX_ENTRIES: usize = 10_000;
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
/// Decompressed tar stream cap, covering headers and PAX/GNU metadata as well as payloads.
const MAX_TAR_STREAM: u64 = MAX_TOTAL_BYTES + 16 * 1024 * 1024;

pub struct Installed {
    pub dest: PathBuf,
    pub bin_dir: PathBuf,
    pub manifest: Manifest,
}

struct Extracted {
    path: String,
    is_dir: bool,
    executable: bool,
    data: Vec<u8>,
}

/// Remove the owned temp directory on every exit path.
struct Owned(PathBuf);
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn install(archive: &Path, checksum_file: &Path, dest: &Path) -> Result<Installed> {
    let bytes = read(archive)?;
    let name = archive
        .file_name()
        .ok_or_else(|| Error("archive has no file name".into()))?
        .to_string_lossy()
        .into_owned();
    verify_checksum(&bytes, &name, &String::from_utf8(read(checksum_file)?)?)?;

    let dest = crate::util::absolute(dest)?;
    if fs::symlink_metadata(&dest).is_ok() {
        bail!(
            "{} already exists; install only into a fresh path",
            dest.display()
        );
    }
    let parent = dest
        .parent()
        .ok_or_else(|| Error("destination has no parent directory".into()))?;
    fs::create_dir_all(parent)?;

    let entries = if name.ends_with(".tar.gz") {
        read_tar_gz(&bytes)?
    } else if name.ends_with(".zip") {
        read_zip(&bytes)?
    } else {
        bail!("unsupported archive type {name:?}; expected .tar.gz or .zip");
    };
    let top = validate(&entries)?;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let temp = parent.join(format!(".javelin-install-{}-{nonce}", std::process::id()));
    fs::create_dir(&temp)?;
    let guard = Owned(temp.clone());
    for entry in &entries {
        let target = temp.join(&entry.path);
        if entry.is_dir {
            fs::create_dir_all(&target)?;
            continue;
        }
        fs::create_dir_all(target.parent().unwrap())?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        std::io::Write::write_all(&mut file, &entry.data)?;
        drop(file);
        set_mode(&target, entry.executable)?;
    }

    let root = temp.join(&top);
    let manifest: Manifest = serde_json::from_slice(&read(&root.join(MANIFEST_FILE))?)?;
    if manifest.channel != "development" || !manifest.pre_ga {
        bail!("package is not marked development / pre-GA");
    }
    let expected_bin = format!("bin/{}", exe_name("javelin"));
    if manifest.binary.path != expected_bin {
        bail!(
            "manifest binary path {:?} is not {expected_bin:?}",
            manifest.binary.path
        );
    }
    let actual = sha256_file(&root.join(&manifest.binary.path))?;
    if actual != manifest.binary.sha256 {
        bail!(
            "extracted binary hash {actual} does not match manifest {}",
            manifest.binary.sha256
        );
    }
    if !root.join("LICENSE").is_file() {
        bail!("package has no LICENSE");
    }
    claim_and_rename(&root, &dest)?;
    drop(guard);
    Ok(Installed {
        bin_dir: dest.join("bin"),
        dest,
        manifest,
    })
}

fn verify_checksum(bytes: &[u8], name: &str, checksum_text: &str) -> Result<()> {
    let mut parts = checksum_text.split_whitespace();
    let (Some(expected), Some(listed), None) = (parts.next(), parts.next(), parts.next()) else {
        bail!("checksum file must contain exactly '<sha256>  <file name>'");
    };
    if !is_hex(expected, 64) {
        bail!("checksum is not 64 lowercase hex digits");
    }
    if listed.trim_start_matches('*') != name {
        bail!("checksum file names {listed:?}, archive is {name:?}");
    }
    let actual = sha256_hex(bytes);
    if actual != expected {
        bail!("checksum mismatch for {name}: expected {expected}, got {actual}");
    }
    Ok(())
}

fn read_limited(reader: &mut impl Read, total: &mut u64) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    reader.take(MAX_TOTAL_BYTES + 1).read_to_end(&mut data)?;
    *total += data.len() as u64;
    if *total > MAX_TOTAL_BYTES {
        bail!("archive expands past {MAX_TOTAL_BYTES} bytes");
    }
    Ok(data)
}

fn read_tar_gz(bytes: &[u8]) -> Result<Vec<Extracted>> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes).take(MAX_TAR_STREAM));
    let (mut out, mut total) = (Vec::new(), 0);
    for entry in archive.entries()? {
        let mut entry = entry?;
        if out.len() >= MAX_ENTRIES {
            bail!("archive has more than {MAX_ENTRIES} entries");
        }
        let path = String::from_utf8(entry.path_bytes().to_vec())
            .map_err(|_| Error("archive entry name is not UTF-8".into()))?;
        let kind = entry.header().entry_type();
        let mode = entry.header().mode().unwrap_or(0);
        let is_dir = match kind {
            tar::EntryType::Regular => false,
            tar::EntryType::Directory => true,
            other => bail!(
                "entry {path:?} has unsupported type {other:?}; only files and directories are allowed"
            ),
        };
        if is_dir && entry.header().size()? != 0 {
            bail!("directory entry {path:?} carries a payload");
        }
        let data = if is_dir {
            Vec::new()
        } else {
            read_limited(&mut entry, &mut total)?
        };
        out.push(Extracted {
            path,
            is_dir,
            executable: mode & 0o111 != 0,
            data,
        });
    }
    // The cap truncates silently; if it was reached the archive was not fully read, and
    // a cut on a header boundary would otherwise look like a clean end of archive.
    if archive.into_inner().limit() == 0 {
        bail!("archive expands past the {MAX_TAR_STREAM} byte decompressed stream cap");
    }
    Ok(out)
}

fn read_zip(bytes: &[u8]) -> Result<Vec<Extracted>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    let (mut out, mut total) = (Vec::new(), 0);
    if archive.len() > MAX_ENTRIES {
        bail!("archive has more than {MAX_ENTRIES} entries");
    }
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let path = file.name()?.to_string();
        let mode = file.unix_mode().unwrap_or(0o644);
        if mode & 0o170000 == 0o120000 {
            bail!("entry {path:?} is a symlink; only files and directories are allowed");
        }
        let is_dir = file.is_dir();
        let data = if is_dir {
            Vec::new()
        } else {
            read_limited(&mut file, &mut total)?
        };
        out.push(Extracted {
            path,
            is_dir,
            executable: mode & 0o111 != 0,
            data,
        });
    }
    Ok(out)
}

const RESERVED: &[&str] = &["con", "prn", "aux", "nul"];

fn valid_component(c: &str) -> bool {
    let stem = c.split('.').next().unwrap_or("").to_lowercase();
    let reserved = RESERVED.contains(&stem.as_str())
        || ((stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    !c.is_empty()
        && c != "."
        && c != ".."
        && c.len() <= 255
        && !reserved
        && !c.ends_with('.')
        && !c.ends_with(' ')
        && !c.chars().any(|ch| {
            ch.is_control() || matches!(ch, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
}

/// Check names, uniqueness, and shape; returns the single top-level directory.
fn validate(entries: &[Extracted]) -> Result<String> {
    if entries.is_empty() {
        bail!("archive is empty");
    }
    let mut top: Option<String> = None;
    let mut seen = HashSet::new();
    let mut files = HashSet::new();
    let mut dirs = HashSet::new();
    for entry in entries {
        let trimmed = entry.path.strip_suffix('/').unwrap_or(&entry.path);
        let parts: Vec<&str> = trimmed.split('/').collect();
        if let Some(bad) = parts.iter().find(|c| !valid_component(c)) {
            bail!("entry {:?} has unsafe component {bad:?}", entry.path);
        }
        if entry.is_dir != entry.path.ends_with('/') {
            bail!("entry {:?} has inconsistent directory marking", entry.path);
        }
        match &top {
            None => top = Some(parts[0].to_string()),
            Some(t) if t != parts[0] => {
                bail!(
                    "entry {:?} is outside the single top-level directory {t:?}",
                    entry.path
                )
            }
            _ => {}
        }
        if !seen.insert(trimmed.to_lowercase()) {
            bail!("duplicate entry {:?}", entry.path);
        }
        for end in 1..parts.len() {
            dirs.insert(parts[..end].join("/").to_lowercase());
        }
        if entry.is_dir {
            dirs.insert(trimmed.to_lowercase());
        } else {
            files.insert(trimmed.to_lowercase());
        }
    }
    if let Some(conflict) = files.intersection(&dirs).next() {
        bail!("{conflict:?} is both a file and a directory");
    }
    Ok(top.unwrap())
}

#[cfg(unix)]
fn set_mode(path: &Path, executable: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
    )?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: bool) -> Result<()> {
    Ok(())
}

/// Move the verified tree into `dest` without ever replacing something that was not ours.
/// Unix: an exclusive `create_dir` claims the path atomically, then the rename replaces
/// only that empty claim. Elsewhere rename cannot replace a directory, so it fails closed.
fn claim_and_rename(from: &Path, dest: &Path) -> Result<()> {
    if cfg!(unix) {
        fs::create_dir(dest).map_err(|e| Error(format!("cannot claim {}: {e}", dest.display())))?;
        if let Err(error) = fs::rename(from, dest) {
            let _ = fs::remove_dir(dest);
            return Err(error.into());
        }
    } else {
        fs::rename(from, dest)?;
    }
    Ok(())
}
