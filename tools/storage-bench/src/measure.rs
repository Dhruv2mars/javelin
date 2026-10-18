use crate::{
    Result,
    corpus::Blob,
    platform, remove,
    report::Output,
    store::{Kind, Store},
};
use javelin::ObjectId;
use std::{fs, path::Path, time::Instant};

fn verify(blobs: &[Blob], dst: &Path) -> Result<()> {
    let stride = if blobs.len() <= 1000 {
        1
    } else {
        (blobs.len() / 100).max(1)
    };
    for b in blobs.iter().step_by(stride) {
        let bytes = fs::read(dst.join(&b.path))?;
        if bytes != b.bytes || ObjectId::for_blob(&bytes) != b.id {
            return Err("materialized content mismatch".into());
        }
    }
    Ok(())
}
fn warm(s: &Store, blobs: &[Blob]) -> Result<()> {
    // Real full sequential read; no corpus-generator cache assumption.
    let reader = s.reader()?;
    for b in blobs {
        std::hint::black_box(reader.read(b.id)?);
    }
    Ok(())
}
fn elapsed(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn incremental(
    s: &Store,
    blobs: &[Blob],
    dst: &Path,
    changes: &[Blob],
    count: usize,
    clones: bool,
) -> Result<()> {
    let r = s.reader()?;
    for i in 0..count {
        let to = if i % 3 == 2 {
            dst.join(format!("added-{i}"))
        } else {
            dst.join(&blobs[i].path)
        };
        if i % 3 == 1 {
            fs::remove_file(to)?;
            continue;
        }
        let tmp = to.with_extension("bench-tmp");
        if clones {
            platform::clone_file(&s.object(changes[i].id), &tmp)?;
        } else {
            fs::write(&tmp, r.read(changes[i].id)?)?;
        }
        fs::rename(tmp, to)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn operations(
    out: &mut Output,
    s: &Store,
    name: &str,
    blobs: &[Blob],
    bytes: u64,
    reps: usize,
    scratch: &Path,
    cache: Option<&str>,
) -> Result<()> {
    let n = blobs.len();
    let dst = scratch.join("base");
    let mut clone_ok = false;
    let methods: &[(&str, bool)] = if s.kind == Kind::Files {
        &[("B-copy", false), ("B-clone", true)]
    } else {
        &[("read-write", false)]
    };
    let mut copy_disk = 0;
    for &(method, clones) in methods {
        let mut unsupported = None;
        for state in ["warm", "cold-start"] {
            if state == "cold-start" && cache.is_none() {
                out.row(
                    n,
                    bytes,
                    name,
                    &format!("materialize/{method}/{state}"),
                    "ms",
                    &[],
                    "unmeasured: no cache hook",
                )?;
                continue;
            }
            let mut times = Vec::new();
            let mut deltas = Vec::new();
            for rep in 0..reps {
                remove(&dst)?;
                let before = platform::settled(scratch)?;
                if state == "cold-start" {
                    platform::cold(cache.unwrap())?;
                } else {
                    warm(s, blobs)?;
                }
                let t = Instant::now();
                if let Err(e) = s.materialize(blobs, &dst, clones) {
                    // Only clone capability errors are recorded as unsupported.
                    if clones
                        && e.downcast_ref::<std::io::Error>().is_some_and(|e| {
                            matches!(
                                e.raw_os_error(),
                                Some(libc::ENOTSUP)
                                    | Some(libc::ENOSYS)
                                    | Some(libc::EXDEV)
                                    | Some(libc::ENOTTY)
                                    | Some(libc::EINVAL)
                            )
                        })
                    {
                        unsupported = Some(e.to_string());
                        remove(&dst)?;
                        break;
                    }
                    return Err(e);
                }
                times.push(elapsed(t));
                let delta = before - platform::settled(scratch)?;
                deltas.push(delta as f64);
                verify(blobs, &dst)?;
                if clones {
                    #[cfg(target_os = "linux")]
                    for b in blobs
                        .iter()
                        .filter(|b| b.bytes.len() >= 4096)
                        .step_by((n / 100).max(1))
                        .take(100)
                    {
                        platform::shared(&dst.join(&b.path))?;
                    }
                    #[cfg(target_os = "macos")]
                    if copy_disk > 0 && delta > copy_disk / 2 {
                        return Err(format!(
                            "clone sharing proof failed: clone {delta}, copy {copy_disk}"
                        )
                        .into());
                    }
                    clone_ok = true;
                } else if s.kind == Kind::Files {
                    copy_disk = delta;
                }
                eprintln!(
                    "{name} {n} {method} {state} rep {}: {:.1} ms",
                    rep + 1,
                    times.last().unwrap()
                );
            }
            if let Some(e) = &unsupported {
                out.row(
                    n,
                    bytes,
                    name,
                    &format!("materialize/{method}/{state}"),
                    "ms",
                    &[],
                    &format!("unsupported: {e}"),
                )?;
                break;
            }
            out.row(
                n,
                bytes,
                name,
                &format!("materialize/{method}/{state}"),
                "ms",
                &times,
                "includes dirs and writable chmod; excludes content verification, sync and cleanup",
            )?;
            out.row(
                n,
                bytes,
                name,
                &format!("base-physical/{method}/{state}"),
                "bytes",
                &deltas,
                "statfs allocation delta; clone metadata included",
            )?;
        }
    }
    remove(&dst)?;
    // Increments use an untimed, fresh base and prestored new blobs for each rep.
    let changes: Vec<_> = blobs
        .iter()
        .take(100)
        .enumerate()
        .map(|(i, b)| {
            let mut bytes = b.bytes.clone();
            bytes.extend_from_slice(format!("change-{i}").as_bytes());
            Blob::new(b.path.clone(), bytes)
        })
        .collect();
    s.ingest(&changes, None)?;
    let incremental_methods: &[bool] = if s.kind == Kind::Files && clone_ok {
        &[false, true]
    } else {
        &[false]
    };
    for &clones in incremental_methods {
        for count in [1, 100] {
            let mut times = Vec::new();
            for _ in 0..reps {
                s.materialize(blobs, &dst, s.kind == Kind::Files && clone_ok)?;
                let t = Instant::now();
                incremental(s, blobs, &dst, &changes, count, clones)?;
                times.push(elapsed(t));
                for i in 0..count {
                    let to = if i % 3 == 2 {
                        dst.join(format!("added-{i}"))
                    } else {
                        dst.join(&blobs[i].path)
                    };
                    if i % 3 == 1 {
                        assert!(!to.exists());
                    } else {
                        assert_eq!(fs::read(to)?, changes[i].bytes);
                    }
                }
                remove(&dst)?;
            }
            out.row(n,bytes,name,&format!("incremental/{count}/{}",if clones {"clone"} else {"read-write"}),"ms",&times,"1 = modified; 100 = 34 modified + 33 deleted + 33 added; atomic temp+rename, no base fsync")?;
        }
    }
    for state in ["warm", "cold-start"] {
        if state == "cold-start" && cache.is_none() {
            out.row(
                n,
                bytes,
                name,
                &format!("random-read/{state}"),
                "us",
                &[],
                "unmeasured",
            )?;
            continue;
        }
        let mut times = Vec::new();
        for rep in 0..reps {
            if state == "cold-start" {
                platform::cold(cache.unwrap())?;
            } else {
                warm(s, blobs)?;
            }
            let r = s.reader()?;
            let mut rng = 0xdecafbad_u64 + rep as u64;
            for _ in 0..1000 {
                rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
                let b = &blobs[(rng >> 32) as usize % n];
                let t = Instant::now();
                let data = r.read(b.id)?;
                times.push(t.elapsed().as_secs_f64() * 1e6);
                if data != b.bytes {
                    return Err("random read content mismatch".into());
                }
                std::hint::black_box(data);
            }
        }
        out.row(n,bytes,name,&format!("random-read/{state}"),"us",&times,"p50 and p99 over 1000 reads/rep; cold-start drops cache once per sequence, then warms; open store handle excluded")?;
    }
    // Remove incremental blobs before corpus-only disk accounting.
    if s.kind != Kind::Real {
        s.evict(&changes.iter().map(|b| b.id).collect::<Vec<_>>())?;
    }
    Ok(())
}

pub fn disk(
    out: &mut Output,
    s: &Store,
    name: &str,
    blobs: &[Blob],
    bytes: u64,
    empty: i64,
    scratch: &Path,
) -> Result<bool> {
    let n = blobs.len();
    let store = empty - platform::settled(scratch)?;
    out.row(
        n,
        bytes,
        name,
        "disk/store",
        "corpus-x",
        &[store as f64 / bytes as f64],
        "one isolated allocation sample after checkpoint and sync",
    )?;
    out.row(
        n,
        bytes,
        name,
        "disk/store-apparent",
        "bytes",
        &[platform::apparent(&s.root)? as f64],
        "sum st_size; includes db + wal; logical, not exclusive blocks",
    )?;
    if s.kind != Kind::Files {
        out.row(
            n,
            bytes,
            name,
            "disk/sqlite-db-wal",
            "bytes",
            &[platform::apparent(&s.root)? as f64],
            "checkpointed DB plus any WAL and SHM",
        )?;
    }
    let base = scratch.join("base");
    let clones = s.kind == Kind::Files && {
        let b = &blobs[0];
        let probe = scratch.join("probe");
        let ok = platform::clone_file(&s.object(b.id), &probe).is_ok();
        let _ = fs::remove_file(probe);
        ok
    };
    s.materialize(blobs, &base, clones)?;
    out.row(
        n,
        bytes,
        name,
        "disk/store+base",
        "corpus-x",
        &[(empty - platform::settled(scratch)?) as f64 / bytes as f64],
        "B clones where supported; ext4 explicit copy",
    )?;
    let mut layer_clones = true;
    for i in 1..=5 {
        let dst = scratch.join(format!("layer-{i}"));
        match platform::layer(&base, &dst, layer_clones) {
            Ok(()) => {}
            Err(e)
                if i == 1
                    && matches!(
                        e.raw_os_error(),
                        Some(libc::ENOTSUP) | Some(libc::ENOTTY) | Some(libc::EINVAL)
                    ) =>
            {
                remove(&dst)?;
                layer_clones = false;
                platform::layer(&base, &dst, false)?;
            }
            Err(e) => return Err(e.into()),
        }
        if i == 1 || i == 5 {
            out.row(
                n,
                bytes,
                name,
                &format!("disk/store+base+{i}layers"),
                "corpus-x",
                &[(empty - platform::settled(scratch)?) as f64 / bytes as f64],
                if layer_clones {
                    "APFS directory clone / Linux per-file reflink"
                } else {
                    "explicit byte-copy Layers (clone unsupported)"
                },
            )?;
            out.row(
                n,
                bytes,
                name,
                &format!("disk/apparent-base+{i}layers"),
                "bytes",
                &[((i + 1) as u64 * platform::apparent(&base)?) as f64],
                "apparent sizes count shared clone contents repeatedly",
            )?;
        }
    }
    for i in 1..=5 {
        remove(&scratch.join(format!("layer-{i}")))?;
    }
    remove(&base)?;
    let mut context = Vec::new();
    // Context uses the very same already-materialized base, outside timed setup.
    s.materialize(blobs, &base, clones)?;
    for _ in 0..5 {
        let dest = scratch.join("context-layer");
        let t = Instant::now();
        platform::layer(&base, &dest, layer_clones)?;
        context.push(elapsed(t));
        remove(&dest)?;
    }
    out.row(
        n,
        bytes,
        name,
        "context/base-to-layer",
        "ms",
        &context,
        if layer_clones {
            "not comparison target; APFS recursive directory clone / Linux per-file reflink"
        } else {
            "ext4 explicit copy; not comparison target"
        },
    )?;
    remove(&base)?;
    Ok(clones)
}

#[allow(clippy::too_many_arguments)]
pub fn eviction(
    out: &mut Output,
    s: &Store,
    name: &str,
    blobs: &[Blob],
    bytes: u64,
    reps: usize,
    scratch: &Path,
    clones: bool,
) -> Result<()> {
    // Recorded deterministic touch sequence; LRU is (last-touch index, original index).
    let n = blobs.len();
    let mut touched = vec![0; n];
    let mut rng = 42u64;
    for touch in 1..=n * 2 {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        touched[(rng >> 32) as usize % n] = touch;
    }
    let mut order: Vec<_> = (0..n).collect();
    order.sort_by_key(|&i| (touched[i], i));
    order.truncate(n / 10);
    let victims: Vec<_> = order
        .iter()
        .map(|&i| Blob::new(blobs[i].path.clone(), blobs[i].bytes.clone()))
        .collect();
    let ids: Vec<_> = victims.iter().map(|b| b.id).collect();
    let sequence = order
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(",");
    out.row(
        n,
        bytes,
        name,
        "eviction/lru-order",
        "indices",
        &[],
        &sequence,
    )?;
    for live in [false, true] {
        let mut times = Vec::new();
        let mut freed = Vec::new();
        let mut vacuum_times = Vec::new();
        let mut vacuum_freed = Vec::new();
        for _ in 0..reps {
            let base = scratch.join("eviction-base");
            if live {
                s.materialize(blobs, &base, clones)?;
            }
            let before = platform::settled(scratch)?;
            let t = Instant::now();
            s.evict(&ids)?;
            times.push(elapsed(t));
            let after = platform::settled(scratch)?;
            freed.push((after - before) as f64);
            if s.kind == Kind::Tuned {
                let c = s.conn()?;
                let t = Instant::now();
                loop {
                    let count: i64 = c.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
                    if count == 0 {
                        break;
                    }
                    c.execute_batch("PRAGMA incremental_vacuum(1000000)")?;
                }
                c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
                vacuum_times.push(elapsed(t));
                drop(c);
                vacuum_freed.push((platform::settled(scratch)? - after) as f64);
            }
            if live {
                verify(blobs, &base)?;
                remove(&base)?;
            }
            // Restore is setup, not measured ingest. Avoid thousands of per-file
            // flushes here; the next statfs sync flushes the complete restored state.
            let restore = Store {
                durable: false,
                ..s.clone()
            };
            restore.ingest(&victims, None)?;
        }
        let state = if live { "live-base" } else { "no-base" };
        out.row(
            n,
            bytes,
            name,
            &format!("eviction/{state}"),
            "ms",
            &times,
            "DELETE transaction + checkpoint / unlink + policy dir fsync; excludes statfs sync",
        )?;
        out.row(
            n,
            bytes,
            name,
            &format!("eviction-freed/{state}"),
            "bytes",
            &freed,
            "before vacuum; 10% deterministic LRU, shared live clone data remains allocated",
        )?;
        if !vacuum_times.is_empty() {
            out.row(
                n,
                bytes,
                name,
                &format!("incremental-vacuum/{state}"),
                "ms",
                &vacuum_times,
                "auto_vacuum=INCREMENTAL; drain freelist, checkpoint; not full compaction",
            )?;
            out.row(
                n,
                bytes,
                name,
                &format!("incremental-vacuum-freed/{state}"),
                "bytes",
                &vacuum_freed,
                "additional physical space returned",
            )?;
        }
    }
    Ok(())
}
