//! Fresh, minimal measurements for the Layer workspace decision (docs/workspace-approach.md).
//!
//! layer-bench --dir SCRATCH --label NAME [--files 10000,100000] [--reps 5] [--btrfs]
//!
//! For each fixture size it generates a deterministic repository-shaped tree on the
//! SCRATCH volume, then measures every available Layer strategy: create time, disk
//! delta, first status scan, rescan after 10 edits (and that exactly 10 are found), and
//! discard time. Nothing here is product code; Javelin never runs this.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, UNIX_EPOCH};

type Cache = HashMap<PathBuf, (u64, u128, u64)>;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn unit(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }
    /// Log-normal file size (median about 3 KB, long tail, capped at 2 MB).
    fn size(&mut self) -> usize {
        let z = (-2.0 * self.unit().ln()).sqrt() * (2.0 * std::f64::consts::PI * self.unit()).cos();
        ((8.0 + 1.3 * z).exp() as usize).min(2_000_000)
    }
}

fn fixture(root: &Path, files: usize, seed: u64) -> u64 {
    fs::create_dir_all(root).unwrap();
    let mut rng = Rng(seed);
    let mut dirs = vec![root.to_path_buf()];
    let mut buf = Vec::new();
    let mut bytes = 0;
    for i in 0..files {
        if i % 20 == 0 {
            let recent = &dirs[dirs.len().saturating_sub(50)..];
            let parent = recent[rng.next() as usize % recent.len()].clone();
            let depth = parent.strip_prefix(root).unwrap().components().count();
            let dir = if depth < 4 {
                parent
            } else {
                root.to_path_buf()
            }
            .join(format!("d{}", i / 20));
            fs::create_dir_all(&dir).unwrap();
            dirs.push(dir);
        }
        let size = rng.size();
        buf.clear();
        while buf.len() < size {
            buf.extend_from_slice(&rng.next().to_le_bytes());
        }
        buf.truncate(size);
        fs::write(dirs.last().unwrap().join(format!("f{i}.txt")), &buf).unwrap();
        bytes += size as u64;
    }
    bytes
}

/// Plain byte copy. Avoids fs::copy and io::copy, which may clone or reflink on their own.
fn copy_bytes(src: &Path, dst: &Path) -> std::io::Result<()> {
    let (mut r, mut w) = (fs::File::open(src)?, fs::File::create(dst)?);
    let mut buf = vec![0; 1 << 16];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        w.write_all(&buf[..n])?;
    }
}

fn tree(
    src: &Path,
    dst: &Path,
    file: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    fs::create_dir(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            tree(&e.path(), &to, file)?;
        } else {
            file(&e.path(), &to)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn clone_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let (s, d) = (
        CString::new(src.as_os_str().as_bytes())?,
        CString::new(dst.as_os_str().as_bytes())?,
    );
    // CLONE_NOFOLLOW; APFS clones a directory recursively in one call.
    match unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), 1) } {
        0 => Ok(()),
        _ => Err(std::io::Error::last_os_error()),
    }
}

fn btrfs(args: &[&str]) -> std::io::Result<()> {
    let out = Command::new("btrfs").args(args).output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ))
    }
}

fn scan(root: &Path, cache: &mut Cache) {
    for e in fs::read_dir(root).unwrap() {
        let e = e.unwrap();
        let m = e.metadata().unwrap();
        if m.is_dir() {
            scan(&e.path(), cache);
        } else {
            let mtime = m
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            #[cfg(unix)]
            let ino = std::os::unix::fs::MetadataExt::ino(&m);
            #[cfg(not(unix))]
            let ino = 0;
            cache.insert(e.path(), (m.len(), mtime, ino));
        }
    }
}

fn sync() {
    #[cfg(unix)]
    let _ = Command::new("sync").status();
}

