use crate::error::{Context, Result};
#[cfg(not(windows))]
use std::fs::File;
#[cfg(windows)]
use std::fs::OpenOptions;
use std::path::Path;

#[cfg(unix)]
pub fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .jctx(
            7,
            "DURABILITY_IO",
            format!("cannot sync {}", path.display()),
        )
}

#[cfg(windows)]
pub fn sync_dir(path: &Path) -> Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

    OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .and_then(|directory| directory.sync_all())
        .jctx(
            7,
            "DURABILITY_IO",
            format!("cannot sync {}", path.display()),
        )
}

#[cfg(not(any(unix, windows)))]
pub fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .jctx(
            7,
            "DURABILITY_IO",
            format!("cannot sync {}", path.display()),
        )
}
