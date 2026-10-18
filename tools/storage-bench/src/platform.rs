use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

pub fn writable(p: &Path) -> io::Result<()> {
    fs::set_permissions(p, fs::Permissions::from_mode(0o644))
}

/// Never fs::copy or io::copy: neither promises a non-reflink byte copy.
pub fn copy_bytes(src: &Path, dst: &Path) -> io::Result<()> {
    let (mut r, mut w) = (File::open(src)?, File::create(dst)?);
    let mut buf = [0; 65536];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        w.write_all(&buf[..n])?;
    }
}

#[cfg(target_os = "macos")]
pub fn clone_file(src: &Path, dst: &Path) -> io::Result<()> {
    let s = CString::new(src.as_os_str().as_bytes())?;
    let d = CString::new(dst.as_os_str().as_bytes())?;
    if unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), 1) } != 0 {
        return Err(io::Error::last_os_error());
    }
    writable(dst)
}

#[cfg(target_os = "linux")]
pub fn clone_file(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let s = File::open(src)?;
    let d = File::create_new(dst)?;
    if unsafe { libc::ioctl(d.as_raw_fd(), 0x40049409 as libc::c_ulong, s.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    writable(dst)
}

pub fn layer(src: &Path, dst: &Path, clones: bool) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    if clones {
        let s = CString::new(src.as_os_str().as_bytes())?;
        let d = CString::new(dst.as_os_str().as_bytes())?;
        if unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), 1) } != 0 {
            return Err(io::Error::last_os_error());
        }
        return fs::set_permissions(dst, fs::Permissions::from_mode(0o755));
    }
    tree(src, dst, clones)
}

pub fn tree(src: &Path, dst: &Path, clones: bool) -> io::Result<()> {
    fs::create_dir(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            tree(&e.path(), &to, clones)?;
        } else if clones {
            clone_file(&e.path(), &to)?;
        } else {
            copy_bytes(&e.path(), &to)?;
        }
    }
    Ok(())
}

