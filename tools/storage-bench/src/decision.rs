//! Matched decision cases are rotated within each repetition. The broader
//! sequential matrix remains context; only these paired cohorts decide timings.
use crate::{
    Result,
    corpus::Blob,
    elapsed, platform, remove,
    report::Output,
    store::{Kind, Store},
};
use std::{fs, path::Path, time::Instant};

// A live native APFS volume has unrelated writers and cannot provide an
// exclusive-space plateau. Those deltas are diagnostic only; authoritative disk
// measurements always use settled() on the dedicated image.
fn boundary(scratch: &Path, isolated: bool) -> Result<i64> {
    if isolated {
        return Ok(platform::settled(scratch)?);
    }
    unsafe {
        libc::sync();
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
    Ok(platform::available(scratch)?)
}

pub fn run(
    out: &mut Output,
    scratch: &Path,
    blobs: &[Blob],
    reps: usize,
    cache: Option<&str>,
) -> Result<()> {
    let bytes = blobs.iter().map(|b| b.bytes.len() as u64).sum();
    let n = blobs.len();
    let cases = [
        ("A-tuned/durable", Kind::Tuned, true),
        ("A-tuned/none", Kind::Tuned, false),
        ("B/durable", Kind::Files, true),
        ("B/none", Kind::Files, false),
    ];
    let mut ingest: [Vec<f64>; 4] = Default::default();
    // Each repetition creates fresh stores; retain only the final durable pair
    // for materialization, after a common sync/settle boundary.
    for rep in 0..reps {
        let mut order = Vec::new();
        for offset in 0..cases.len() {
            let i = (rep + offset) % cases.len();
            let (name, kind, durable) = cases[i];
            order.push(name);
            let s = Store {
                root: scratch.join(format!("decision-{i}")),
                kind,
                durable,
                page: 4096,
                incremental: false,
            };
            remove(&s.root)?;
            boundary(scratch, cache.is_some())?;
            let t = Instant::now();
            s.init()?;
            s.ingest(blobs, None)?;
            ingest[i].push(elapsed(t));
            eprintln!(
                "decision {n} rep {} ingest {name}: {:.2} ms",
                rep + 1,
                ingest[i].last().unwrap()
            );
            if rep + 1 != reps || !durable {
                remove(&s.root)?;
            }
        }
        out.row(
            n,
            bytes,
            "decision-order",
            &format!("ingest/rep-{}", rep + 1),
            "order",
            &[],
            &order.join(" -> "),
        )?;
    }
    for (i, (name, _, _)) in cases.iter().enumerate() {
        out.row(n, bytes, name, "decision/ingest", "ms", &ingest[i], "rotated matched cohort; fixed 4KiB/SELECT; fresh store setup, hashes and WAL checkpoint included; ordinary fsync matched durability")?;
    }
    let a = Store {
        root: scratch.join("decision-0"),
        kind: Kind::Tuned,
        durable: true,
        page: 4096,
        incremental: false,
    };
    let b = Store {
        root: scratch.join("decision-2"),
        kind: Kind::Files,
        ..a.clone()
    };
    let methods = [
        (&a, "A-tuned/durable", "read-write", false),
        (&b, "B/durable", "B-clone", true),
        (&b, "B/durable", "B-copy", false),
    ];
    // Rotate both method and cache-state order, not warm-then-cold cohorts.
    let mut timings: [Vec<f64>; 6] = Default::default();
    let mut allocations: [Vec<f64>; 6] = Default::default();
    let mut unsupported = [false; 6];
    let case_count = if cache.is_some() { 6 } else { 3 };
    let dst = scratch.join("decision-base");
    for rep in 0..reps {
        let mut order = Vec::new();
        for offset in 0..case_count {
            let index = (rep + offset) % case_count;
            let (s, name, method, clones) = methods[index % 3];
            let state = if index < 3 { "warm" } else { "cold-start" };
            order.push(format!("{name}/{method}/{state}"));
            if unsupported[index] {
                continue;
            }
            remove(&dst)?;
            let before = boundary(scratch, cache.is_some())?;
            if state == "cold-start" {
                platform::cold(cache.unwrap())?;
            } else {
                let reader = s.reader()?;
                for blob in blobs {
                    std::hint::black_box(reader.read(blob.id)?);
                }
            }
            let reader = s.reader()?;
            let t = Instant::now();
            if let Err(e) = s.materialize_from(blobs, &dst, clones, &reader) {
                if clones
                    && e.downcast_ref::<std::io::Error>().is_some_and(|e| {
                        matches!(
                            e.raw_os_error(),
                            Some(
                                libc::ENOTSUP
                                    | libc::ENOSYS
                                    | libc::EXDEV
                                    | libc::ENOTTY
                                    | libc::EINVAL
                            )
                        )
                    })
                {
                    unsupported[index] = true;
                    remove(&dst)?;
                    continue;
                }
                return Err(e);
            }
            timings[index].push(elapsed(t));
            allocations[index].push((before - boundary(scratch, cache.is_some())?) as f64);
            for blob in blobs.iter().step_by((n / 100).max(1)) {
                assert_eq!(fs::read(dst.join(&blob.path))?, blob.bytes);
            }
            #[cfg(target_os = "linux")]
            if clones {
                crate::measure::fiemap(
                    out,
                    name,
                    blobs,
                    bytes,
                    &dst,
                    &format!("decision/{state}/rep-{}", rep + 1),
                )?;
            }
            eprintln!(
                "decision {n} rep {} {method}/{state}: {:.2} ms",
                rep + 1,
                timings[index].last().unwrap()
            );
        }
        out.row(
            n,
            bytes,
            "decision-order",
            &format!("materialize/rep-{}", rep + 1),
            "order",
            &[],
            &order.join(" -> "),
        )?;
    }
    for index in 0..6 {
        let (_, name, method, _) = methods[index % 3];
        let state = if index < 3 { "warm" } else { "cold-start" };
        let note = if index >= case_count {
            "unmeasured: no native cache-reset hook"
        } else if unsupported[index] {
            "unsupported clone; no fallback"
        } else {
            "rotated matched cohort; common sync+2s boundary before each case (isolated runs require a plateau; native statfs deltas diagnostic); warm reads entire source; dirs/chmod included; sync/verification excluded"
        };
        out.row(
            n,
            bytes,
            name,
            &format!("decision/materialize/{method}/{state}"),
            "ms",
            &timings[index],
            note,
        )?;
        out.row(
            n,
            bytes,
            name,
            &format!("decision/base-physical/{method}/{state}"),
            "bytes",
            &allocations[index],
            note,
        )?;
    }
    #[cfg(target_os = "macos")]
    {
        let clone = crate::report::percentile(&allocations[1], 0.5);
        let copy = crate::report::percentile(&allocations[2], 0.5);
        if cache.is_some() && (clone < 0.0 || clone > copy / 2.0) {
            return Err(
                format!("APFS decision sharing proof failed: clone {clone}, copy {copy}").into(),
            );
        }
    }
    remove(&dst)?;
    remove(&a.root)?;
    remove(&b.root)?;
    Ok(())
}
