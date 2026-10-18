//! Standalone measurement harness; never called by Javelin.
mod corpus;
mod extras;
mod measure;
mod platform;
mod report;
mod store;

use corpus::fixture;
use javelin::ObjectId;
use measure::{disk, eviction, operations};
use report::{Output, percentile};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};
use store::{Kind, Store};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn command(program: &str, args: &[&str]) -> String {
    match Command::new(program).args(args).output() {
        Ok(o) => format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
        .trim()
        .into(),
        Err(e) => e.to_string(),
    }
}
fn specs(dir: &Path, label: &str, cache: Option<&str>) -> String {
    #[cfg(target_os = "macos")]
    let os = format!(
        "{}\nCPU: {}\nRAM bytes: {}",
        command("sw_vers", &[]),
        command("sysctl", &["-n", "machdep.cpu.brand_string"]),
        command("sysctl", &["-n", "hw.memsize"])
    );
    #[cfg(target_os = "linux")]
    let os = format!(
        "{}\n{}\n{}",
        fs::read_to_string("/etc/os-release").unwrap_or_default(),
        command("lscpu", &[]),
        fs::read_to_string("/proc/meminfo").unwrap_or_default()
    );
    #[cfg(target_os = "macos")]
    let filesystem = {
        let df = command("df", &["-P", dir.to_str().unwrap()]);
        let device = df
            .lines()
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .unwrap_or(dir.to_str().unwrap());
        command("diskutil", &["info", device])
    };
    #[cfg(target_os = "linux")]
    let filesystem = command("findmnt", &["-T", dir.to_str().unwrap()]);
    let os = format!("{os}\n{filesystem}");
    format!(
        "{label}\n{os}\n{}\n{}\n{}\ncache hook: {}\nArgs: {:?}",
        command("uname", &["-a"]),
        command("rustc", &["--version"]),
        command("df", &["-h", dir.to_str().unwrap()]),
        cache.unwrap_or("none: cold unmeasured"),
        std::env::args().collect::<Vec<_>>()
    )
}
fn remove(p: &Path) -> Result<()> {
    if p.exists() {
        fs::remove_dir_all(p)?;
    }
    Ok(())
}
fn elapsed(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// Exercise SIGKILL exactly while the next object exists only in tmp/.
fn crash_child(root: PathBuf) -> Result<()> {
    use std::io::Write;
    let s = Store {
        root,
        kind: Kind::Files,
        durable: true,
        page: 4096,
        incremental: false,
    };
    s.init()?;
    s.ingest(&fixture(100, 100), None)?;
    let mut f = fs::File::create(s.root.join("tmp/interrupted"))?;
    f.write_all(b"partial next blob")?;
    platform::fsync(&f)?;
    fs::write(s.root.join("ready"), b"kill me")?;
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
fn crash_check(root: &Path) -> Result<()> {
    let mut child = Command::new(std::env::current_exe()?)
        .arg("--crash-child")
        .arg(root)
        .spawn()?;
    let start = Instant::now();
    while !root.join("ready").exists() {
        if let Some(status) = child.try_wait()? {
            return Err(format!("crash child exited: {status}").into());
        }
        if start.elapsed().as_secs() > 30 {
            child.kill()?;
            child.wait()?;
            return Err("crash child failed to reach boundary".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    child.kill()?;
    child.wait()?;
    assert!(root.join("tmp/interrupted").exists());
    let s = Store {
        root: root.into(),
        kind: Kind::Files,
        durable: true,
        page: 4096,
        incremental: false,
    };
    for b in fixture(100, 100) {
        assert_eq!(ObjectId::for_blob(&fs::read(s.object(b.id))?), b.id);
    }
    let finals: usize = fs::read_dir(root.join("objects"))?
        .map(|d| fs::read_dir(d.unwrap().path()).unwrap().count())
        .sum();
    assert_eq!(finals, 100);
    fs::remove_file(root.join("tmp/interrupted"))?;
    remove(root)
}

fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "--kill-writer") {
        return extras::child(
            args.get(2).ok_or("writer kind missing")?,
            PathBuf::from(args.get(3).ok_or("writer path missing")?),
        );
    }
    if args.get(1).is_some_and(|s| s == "--crash-child") {
        return crash_child(PathBuf::from(args.get(2).ok_or("crash path missing")?));
    }
    let flag = |key: &str| {
        args.iter()
            .position(|a| a == key)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    if args.iter().any(|a| a == "--help") {
        println!(
            "storage-bench --dir NEW_SCRATCH --files 1000,10000,100000 --reps 5 --label FS [--json out.json] [--disk-only | --extras-only] [--stores A-real,...] [--real10k-ms MS] [--page-size 4096|16384] [--ingest-dir NEW_NATIVE_SCRATCH] [--drop-caches-cmd 'sync; ...']"
        );
        return Ok(());
    }
    let scratch = PathBuf::from(flag("--dir").ok_or("--dir required (must not exist)")?);
    if scratch.exists() {
        return Err("scratch exists; choose a fresh path (never deletes preexisting data)".into());
    }
    fs::create_dir_all(&scratch)?;
    let ingest_dir = flag("--ingest-dir").map(PathBuf::from);
    if let Some(p) = &ingest_dir {
        if p.exists() {
            return Err("ingest dir exists; choose a fresh path".into());
        }
        fs::create_dir_all(p)?;
    }
    let disk_only = args.iter().any(|a| a == "--disk-only");
    let reps: usize = flag("--reps").unwrap_or("5".into()).parse()?;
    if reps < 5 {
        return Err("--reps must be >=5".into());
    }
    let sizes: Vec<usize> = flag("--files")
        .unwrap_or("1000,10000,100000".into())
        .split(',')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    if sizes.iter().any(|n| *n < 100) {
        return Err("file counts must be >=100".into());
    }
    let cache = flag("--drop-caches-cmd");
    if let Some(hook) = &cache {
        platform::cold(hook)?;
    }
    let mut machine = specs(
        &scratch,
        &flag("--label").unwrap_or("unspecified".into()),
        cache.as_deref(),
    );
    if let Some(p) = &ingest_dir {
        machine.push_str(&format!(
            "\nINGEST LOCATION (all stores): {}\n{}",
            p.display(),
            specs(p, "separate ingest volume; materialize/disk on --dir", None)
        ));
    }
    let mut out = Output {
        json: flag("--json").map(PathBuf::from),
        rows: Vec::new(),
        md: format!(
            "# Storage benchmark\n\n```text\n{machine}\n```\n\n| files | corpus bytes | store | operation | unit | samples | median/p50 | p95 | p99 | note |\n| ---: | ---: | --- | --- | --- | ---: | ---: | ---: | ---: | --- |\n"
        ),
        machine,
    };
    print!("{}", out.md);
    if args.iter().any(|a| a == "--extras-only") {
        let n = *sizes.last().ok_or("--files is empty")?;
        if n < 1000 {
            return Err("extras require >=1000 files".into());
        }
        extras::checkpoint(&mut out, &scratch, n, 4096)?;
        extras::killed_writers(&mut out, &scratch)?;
        if let Some(p) = ingest_dir {
            fs::remove_dir(p)?;
        }
        fs::remove_dir(&scratch)?;
        out.save()?;
        return Ok(());
    }
    crash_check(&scratch.join("crash-test"))?;
    out.row(100,0,"B/durable","crash-sigkill","passed",&[1.0],"100 complete finals hash-verified; partial tmp detected and swept; deterministic pre-rename boundary, not power-loss proof")?;
    // Cheap page-size pilot at matched durable policy, before choosing for all cases.
    let pilot = fixture(1000, 1000);
    let mut best = (f64::INFINITY, 4096);
    for page in [4096, 16384] {
        let mut samples = Vec::new();
        for _ in 0..5 {
            let s = Store {
                root: scratch.join("pilot"),
                kind: Kind::Tuned,
                durable: true,
                page,
                incremental: false,
            };
            let t = Instant::now();
            s.init()?;
            s.ingest(&pilot, None)?;
            samples.push(elapsed(t));
            remove(&s.root)?;
        }
        let median = percentile(&samples, 0.5);
        if median < best.0 {
            best = (median, page);
        }
        out.row(
            1000,
            pilot.iter().map(|b| b.bytes.len() as u64).sum(),
            "A-tuned-pilot",
            &format!("ingest/page-{page}"),
            "ms",
            &samples,
            "durable FULL; 5 fresh stores; choose fastest median",
        )?;
    }
    let page = flag("--page-size")
        .map(|p| p.parse::<usize>())
        .transpose()?
        .unwrap_or(best.1);
    if ![4096, 16384].contains(&page) {
        return Err("--page-size must be 4096 or 16384".into());
    }
    let read_store = Store {
        root: scratch.join("read-pilot"),
        kind: Kind::Tuned,
        durable: true,
        page,
        incremental: false,
    };
    read_store.init()?;
    read_store.ingest(&pilot, None)?;
    let mut read_best = (f64::INFINITY, false);
    for incremental in [false, true] {
        let mut samples = Vec::new();
        let c = read_store.conn()?;
        for _ in 0..5 {
            let t = Instant::now();
            for b in &pilot {
                assert_eq!(store::sqlite_read(&c, b.id, incremental)?, b.bytes);
            }
            samples.push(elapsed(t));
        }
        let median = percentile(&samples, 0.5);
        if median < read_best.0 {
            read_best = (median, incremental);
        }
        out.row(
            1000,
            pilot.iter().map(|b| b.bytes.len() as u64).sum(),
            "A-tuned-pilot",
            if incremental {
                "read/incremental"
            } else {
                "read/select"
            },
            "ms",
            &samples,
            "warm 1000 reads; cached statement; includes lookup and Vec allocation",
        )?;
    }
    let incremental = read_best.1;
    remove(&read_store.root)?;
    out.row(
        0,
        0,
        "configuration",
        "chosen-page-size",
        "bytes",
        &[page as f64],
        &format!("64MiB cache; 2GiB mmap; batch 1000; incremental-read={incremental}"),
    )?;
    let selected = flag("--stores");
    if let Some(names) = &selected {
        for name in names.split(',') {
            if ![
                "A-real",
                "A-tuned/durable",
                "A-tuned/none",
                "B/durable",
                "B/none",
            ]
            .contains(&name)
            {
                return Err(format!("unknown store {name}").into());
            }
        }
    }
    let mut real10k = flag("--real10k-ms").map(|s| s.parse::<f64>()).transpose()?;
    if real10k.is_some_and(|n| !n.is_finite() || n <= 0.0) {
        return Err("--real10k-ms must be finite and positive".into());
    }
    let extras = sizes.contains(&100000) && !disk_only;
    for n in sizes {
        let blobs = fixture(n, n as u64);
        let bytes = blobs.iter().map(|b| b.bytes.len() as u64).sum();
        let mut fingerprint = Sha256::new();
        for b in &blobs {
            fingerprint.update(b.path.to_string_lossy().as_bytes());
            fingerprint.update([0]);
            fingerprint.update((b.bytes.len() as u64).to_le_bytes());
            fingerprint.update(&b.bytes);
        }
        out.row(
            n,
            bytes,
            "corpus",
            "sha256",
            "digest",
            &[],
            &fingerprint
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
        )?;
        out.row(n,bytes,"corpus","rounded-4k-payload","bytes",&[blobs.iter().map(|b|b.bytes.len().div_ceil(4096)*4096).sum::<usize>() as f64],"deterministic 4KiB payload-block bound; excludes inode/directory metadata; filesystem inline data may differ")?;
        for (name, kind, durable) in [
            ("A-real", Kind::Real, true),
            ("A-tuned/durable", Kind::Tuned, true),
            ("A-tuned/none", Kind::Tuned, false),
            ("B/durable", Kind::Files, true),
            ("B/none", Kind::Files, false),
        ] {
            if selected
                .as_ref()
                .is_some_and(|names| !names.split(',').any(|s| s == name))
            {
                continue;
            }
            let s = Store {
                root: scratch.join("store"),
                kind,
                durable,
                page,
                incremental,
            };
            let ingest = Store {
                root: ingest_dir
                    .as_ref()
                    .map_or_else(|| s.root.clone(), |p| p.join("store")),
                ..s.clone()
            };
            let mut fast_seed = kind == Kind::Real
                && n >= 100000
                && (disk_only || real10k.is_some_and(|ms: f64| ms * n as f64 / 10000.0 > 900000.0));
            if fast_seed {
                if !disk_only {
                    out.row(n,bytes,name,"ingest/extrapolated-from-10k","ms",&[real10k.unwrap()*n as f64/10000.0],"NOT measured; projected >15min. Read/materialize context uses untimed batched SQL seeding into real schema, then real Repository API reads")?;
                }
                ingest.init()?;
                ingest.seed_real_read_context(&blobs)?;
                out.row(n,bytes,name,"setup/real-read-context","setup",&[],"100k objects batch-seeded outside timings; read_blob is actual API; production per-blob ingest is not claimed")?;
            }
            let count = if fast_seed {
                0
            } else if disk_only || (kind == Kind::Real && n >= 100000) {
                1
            } else {
                reps
            };
            let mut times = Vec::new();
            let mut complete = true;
            for rep in 0..count {
                remove(&ingest.root)?;
                platform::settled(&scratch)?;
                let t = Instant::now();
                ingest.init()?;
                if !ingest.ingest(
                    &blobs,
                    if kind == Kind::Real && n >= 100000 {
                        Some(900.0)
                    } else {
                        None
                    },
                )? {
                    complete = false;
                    out.row(
                        n,
                        bytes,
                        name,
                        "ingest/aborted",
                        "ms",
                        &[elapsed(t)],
                        "exceeded 15min; partial store removed; no throughput claimed",
                    )?;
                    break;
                }
                times.push(elapsed(t));
                eprintln!(
                    "{name} {n} ingest {}/{}: {:.1} ms",
                    rep + 1,
                    count,
                    times.last().unwrap()
                );
            }
            if !complete {
                remove(&ingest.root)?;
                if kind != Kind::Real || n < 100000 {
                    continue;
                }
                let projection: Vec<_> = real10k
                    .map(|ms| ms * n as f64 / 10000.0)
                    .into_iter()
                    .collect();
                out.row(n,bytes,name,"ingest/extrapolated-from-10k","ms",&projection,"NOT measured; actual attempt exceeded 15min. Empty samples mean no 10k baseline was supplied; API read context is batch-seeded outside timings")?;
                ingest.init()?;
                ingest.seed_real_read_context(&blobs)?;
                fast_seed = true;
                out.row(n,bytes,name,"setup/real-read-context","setup",&[],"bounded real ingest aborted; complete real-schema corpus batch-seeded outside timings; production API reads remain measured")?;
            }
            if kind == Kind::Real && n == 10000 {
                real10k = Some(percentile(&times, 0.5));
            }
            if !disk_only && !fast_seed {
                out.row(n,bytes,name,"ingest","ms",&times,"RAM corpus; includes hashing, fresh store setup and final WAL checkpoint; A-real 100k single run")?;
                out.row(
                    n,
                    bytes,
                    name,
                    "ingest-throughput",
                    "MB/s",
                    &times
                        .iter()
                        .map(|ms| bytes as f64 / ms / 1000.0)
                        .collect::<Vec<_>>(),
                    "decimal MB",
                )?;
                out.row(
                    n,
                    bytes,
                    name,
                    "ingest-files",
                    "files/s",
                    &times
                        .iter()
                        .map(|ms| n as f64 * 1000.0 / ms)
                        .collect::<Vec<_>>(),
                    "",
                )?;
            }
            if ingest_dir.is_some() {
                // Source handles are closed and WAL is checkpointed. Seed the
                // isolated-volume store outside timings using its actual write path.
                platform::settled(&scratch)?;
                s.seed_from(&ingest.root, &blobs)?;
                remove(&ingest.root)?;
            }
            // Retain the final ingest or its byte-identical isolated-volume seed.
            let clones = disk(&mut out, &s, name, &blobs, bytes, reps, &scratch)?;
            if disk_only {
                remove(&s.root)?;
                continue;
            }
            operations(
                &mut out,
                &s,
                name,
                &blobs,
                bytes,
                reps,
                &scratch,
                cache.as_deref(),
            )?;
            if kind != Kind::Real {
                eviction(&mut out, &s, name, &blobs, bytes, reps, &scratch, clones)?;
            }
            remove(&s.root)?;
        }
    }
    if extras {
        extras::checkpoint(&mut out, &scratch, 100000, page)?;
        extras::killed_writers(&mut out, &scratch)?;
    }
    if let Some(p) = ingest_dir {
        fs::remove_dir(p)?;
    }
    fs::remove_dir(&scratch)?;
    out.save()?;
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("storage-bench: {e}");
        std::process::exit(1);
    }
}
