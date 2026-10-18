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
    if src.is_dir() {
        fs::set_permissions(dst, fs::Permissions::from_mode(0o755))
    } else {
        writable(dst)
    }
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
        return clone_file(src, dst);
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

pub fn settled(p: &Path) -> io::Result<i64> {
    unsafe {
        libc::sync();
    }
    let mut prev = available(p)?;
    // Bounded APFS lazy-accounting settle; failure is explicit, never a noisy number.
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(200));
        let now = available(p)?;
        if now == prev {
            return Ok(now);
        }
        prev = now;
    }
    Err(io::Error::other("free space did not settle within 20s"))
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
pub fn shared_bytes(p: &Path) -> io::Result<(u64, u64)> {
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
        flags: 1,
        mapped: 0,
        count: 32,
        reserved: 0,
        extents: [Extent::default(); 32],
    };
    let mut total = 0;
    let mut shared = 0;
    loop {
        if unsafe { libc::ioctl(file.as_raw_fd(), 0xc020660b as libc::c_ulong, &mut map) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if map.mapped == 0 {
            break;
        }
        for e in &map.extents[..map.mapped as usize] {
            total += e.length;
            if e.flags & 0x2000 != 0 {
                shared += e.length;
            }
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
    if total == 0 || shared != total {
        return Err(io::Error::other(format!(
            "FIEMAP: shared {shared}/{total} extent bytes"
        )));
    }
    Ok((shared, total))
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

#[cfg(target_os = "linux")]
pub fn shared(p: &Path) -> io::Result<()> {
    shared_bytes(p).map(|_| ())
}
