//! Copy-on-write tree clones, chosen by trying each primitive on the real volume.
//!
//! Order: one-call tree clone (APFS directory `clonefile`, btrfs subvolume snapshot),
//! then per-file clone (`FICLONE`, per-file `clonefile`, Windows `CopyFile` block
//! clone verified by shared clusters), then a byte copy that reports itself.
#[cfg(windows)]
use std::path::PathBuf;
use std::{fs, io, path::Path};

use serde::Serialize;

/// How a tree was created. `Copy` duplicated file data; every other strategy shares it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CloneStrategy {
    /// One APFS `clonefile(2)` of the whole directory.
    DirectoryClone,
    /// One btrfs subvolume snapshot.
    Snapshot,
    /// A per-file copy-on-write clone (`FICLONE`, `clonefile`, ReFS block clone).
    FileClone,
    /// A byte copy. Unchanged data is duplicated.
    Copy,
}

impl CloneStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DirectoryClone => "directory_clone",
            Self::Snapshot => "snapshot",
            Self::FileClone => "file_clone",
            Self::Copy => "copy",
        }
    }
}

/// The outcome of one tree clone: the strategy that worked, why faster ones did not,
/// and how much file data was duplicated.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CloneReport {
    pub strategy: CloneStrategy,
    /// Why each faster strategy was not used, when one was tried and failed.
    pub fallback: Option<String>,
    /// File bytes written by a copy. Zero when every file was cloned.
    pub copied_bytes: u64,
    /// Entries visited by a per-file walk. Zero for a one-call tree clone.
    pub entries: u64,
}

/// Clones `src` to the new path `dst`. `src` must be readable; `dst` must not exist.
pub(crate) fn clone_tree(src: &Path, dst: &Path) -> io::Result<CloneReport> {
    let mut reasons = Vec::new();
    if let Some(strategy) = whole_tree(src, dst, &mut reasons)? {
        return Ok(CloneReport {
            strategy,
            fallback: joined(reasons),
            copied_bytes: 0,
            entries: 0,
        });
    }
    let mut walk = Walk {
        clone: file_clone_available(dst, &mut reasons)?,
        tried: false,
        reasons,
        copied: 0,
        entries: 0,
    };
    walk.tree(src, dst)?;
    Ok(CloneReport {
        strategy: if walk.clone {
            CloneStrategy::FileClone
        } else {
            CloneStrategy::Copy
        },
        fallback: joined(walk.reasons),
        copied_bytes: walk.copied,
        entries: walk.entries,
    })
}

fn joined(reasons: Vec<String>) -> Option<String> {
    (!reasons.is_empty()).then(|| reasons.join("; "))
}

/// Errors that mean "this volume cannot do that", as opposed to a real failure.
fn unsupported(error: &io::Error) -> bool {
    #[cfg(unix)]
    if let Some(code) = error.raw_os_error() {
        return [
            libc::ENOTSUP,
            libc::EOPNOTSUPP,
            libc::EXDEV,
            libc::EINVAL,
            libc::ENOTTY,
            libc::ENOSYS,
            libc::EPERM,
        ]
        .contains(&code);
    }
    error.kind() == io::ErrorKind::Unsupported
}

struct Walk {
    clone: bool,
    /// The first file decides: an unsupported clone there switches to copying.
    tried: bool,
    reasons: Vec<String>,
    copied: u64,
    entries: u64,
}

