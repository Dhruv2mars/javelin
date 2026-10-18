//! Additional decision measurements. No production capture/storage code is changed.
use crate::{
    Result,
    corpus::{Blob, fixture},
    elapsed, platform, remove,
    report::Output,
    store::{Kind, Store},
};
use javelin::{FileEntry, Manifest, ObjectId, RepoPath, Repository};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

fn manifest(blobs: &[Blob]) -> Result<Manifest> {
    Ok(Manifest::new(
        blobs
            .iter()
            .map(|b| {
                Ok((
                    RepoPath::parse(b.path.to_str().unwrap())?,
                    FileEntry::Regular {
                        content: b.id,
                        executable: false,
                    },
                ))
            })
            .collect::<Result<Vec<_>>>()?,
    )?)
}
fn metadata_id(kind: u8, bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b"javelin-object-v1\0");
    h.update([kind]);
    h.update(bytes);
    ObjectId::from_bytes(h.finalize().into()).to_string()
}
fn insert_metadata(c: &Connection, kind: u8, bytes: &[u8]) -> Result<()> {
    c.execute(
        "INSERT INTO objects(id,kind,bytes) VALUES(?1,?2,?3) ON CONFLICT(id) DO NOTHING",
        params![metadata_id(kind, bytes), kind, bytes],
    )?;
    Ok(())
}
/// This isolates store-side cost: the full manifest is a conservative, explicitly
/// requested metadata-row proxy; the real capture checkpoint stores sparse changes.
fn commit(
    s: &Store,
    paths: &[PathBuf],
    metadata: Option<(u8, &[u8])>,
    repo: Option<&mut Repository>,
    c: &mut Connection,
) -> Result<()> {
    match s.kind {
        Kind::Real => {
            let r = repo.ok_or("missing real writer")?;
            for p in paths {
                r.put_blob(&fs::read(p)?)?;
            }
            if let Some((kind, bytes)) = metadata {
                c.execute_batch("BEGIN IMMEDIATE")?;
                insert_metadata(c, kind, bytes)?;
                c.execute_batch("COMMIT")?;
            }
        }
        Kind::Tuned => {
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            {
                let mut ins = tx.prepare(
                    "INSERT INTO objects(id,kind,bytes) VALUES(?1,1,?2) ON CONFLICT(id) DO NOTHING",
                )?;
                for p in paths {
                    let bytes = fs::read(p)?;
                    ins.execute(params![ObjectId::for_blob(&bytes).to_string(), bytes])?;
                }
            }
            if let Some((kind, bytes)) = metadata {
                insert_metadata(&tx, kind, bytes)?;
            }
            tx.commit()?;
            c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        }
        Kind::Files => {
            // Store::ingest computes the canonical hash once; its file path does
            // not use this placeholder ID. File reads are inside the timer.
            let blobs = paths
                .iter()
                .map(|p| {
                    Ok(Blob {
                        path: p.clone(),
                        bytes: fs::read(p)?,
                        id: ObjectId::from_bytes([0; 32]),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            s.ingest(&blobs, None)?;
            if let Some((kind, bytes)) = metadata {
                let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                insert_metadata(&tx, kind, bytes)?;
                tx.commit()?;
                c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
            }
        }
    }
    Ok(())
}
fn stat_walk(p: &Path) -> Result<usize> {
    let mut count = 0;
    for e in fs::read_dir(p)? {
        let e = e?;
        let m = fs::symlink_metadata(e.path())?;
        if m.is_dir() {
            count += stat_walk(&e.path())?;
        } else {
            count += 1;
            std::hint::black_box(m);
        }
    }
    Ok(count)
}

pub fn checkpoint(out: &mut Output, scratch: &Path, n: usize, page: usize) -> Result<()> {
    let mut blobs = fixture(n, n as u64);
    let bytes = blobs.iter().map(|b| b.bytes.len() as u64).sum();
    let base = scratch.join("checkpoint-base");
    fs::create_dir(&base)?;
    for b in &blobs {
        let p = base.join(&b.path);
        fs::create_dir_all(p.parent().unwrap())?;
        fs::write(p, &b.bytes)?;
    }
    let root = scratch.join("checkpoint-layer");
    let cloned = match platform::layer(&base, &root, true) {
        Ok(()) => true,
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(libc::ENOTSUP) | Some(libc::ENOTTY) | Some(libc::EINVAL)
            ) =>
        {
            remove(&root)?;
            platform::tree(&base, &root, false)?;
            false
        }
        Err(e) => return Err(e.into()),
    };
    out.row(
        n,
        bytes,
        "checkpoint-context",
        "layer-construction",
        "context",
        &[],
        if cloned {
            "actual APFS directory clone / Linux per-file reflink; base remains live during edits"
        } else {
            "explicit-copy Layer: filesystem clone unsupported; base remains live"
        },
    )?;
    platform::settled(scratch)?;
    let manifest_size = manifest(&blobs)?.canonical_bytes().len();
    out.row(n,bytes,"checkpoint-context","manifest-canonical-size","bytes",&[manifest_size as f64],"real Manifest::canonical_bytes, 100k entries; capture itself writes sparse checkpoint metadata, not a fresh full manifest")?;
    for (name, kind) in [
        ("A-real", Kind::Real),
        ("A-tuned/durable", Kind::Tuned),
        ("B/durable", Kind::Files),
    ] {
        let s = Store {
            root: scratch.join("checkpoint-store"),
            kind,
            durable: true,
            page,
            incremental: false,
        };
        s.init()?;
        if kind == Kind::Real {
            s.seed_real_read_context(&blobs)?;
        } else {
            s.ingest(&blobs, None)?;
        }
        let meta = Store {
            root: scratch.join("checkpoint-metadata"),
            kind: Kind::Tuned,
            ..s.clone()
        };
        if kind == Kind::Files {
            meta.init()?;
        }
        let initial: Vec<_> = blobs
            .iter()
            .take(600)
            .map(|b| (b.bytes.len(), b.bytes[0]))
            .collect();
        let base_bytes = manifest(&blobs)?.canonical_bytes();
        let c = if kind == Kind::Files {
            meta.conn()?
        } else {
            s.conn()?
        };
        insert_metadata(&c, 2, &base_bytes)?;
        drop(c);
        let mut repo = if kind == Kind::Real {
            Some(Repository::open(&s.root)?)
        } else {
            None
        };
        let mut conn = if kind == Kind::Files {
            meta.conn()?
        } else if kind == Kind::Real {
            let c = Connection::open(s.db())?;
            c.execute_batch("PRAGMA fullfsync=ON; PRAGMA synchronous=EXTRA")?;
            c
        } else {
            s.conn()?
        };
        for (variant_index, variant) in ["blobs-only", "with-checkpoint", "with-manifest"]
            .into_iter()
            .enumerate()
        {
            let base_id = ObjectId::for_manifest(&manifest(&blobs)?);
            let mut samples = Vec::new();
            let mut scans = Vec::new();
            for rep in 0..20 {
                // Distinct paths per trial and per metadata variant. The write
                // pattern matches examples/capture_bench: append x / XOR byte 0.
                let start = (rep + variant_index * 20) * 10;
                let mut paths = Vec::new();
                for i in 0..10 {
                    let b = &mut blobs[(start + i) % n];
                    let p = root.join(&b.path);
                    if i % 2 == 0 {
                        b.bytes.push(b'x');
                        fs::OpenOptions::new()
                            .append(true)
                            .open(&p)?
                            .write_all(b"x")?;
                    } else {
                        b.bytes[0] ^= 0xff;
                        fs::write(&p, &b.bytes)?;
                    }
                    b.id = ObjectId::for_blob(&b.bytes);
                    paths.push(p);
                }
                let t = Instant::now();
                assert_eq!(stat_walk(&root)?, n);
                scans.push(elapsed(t));
                // Serialization and filesystem edits are common work, outside
                // the store timer. Canonical metadata hashing stays inside it.
                let metadata = match variant {
                    "with-manifest" => Some((2, manifest(&blobs)?.canonical_bytes())),
                    "with-checkpoint" => {
                        let delta =
                            manifest(&blobs[variant_index * 200..start + 10])?.canonical_bytes();
                        // EncodedCheckpoint field order; empty sources omitted.
                        let encoded = format!(
                            "{{\"repository\":\"{}\",\"version\":1,\"manifest\":\"{}\",\"changes\":{}}}",
                            "0".repeat(64),
                            base_id,
                            String::from_utf8(delta)?
                        );
                        Some((3, encoded.into_bytes()))
                    }
                    _ => None,
                };
                let t = Instant::now();
                commit(
                    &s,
                    &paths,
                    metadata.as_ref().map(|(k, b)| (*k, b.as_slice())),
                    repo.as_mut(),
                    &mut conn,
                )?;
                samples.push(elapsed(t));
                let reader = s.reader()?;
                for p in &paths {
                    let expected = fs::read(p)?;
                    if reader.read(ObjectId::for_blob(&expected))? != expected {
                        return Err("checkpoint blob read-back mismatch".into());
                    }
                }
                if let Some((kind, metadata)) = &metadata {
                    let stored: Vec<u8> = conn.query_row(
                        "SELECT bytes FROM objects WHERE id=?1",
                        [metadata_id(*kind, metadata)],
                        |r| r.get(0),
                    )?;
                    if stored != *metadata {
                        return Err("checkpoint metadata mismatch".into());
                    }
                }
                eprintln!(
                    "{name} checkpoint {variant} rep {}/20: {:.2} ms",
                    rep + 1,
                    samples.last().unwrap()
                );
            }
            out.row(n,bytes,name,&format!("checkpoint-10/{variant}"),"ms",&samples,"20 reps; 5 single-byte appends + 5 same-size first-byte rewrites; file reads, canonical hashes, store commits included; no stat scan/serialization/cache-save/head CAS; real uses per-blob API, tuned single transaction, split keeps metadata in WAL FULL SQLite; with-checkpoint writes canonical-shaped sparse metadata row; with-manifest adds requested conservative full-tree metadata proxy instead; neither executes graph validation/head publication")?;
            out.row(n,bytes,name,&format!("checkpoint-stat-scan/{variant}"),"ms",&scans,"100k metadata-only lstat walk alongside each 10-edit trial, following capture_bench baseline; not the complete production scan/cache algorithm")?;
        }
        drop(repo);
        drop(conn);
        remove(&s.root)?;
        if kind == Kind::Files {
            remove(&meta.root)?;
        }
        for (b, (len, first)) in blobs.iter_mut().zip(initial) {
            b.bytes.truncate(len);
            b.bytes[0] = first;
            b.id = ObjectId::for_blob(&b.bytes);
            let p = root.join(&b.path);
            fs::remove_file(&p)?;
            if cloned {
                platform::clone_file(&base.join(&b.path), &p)?;
            } else {
                platform::copy_bytes(&base.join(&b.path), &p)?;
            }
        }
    }
    remove(&root)?;
    remove(&base)
}

pub fn child(kind: &str, root: PathBuf) -> Result<()> {
    let s = Store {
        root,
        kind: if kind == "files" {
            Kind::Files
        } else {
            Kind::Tuned
        },
        durable: true,
        page: 4096,
        incremental: false,
    };
    let mut blobs = fixture(100, 819);
    // Maximum-size fault-injection payload; timed fixture generation is unchanged.
    let mut large = vec![0; 2 * 1024 * 1024];
    let mut rng = 0x813d_d3a9_u64;
    for byte in &mut large {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        *byte = rng as u8;
    }
    blobs.insert(0, Blob::new(PathBuf::from("large-crash-blob"), large));
    fs::write(s.root.join("ready"), b"ready")?;
    for batch in 0u64.. {
        for (i, b) in blobs.iter_mut().enumerate() {
            b.bytes
                .extend_from_slice(&(batch * 100 + i as u64).to_le_bytes());
            b.id = ObjectId::for_blob(&b.bytes);
        }
        s.ingest(&blobs, None)?;
    }
    unreachable!()
}

pub fn killed_writers(out: &mut Output, scratch: &Path) -> Result<()> {
    use std::os::unix::process::ExitStatusExt;
    let mut rng = 0x59c5_11dau64;
    for (name, kind) in [("A-tuned/durable", Kind::Tuned), ("B/durable", Kind::Files)] {
        let mut passed = Vec::new();
        let mut leftovers = Vec::new();
        let mut finals = Vec::new();
        let mut delays = Vec::new();
        for trial in 0..20 {
            let s = Store {
                root: scratch.join("random-kill"),
                kind,
                durable: true,
                page: 4096,
                incremental: false,
            };
            s.init()?;
            s.ingest(&fixture(100, 100), None)?;
            let mut c = Command::new(std::env::current_exe()?)
                .args([
                    "--kill-writer",
                    if kind == Kind::Files {
                        "files"
                    } else {
                        "sqlite"
                    },
                ])
                .arg(&s.root)
                .spawn()?;
            let t = Instant::now();
            while !s.root.join("ready").exists() {
                if let Some(status) = c.try_wait()? {
                    return Err(format!("writer exited early: {status}").into());
                }
                if t.elapsed() > Duration::from_secs(30) {
                    c.kill()?;
                    c.wait()?;
                    return Err("writer startup timeout".into());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            let delay = 1 + (rng >> 32) % 200;
            delays.push(delay);
            std::thread::sleep(Duration::from_millis(delay));
            c.kill()?;
            let status = c.wait()?;
            if status.signal() != Some(libc::SIGKILL) {
                return Err(format!("expected SIGKILL: {status}").into());
            }
            let mut count = 0;
            let mut tmp = 0;
            if kind == Kind::Files {
                for d in fs::read_dir(s.root.join("objects"))? {
                    let d = d?;
                    for f in fs::read_dir(d.path())? {
                        let f = f?;
                        let id = format!(
                            "{}{}",
                            d.file_name().to_string_lossy(),
                            f.file_name().to_string_lossy()
                        );
                        if ObjectId::for_blob(&fs::read(f.path())?).to_string() != id {
                            return Err("visible partial blob after SIGKILL".into());
                        }
                        count += 1;
                    }
                }
                for f in fs::read_dir(s.root.join("tmp"))? {
                    fs::remove_file(f?.path())?;
                    tmp += 1;
                }
                assert_eq!(fs::read_dir(s.root.join("tmp"))?.count(), 0);
            } else {
                let conn = s.conn()?;
                let check: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
                if check != "ok" {
                    return Err(format!("SQLite integrity_check: {check}").into());
                }
                let mut q = conn.prepare("SELECT id,bytes FROM objects WHERE kind=1")?;
                let rows = q.query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
                })?;
                for row in rows {
                    let (id, bytes) = row?;
                    if ObjectId::for_blob(&bytes).to_string() != id {
                        return Err("SQLite row hash mismatch after SIGKILL".into());
                    }
                    count += 1;
                }
            }
            passed.push(1.0);
            leftovers.push(tmp as f64);
            finals.push(count as f64);
            remove(&s.root)?;
            eprintln!(
                "{name} randomized SIGKILL {}/20: {count} finals verified, {tmp} tmp swept",
                trial + 1
            );
        }
        let integrity = if kind == Kind::Tuned {
            "SQLite integrity_check=ok each trial"
        } else {
            "file-object namespace checked each trial; no SQLite writer in this case"
        };
        let note = format!(
            "20/20 SIGKILL trials; all final objects fully hash-verified; {integrity}; seeded 100 baseline blobs; stream includes one 2MiB random payload plus 100 fixture blobs per batch; ready handshake then deterministic pseudorandom delays ms={delays:?}; process-crash only, not power-loss"
        );
        out.row(
            100000,
            0,
            name,
            "crash-random-kill",
            "passed",
            &passed,
            &note,
        )?;
        out.row(
            100000,
            0,
            name,
            "crash-final-objects-verified",
            "count",
            &finals,
            "every final row/file checked after every kill, including newly committed objects",
        )?;
        out.row(
            100000,
            0,
            name,
            "crash-tmp-swept",
            "count",
            &leftovers,
            "leftover tmp files counted then removed; zero remain after each sweep",
        )?;
    }
    Ok(())
}
