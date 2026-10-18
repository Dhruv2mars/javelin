use crate::{
    Result,
    corpus::Blob,
    platform, remove,
    report::Output,
    store::{Kind, Reader, Store},
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
    r: &Reader,
    blobs: &[Blob],
    dst: &Path,
    changes: &[Blob],
    count: usize,
    clones: bool,
) -> Result<()> {
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
    #[cfg(target_os = "macos")]
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
                let reader = s.reader()?;
                let t = Instant::now();
                if let Err(e) = s.materialize_from(blobs, &dst, clones, &reader) {
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
                    if copy_disk <= 0 || delta < 0 || delta > copy_disk / 2 {
                        return Err(format!(
                            "clone sharing proof failed: clone {delta}, copy {copy_disk}"
                        )
                        .into());
                    }
                    clone_ok = true;
                }
                #[cfg(target_os = "macos")]
                if !clones && s.kind == Kind::Files {
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
                "includes dirs and writable chmod; excludes store-handle opening, verification, sync and cleanup",
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
    // Reuse a base, restoring changed paths outside timing before the next rep.
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
            s.materialize(blobs, &dst, s.kind == Kind::Files && clone_ok)?;
            let reader = s.reader()?;
            for _ in 0..reps {
                let t = Instant::now();
                incremental(s, &reader, blobs, &dst, &changes, count, clones)?;
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
                for (i, b) in blobs.iter().take(count).enumerate() {
                    if i % 3 == 2 {
                        fs::remove_file(dst.join(format!("added-{i}")))?;
                    } else {
                        fs::write(dst.join(&b.path), reader.read(b.id)?)?;
                    }
                }
            }
            verify(blobs, &dst)?;
            remove(&dst)?;
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
    reps: usize,
    scratch: &Path,
) -> Result<bool> {
    // Preserve a closed byte-identical seed outside the measured directory. Its
    // allocation is constant and excluded from each before/after delta.
    let seed = std::env::temp_dir().join(format!("javelin-storage-seed-{}", std::process::id()));
    if s.kind != Kind::Files {
        platform::tree(&s.root, &seed, false)?;
    }
    remove(&s.root)?;
    let n = blobs.len();
    let mut physical: [Vec<f64>; 4] = Default::default();
    let mut apparent = Vec::new();
    let mut base_apparent = Vec::new();
    let mut context = Vec::new();
    let mut clones = false;
    let mut layer_clones = true;
    for rep in 0..reps {
        let empty = platform::settled(scratch)?;
        s.seed_from(&seed, blobs)?;
        if s.kind == Kind::Files {
            let probe = scratch.join("probe");
            clones = match platform::clone_file(&s.object(blobs[0].id), &probe) {
                Ok(()) => true,
                Err(e)
                    if matches!(
                        e.raw_os_error(),
                        Some(libc::ENOTSUP)
                            | Some(libc::ENOSYS)
                            | Some(libc::EXDEV)
                            | Some(libc::ENOTTY)
                            | Some(libc::EINVAL)
                    ) =>
                {
                    false
                }
                Err(e) => return Err(e.into()),
            };
            if probe.exists() {
                fs::remove_file(probe)?;
            }
        }
        let store_free = platform::settled(scratch)?;
        physical[0].push((empty - store_free) as f64 / bytes as f64);
        apparent.push(platform::apparent(&s.root)? as f64);
        let base = scratch.join("base");
        s.materialize(blobs, &base, clones)?;
        verify(blobs, &base)?;
        let base_free = platform::settled(scratch)?;
        if clones {
            #[cfg(target_os = "macos")]
            if store_free - base_free < 0 || store_free - base_free > bytes as i64 / 2 {
                return Err("disk snapshot failed APFS sharing check".into());
            }
            #[cfg(target_os = "linux")]
            for b in blobs
                .iter()
                .filter(|b| b.bytes.len() >= 4096)
                .step_by((n / 100).max(1))
                .take(100)
            {
                platform::shared(&base.join(&b.path))?;
            }
        }
        physical[1].push((empty - base_free) as f64 / bytes as f64);
        base_apparent.push(platform::apparent(&base)? as f64);
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
                physical[if i == 1 { 2 } else { 3 }]
                    .push((empty - platform::settled(scratch)?) as f64 / bytes as f64);
            }
        }
        let dest = scratch.join("context-layer");
        let t = Instant::now();
        platform::layer(&base, &dest, layer_clones)?;
        context.push(elapsed(t));
        remove(&dest)?;
        for i in 1..=5 {
            remove(&scratch.join(format!("layer-{i}")))?;
        }
        remove(&base)?;
        remove(&s.root)?;
        eprintln!("{name} {n} disk allocation rep {}/{}", rep + 1, reps);
    }
    for (i, op) in [
        "store",
        "store+base",
        "store+base+1layers",
        "store+base+5layers",
    ]
    .iter()
    .enumerate()
    {
        out.row(n,bytes,name,&format!("disk/{op}"),"corpus-x",&physical[i],if layer_clones {"fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers"} else {"fresh independent allocations; sync+settle; explicit-copy Layers (clone unsupported)"})?;
    }
    out.row(
        n,
        bytes,
        name,
        "disk/store-apparent",
        "bytes",
        &apparent,
        "sum st_size; includes db + wal; logical, not exclusive blocks",
    )?;
    if s.kind != Kind::Files {
        out.row(
            n,
            bytes,
            name,
            "disk/sqlite-db-wal",
            "bytes",
            &apparent,
            "closed checkpointed DB plus any WAL/SHM",
        )?;
    }
    for layers in [1, 5] {
        out.row(
            n,
            bytes,
            name,
            &format!("disk/apparent-base+{layers}layers"),
            "bytes",
            &base_apparent
                .iter()
                .map(|b| b * (layers + 1) as f64)
                .collect::<Vec<_>>(),
            "apparent sizes count clone contents repeatedly",
        )?;
    }
    out.row(
        n,
        bytes,
        name,
        "context/base-to-layer",
        "ms",
        &context,
        if layer_clones {
            "not comparison target; APFS directory clone / Linux per-file reflink"
        } else {
            "ext4 explicit copy; not comparison target"
        },
    )?;
    // Leave an unchanged store for the remaining operations.
    s.seed_from(&seed, blobs)?;
    if s.kind != Kind::Files {
        remove(&seed)?;
    }
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