pub fn available(p: &Path) -> io::Result<i64> {
    let path = CString::new(p.as_os_str().as_bytes())?;
    #[cfg(target_os = "macos")]
    {
        let mut s = std::mem::MaybeUninit::<libc::statfs>::uninit();
        if unsafe { libc::statfs(path.as_ptr(), s.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let s = unsafe { s.assume_init() };
        Ok((s.f_bavail * s.f_bsize as u64) as i64)
    }
    #[cfg(target_os = "linux")]
    {
        let mut s = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::statvfs(path.as_ptr(), s.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let s = unsafe { s.assume_init() };
        Ok((s.f_bavail * s.f_frsize) as i64)
    }
}

// Kept in the raw artifact, not in the compact committed summary.
static ACCOUNTING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub fn accounting() -> String {
    ACCOUNTING.lock().unwrap().join(",\n")
}

#[cfg(target_os = "linux")]
fn btrfs(p: &Path) -> io::Result<bool> {
    let path = CString::new(p.as_os_str().as_bytes())?;
    let mut s = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::statfs(path.as_ptr(), s.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { s.assume_init() }.f_type == 0x9123683e)
}

/// Sum used Data/Metadata/System with profile replication. Chunk capacity and
/// GlobalReserve (a subset of metadata) are deliberately not counted as used.
#[cfg(target_os = "linux")]
fn btrfs_used(raw: &str) -> io::Result<i64> {
    let mut total = 0;
    let mut profiles = 0;
    for line in raw.lines() {
        if !["Data,", "Metadata,", "System,"]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        {
            continue;
        }
        let (profile, values) = line
            .split_once(':')
            .ok_or_else(|| io::Error::other("btrfs usage profile missing"))?;
        let multiplier = match profile.split(',').nth(1) {
            Some("single") => 1,
            Some("DUP" | "RAID1") => 2,
            _ => {
                return Err(io::Error::other(format!(
                    "unsupported btrfs profile: {profile}"
                )));
            }
        };
        let used: i64 = values
            .split_once("Used:")
            .ok_or_else(|| io::Error::other("btrfs Used missing"))?
            .1
            .split_whitespace()
            .next()
            .ok_or_else(|| io::Error::other("empty btrfs Used value"))?
            .parse()
            .map_err(io::Error::other)?;
        total += used * multiplier;
        profiles += 1;
    }
    if profiles < 3 {
        return Err(io::Error::other("missing btrfs Data/Metadata/System usage"));
    }
    Ok(total)
}

pub fn settled(p: &Path) -> io::Result<i64> {
    unsafe {
        libc::sync();
    }
    #[cfg(target_os = "linux")]
    let physical = btrfs(p)?;
    #[cfg(target_os = "macos")]
    let physical = false;
    let mut readings = Vec::new();
    let mut statfs = Vec::new();
    #[cfg(target_os = "linux")]
    let mut raw = String::new();
    #[cfg(target_os = "macos")]
    let raw = String::new();
    let mut equal = 0;
    let mut previous = None;
    // Five equal readings spanning at least two seconds; bounded at 60 seconds.
    let start = std::time::Instant::now();
    loop {
        statfs.push(available(p)?);
        #[cfg(target_os = "linux")]
        let now = if physical {
            let output = Command::new("sudo")
                .args(["-n", "btrfs", "filesystem", "usage", "--raw"])
                .arg(p)
                .output()?;
            if !output.status.success() {
                return Err(io::Error::other(format!(
                    "btrfs filesystem usage: {}",
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
            raw = String::from_utf8_lossy(&output.stdout).into_owned();
            -btrfs_used(&raw)?
        } else {
            *statfs.last().unwrap()
        };
        #[cfg(target_os = "macos")]
        let now = *statfs.last().unwrap();
        equal = if previous == Some(now) { equal + 1 } else { 1 };
        previous = Some(now);
        readings.push(now);
        if equal >= 5 || start.elapsed() >= Duration::from_secs(60) {
            ACCOUNTING.lock().unwrap().push(format!(
                "{{\"path\":{},\"method\":{},\"elapsed_ms\":{},\"readings\":{:?},\"statfs_available\":{:?},\"btrfs_usage_raw\":{}}}",
                crate::report::quote(&p.display().to_string()),
                crate::report::quote(if physical { "negative physical used: Data+Metadata+System, profile replication included" } else { "statfs available" }),
                start.elapsed().as_millis(), readings, statfs, crate::report::quote(&raw)
            ));
            return if equal >= 5 {
                Ok(now)
            } else {
                Err(io::Error::other("allocation did not plateau within 60s"))
            };
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub fn apparent(p: &Path) -> io::Result<u64> {
    let mut n = 0;
    for e in fs::read_dir(p)? {
        let e = e?;
        n += if e.file_type()?.is_dir() {
            apparent(&e.path())?
        } else {
            e.metadata()?.len()
        };
    }
    Ok(n)
}

pub fn cold(hook: &str) -> io::Result<()> {
    let status = Command::new("sh").args(["-c", hook]).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("cache hook failed: {status}")))
    }
}

#[cfg(target_os = "linux")]
pub fn extents(p: &Path) -> io::Result<Extents> {
    use std::os::fd::AsRawFd;
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Extent {
        logical: u64,
        physical: u64,
        length: u64,
        reserved: [u64; 2],
        flags: u32,
        tail: [u32; 3],
    }
    #[repr(C)]
    struct Map {
        start: u64,
        length: u64,
        flags: u32,
        mapped: u32,
        count: u32,
        reserved: u32,
        extents: [Extent; 32],
    }
    let file = File::open(p)?;
    let mut map = Map {
        start: 0,
        length: u64::MAX,
        flags: 0, // callers synced the filesystem; do not sync each file again
        mapped: 0,
        count: 32,
        reserved: 0,
        extents: [Extent::default(); 32],
    };
    let size = file.metadata()?.len();
    let mut result = Extents::default();
    loop {
        if unsafe { libc::ioctl(file.as_raw_fd(), 0xc020660b as libc::c_ulong, &mut map) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if map.mapped == 0 {
            break;
        }
        for e in &map.extents[..map.mapped as usize] {
            let bytes = (size.saturating_sub(e.logical)).min(e.length);
            if e.flags & 0x200 != 0 {
                // FIEMAP_EXTENT_DATA_INLINE
                result.inline += bytes;
            } else if e.flags & 0x2000 != 0 {
                // FIEMAP_EXTENT_SHARED
                result.shared += bytes;
            } else {
                result.unshared += bytes;
            }
            result.extents += 1;
        }
        let last = map.extents[map.mapped as usize - 1];
        if last.flags & 1 != 0 {
            break;
        }
        let next = last.logical + last.length;
        if next <= map.start {
            return Err(io::Error::other("FIEMAP failed to advance"));
        }
        map.start = next;
        map.length = u64::MAX - next;
        map.mapped = 0;
    }
    result.unmapped = size.saturating_sub(result.inline + result.shared + result.unshared);
    Ok(result)
}

#[cfg(target_os = "linux")]
#[derive(Default)]
pub struct Extents {
    pub inline: u64,
    pub shared: u64,
    pub unshared: u64,
    pub unmapped: u64,
    pub extents: u64,
}

/// Rust File::sync_all uses F_FULLFSYNC on Apple; call plain fsync explicitly
/// to match SQLite FULL with fullfsync=OFF. Retry interrupted syscalls.
pub fn fsync(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    loop {
        if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn counts_used_with_metadata_dup_and_ignores_capacity() {
        let usage = "Data,single: Size:1073741824, Used:100 (0.00%)\nMetadata,DUP: Size:268435456, Used:30 (0.00%)\nSystem,DUP: Size:8388608, Used:10 (0.00%)\nGlobalReserve,single: Size:16777216, Used:0 (0.00%)";
        assert_eq!(super::btrfs_used(usage).unwrap(), 180);
        assert!(super::btrfs_used("Data,single: Size:1, Used:1").is_err());
    }
}