fn avail(dir: &Path) -> i64 {
    fs4::available_space(dir).unwrap() as i64
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if v.is_empty() {
        f64::NAN
    } else {
        v[v.len() / 2]
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .map(|i| args[i + 1].clone())
    };
    let dir = PathBuf::from(flag("--dir").expect("--dir SCRATCH"));
    let label = flag("--label").expect("--label NAME");
    let reps: usize = flag("--reps").map_or(5, |r| r.parse().unwrap());
    let sizes: Vec<usize> = flag("--files")
        .unwrap_or("10000,100000".into())
        .split(',')
        .map(|s| s.parse().unwrap())
        .collect();
    let use_btrfs = args.iter().any(|a| a == "--btrfs");
    fs::create_dir_all(&dir).unwrap();

    type Make = Box<dyn Fn(&Path, &Path) -> std::io::Result<()>>;
    let mut strategies: Vec<(&str, Make)> = vec![
        ("copy", Box::new(|s, d| tree(s, d, &copy_bytes))),
        (
            "clone-files",
            Box::new(|s, d| tree(s, d, &|a, b| reflink_copy::reflink(a, b))),
        ),
        // Hybrid: copy small files (a few clusters each), clone the ones that hold most bytes.
        (
            "clone-large",
            Box::new(|s, d| {
                tree(s, d, &|a, b| {
                    if fs::metadata(a)?.len() >= 16 * 1024 {
                        reflink_copy::reflink(a, b)
                    } else {
                        copy_bytes(a, b)
                    }
                })
            }),
        ),
    ];
    #[cfg(target_os = "macos")]
    strategies.push(("clone-dir", Box::new(clone_dir)));
    if use_btrfs {
        strategies.push((
            "btrfs-snapshot",
            Box::new(|s, d| {
                btrfs(&[
                    "subvolume",
                    "snapshot",
                    s.to_str().unwrap(),
                    d.to_str().unwrap(),
                ])
            }),
        ));
    }

    println!(
        "| filesystem | files | strategy | create ms | disk MB | scan ms | rescan ms (10 edits) | discard ms |"
    );
    println!("| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: |");
    for &files in &sizes {
        let base = dir.join(format!("base-{files}"));
        if use_btrfs {
            btrfs(&["subvolume", "create", base.to_str().unwrap()]).unwrap();
        }
        let bytes = fixture(&base, files, files as u64);
        for (name, make) in &strategies {
            let (mut create, mut disk, mut first, mut rescan, mut discard) =
                (vec![], vec![], vec![], vec![], vec![]);
            let mut note = String::new();
            for i in 0..reps {
                let layer = dir.join(format!("{name}-{files}-{i}"));
                sync();
                let before = avail(&dir);
                let t = Instant::now();
                if let Err(e) = make(&base, &layer) {
                    note = format!("unsupported: {e}");
                    let _ = fs::remove_dir_all(&layer);
                    break;
                }
                create.push(ms(t));
                sync();
                std::thread::sleep(std::time::Duration::from_millis(500));
                disk.push((before - avail(&dir)) as f64 / 1e6);

                let mut cache = Cache::new();
                let t = Instant::now();
                scan(&layer, &mut cache);
                first.push(ms(t));
                assert_eq!(cache.len(), files);
                let mut paths: Vec<_> = cache.keys().cloned().collect();
                paths.sort();
                for p in &paths[..10] {
                    fs::OpenOptions::new()
                        .append(true)
                        .open(p)
                        .unwrap()
                        .write_all(b"x")
                        .unwrap();
                }
                let mut after = Cache::new();
                let t = Instant::now();
                scan(&layer, &mut after);
                let changed = after
                    .iter()
                    .filter(|(p, v)| cache.get(*p) != Some(v))
                    .count();
                rescan.push(ms(t));
                assert_eq!(changed, 10, "{name}: stat cache missed edits");

                let t = Instant::now();
                if *name == "btrfs-snapshot"
                    && btrfs(&["subvolume", "delete", layer.to_str().unwrap()]).is_err()
                {
                    note = "unprivileged subvolume delete refused; removed by emptying then rmdir"
                        .into();
                    fs::remove_dir_all(&layer).unwrap();
                } else if layer.exists() {
                    fs::remove_dir_all(&layer).unwrap();
                }
                discard.push(ms(t));
            }
            println!(
                "| {label} | {files} ({:.0} MB) | {name} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {note}",
                bytes as f64 / 1e6,
                median(&mut create),
                median(&mut disk),
                median(&mut first),
                median(&mut rescan),
                median(&mut discard),
            );
        }
        if use_btrfs {
            let _ = btrfs(&["subvolume", "delete", base.to_str().unwrap()]);
        }
        let _ = fs::remove_dir_all(&base);
    }
}