impl Walk {
    fn tree(&mut self, src: &Path, dst: &Path) -> io::Result<()> {
        fs::create_dir(dst)?;
        self.entries += 1;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let (from, to) = (entry.path(), dst.join(entry.file_name()));
            let kind = entry.file_type()?;
            if kind.is_dir() {
                self.tree(&from, &to)?;
            } else if kind.is_symlink() {
                super::symlink(&fs::read_link(&from)?, &to)?;
                self.entries += 1;
            } else {
                self.file(&from, &to)?;
            }
        }
        fs::set_permissions(dst, fs::symlink_metadata(src)?.permissions())
    }

    fn file(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        self.entries += 1;
        if self.clone {
            let first = !std::mem::replace(&mut self.tried, true);
            match clone_file(from, to) {
                Ok(()) => return Ok(()),
                Err(e) if first && unsupported(&e) => {
                    let _ = fs::remove_file(to);
                    self.reasons
                        .push(format!("per-file clone unavailable: {e}"));
                    self.clone = false;
                }
                Err(e) => return Err(e),
            }
        }
        self.copied += copy_file(from, to)?;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn whole_tree(
    src: &Path,
    dst: &Path,
    reasons: &mut Vec<String>,
) -> io::Result<Option<CloneStrategy>> {
    match clonefile(src, dst) {
        Ok(()) => Ok(Some(CloneStrategy::DirectoryClone)),
        Err(e) if unsupported(&e) => {
            reasons.push(format!("directory clonefile unavailable: {e}"));
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

#[cfg(target_os = "linux")]
fn whole_tree(
    src: &Path,
    dst: &Path,
    reasons: &mut Vec<String>,
) -> io::Result<Option<CloneStrategy>> {
    match btrfs::snapshot(src, dst) {
        Ok(()) => Ok(Some(CloneStrategy::Snapshot)),
        Err(e) if unsupported(&e) => {
            reasons.push(format!("subvolume snapshot unavailable: {e}"));
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn whole_tree(_: &Path, _: &Path, _: &mut Vec<String>) -> io::Result<Option<CloneStrategy>> {
    Ok(None)
}

/// Unix per-file clones are tried on the first file and abandoned if unsupported.
#[cfg(unix)]
fn file_clone_available(_: &Path, _: &mut Vec<String>) -> io::Result<bool> {
    Ok(cfg!(any(target_os = "macos", target_os = "linux")))
}

#[cfg(target_os = "macos")]
fn clonefile(src: &Path, dst: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let src = CString::new(src.as_os_str().as_bytes())?;
    let dst = CString::new(dst.as_os_str().as_bytes())?;
    // CLONE_NOFOLLOW: clone symlinks themselves. Without CLONE_ACL the clone inherits
    // the destination directory's ACLs, so nothing copies base protection over.
    if unsafe { libc::clonefile(src.as_ptr(), dst.as_ptr(), 0x0001) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn clone_file(src: &Path, dst: &Path) -> io::Result<()> {
    clonefile(src, dst)
}

#[cfg(target_os = "linux")]
fn clone_file(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
    let from = fs::File::open(src)?;
    let mode = from.metadata()?.permissions();
    let to = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dst)?;
    if unsafe { libc::ioctl(to.as_raw_fd(), libc::FICLONE, from.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    to.set_permissions(mode)
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn clone_file(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// A plain byte copy that keeps permissions; returns the bytes written.
#[cfg(unix)]
fn copy_file(src: &Path, dst: &Path) -> io::Result<u64> {
    fs::copy(src, dst)
}

/// `CopyFileExW` block-clones on ReFS and Dev Drive (Windows 11 24H2 and Server
/// 2025) and copies elsewhere. The walk uses it for both; the probe decides the label.
#[cfg(windows)]
fn clone_file(src: &Path, dst: &Path) -> io::Result<()> {
    fs::copy(src, dst).map(drop)
}

#[cfg(windows)]
fn copy_file(src: &Path, dst: &Path) -> io::Result<u64> {
    fs::copy(src, dst)
}

/// Windows: copy a probe file next to the destination and compare the first logical
/// cluster of both files. Shared clusters prove `CopyFile` cloned rather than copied.
#[cfg(windows)]
fn file_clone_available(dst: &Path, reasons: &mut Vec<String>) -> io::Result<bool> {
    let parent = dst.parent().unwrap_or(dst);
    let (from, to) = (probe_path(parent, "src"), probe_path(parent, "dst"));
    let result = (|| {
        // Large enough to need real clusters on every volume; small files can live in metadata.
        fs::write(&from, vec![0x5a; 256 * 1024])?;
        fs::copy(&from, &to)?;
        Ok::<_, io::Error>((windows::first_cluster(&from)?, windows::first_cluster(&to)?))
    })();
    let _ = fs::remove_file(&from);
    let _ = fs::remove_file(&to);
    match result {
        Ok((a, b)) if a == b => Ok(true),
        Ok(_) => {
            reasons.push("CopyFile did not share clusters on this volume".into());
            Ok(false)
        }
        Err(e) => {
            reasons.push(format!("block clone could not be verified: {e}"));
            Ok(false)
        }
    }
}

#[cfg(windows)]
fn probe_path(parent: &Path, name: &str) -> PathBuf {
    parent.join(format!(".clone-probe-{name}-{}", std::process::id()))
}

#[cfg(windows)]
mod windows {
    use std::{ffi::c_void, fs, io, os::windows::io::AsRawHandle, path::Path};

    const FSCTL_GET_RETRIEVAL_POINTERS: u32 = 0x0009_0073;
    const ERROR_MORE_DATA: i32 = 234;

    unsafe extern "system" {
        fn DeviceIoControl(
            device: *mut c_void,
            code: u32,
            input: *const c_void,
            input_size: u32,
            output: *mut c_void,
            output_size: u32,
            returned: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
    }

    /// The LCN of a file's first extent (`RETRIEVAL_POINTERS_BUFFER.Extents[0].Lcn`).
    pub(super) fn first_cluster(path: &Path) -> io::Result<i64> {
        let file = fs::File::open(path)?;
        let start: i64 = 0;
        // ExtentCount (u32) + padding, StartingVcn (i64), then one {NextVcn, Lcn} pair.
        let mut out = [0i64; 4];
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_GET_RETRIEVAL_POINTERS,
                (&start as *const i64).cast(),
                8,
                out.as_mut_ptr().cast(),
                32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_MORE_DATA) {
                return Err(error);
            }
        }
        if out[0] as u32 == 0 {
            return Err(io::Error::other("file has no allocated extents"));
        }
        Ok(out[3])
    }
}

/// btrfs subvolumes through ioctls. Creating and snapshotting a subvolume you own
/// needs no privilege; deletion uses ordinary unlink and `rmdir` (Linux 4.18+).
#[cfg(target_os = "linux")]
pub(crate) mod btrfs {
    use std::{
        ffi::CString,
        fs, io,
        os::{fd::AsRawFd, unix::ffi::OsStrExt},
        path::Path,
    };

    #[repr(C)]
    struct VolArgs {
        fd: i64,
        name: [u8; 4088],
    }

    const SNAP_CREATE: libc::Ioctl = libc::_IOW::<VolArgs>(0x94, 1);
    const SUBVOL_CREATE: libc::Ioctl = libc::_IOW::<VolArgs>(0x94, 14);

    fn call(request: libc::Ioctl, source: i64, dst: &Path) -> io::Result<()> {
        let parent = fs::File::open(dst.parent().ok_or(io::ErrorKind::InvalidInput)?)?;
        let name = CString::new(
            dst.file_name()
                .ok_or(io::ErrorKind::InvalidInput)?
                .as_bytes(),
        )?;
        let mut args = VolArgs {
            fd: source,
            name: [0; 4088],
        };
        let bytes = name.as_bytes_with_nul();
        if bytes.len() > args.name.len() {
            return Err(io::ErrorKind::InvalidFilename.into());
        }
        args.name[..bytes.len()].copy_from_slice(bytes);
        if unsafe { libc::ioctl(parent.as_raw_fd(), request, &args) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Writable snapshot of the subvolume `src` at the new path `dst`.
    pub(super) fn snapshot(src: &Path, dst: &Path) -> io::Result<()> {
        let source = fs::File::open(src)?;
        call(SNAP_CREATE, source.as_raw_fd().into(), dst)
    }

    /// An empty subvolume at the new path `dst`, so later bases and Layers can snapshot it.
    pub(crate) fn create(dst: &Path) -> io::Result<()> {
        call(SUBVOL_CREATE, 0, dst)
    }
}

/// Makes a fresh empty directory for a base: a subvolume where btrfs allows it.
pub(crate) fn create_base_dir(dst: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    match btrfs::create(dst) {
        Ok(()) => return Ok(()),
        Err(e) if unsupported(&e) => {}
        Err(e) => return Err(e),
    }
    fs::create_dir(dst)
}

/// Flushes everything written under `dir` before it is published by rename.
/// Linux flushes the whole filesystem in one `syncfs`; elsewhere writers sync each file.
pub(crate) fn barrier(dir: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let handle = fs::File::open(dir)?;
        if unsafe { libc::syncfs(handle.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = dir;
    Ok(())
}

/// Whether file writers must sync each file themselves (no filesystem-wide barrier).
pub(crate) const SYNC_EACH_FILE: bool = !cfg!(target_os = "linux");
