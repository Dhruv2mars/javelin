//! Real multi-process contention on one repository: compare_and_advance and put_blob.
//!
//! Retry rule (from the API docs): after an uncertain `storage_error`/`io_error` the
//! caller repeats the same operation ID and input. Every attempt is a raw sample;
//! the SQLite busy timeout (5 s, src/repository.rs) is documented, not tuned.
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Command, Stdio},
};

use javelin::{
    AdvanceOutcome, FileEntry, Manifest, ObjectId, OperationId, RepoPath, Repository, VersionNumber,
};
use serde_json::json;

use crate::{
    ctx::{Ctx, R},
    fixture,
    record::{Record, Sample, read_records, timed, unix_ns, write_records},
};

pub const MAX_RETRIES: u32 = 3;
const BLOB_BYTES: usize = 4096;
const BARRIER_LEAD_NS: u64 = 300_000_000;

pub fn blob_key(worker: u32, j: usize) -> String {
    if j.is_multiple_of(2) {
        format!("shared:{j}")
    } else {
        format!("unique:{worker}:{j}")
    }
}

/// Regenerates the exact payload for a contention blob key.
pub fn blob_bytes(seed: u64, key: &str) -> Vec<u8> {
    let salt = key
        .bytes()
        .fold(0u64, |a, b| a.wrapping_mul(131).wrapping_add(b as u64));
    fixture::bytes(seed ^ salt, BLOB_BYTES)
}

fn retryable(code: &str) -> bool {
    code == "storage_error" || code == "io_error"
}

pub fn worker(args: &[String]) -> R<()> {
    let get = |name: &str| -> R<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
            .ok_or_else(|| format!("worker missing {name}").into())
    };
    let dir = get("--repo")?;
    let out = get("--out")?;
    let w: u32 = get("--worker")?.parse()?;
    let seed: u64 = get("--seed")?.parse()?;
    let ops: usize = get("--ops")?.parse()?;
    let blobs: usize = get("--blobs")?.parse()?;
    let deadline: u64 = get("--deadline-unix-ns")?.parse()?;
    let mut ctx = Ctx {
        name: "contention".into(),
        params: crate::params::Params::profile("smoke", seed)?,
        out: out.clone().into(),
        dir: dir.clone().into(),
        deadline_unix_ns: deadline,
        records: vec![],
    };
    let (ns, opened) = timed(|| Repository::open(&dir));
    let mut s = ctx.sample("open", 0, ns, &opened);
    s.process = Some(w);
    ctx.push(s);
    let mut repo = opened?;
    // Prepare one distinct manifest per operation so every winner is traceable.
    let mut manifests = Vec::new();
    for i in 0..ops {
        let payload = format!("contention w{w} i{i} seed{seed}");
        let (ns, blob) = timed(|| repo.put_blob(payload.as_bytes()));
        let mut s = ctx.sample("prep_put_blob", i as u64, ns, &blob);
        s.process = Some(w);
        ctx.push(s);
        let blob = blob?;
        let manifest = Manifest::new([(
            RepoPath::parse(&format!("contention/w{w}-i{i}"))?,
            FileEntry::Regular {
                content: blob,
                executable: false,
            },
        )])?;
        let (ns, stored) = timed(|| repo.put_manifest(&manifest));
        let mut s = ctx.sample("prep_put_manifest", i as u64, ns, &stored);
        s.process = Some(w);
        ctx.push(s);
        manifests.push(stored?);
    }
    println!("ready");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let target: u64 = line
        .trim()
        .strip_prefix("go ")
        .ok_or("expected go <unix_ns>")?
        .parse()?;
    while unix_ns() < target.saturating_sub(2_000_000) {
        std::thread::sleep(std::time::Duration::from_micros(200));
    }
    while unix_ns() < target {
        std::hint::spin_loop();
    }
    let s = Sample {
        scenario: "contention".into(),
        op: "barrier_release".into(),
        pid: std::process::id(),
        process: Some(w),
        outcome: "ok".into(),
        start_unix_ns: Some(unix_ns()),
        param: Some(target),
        ..Default::default()
    };
    ctx.push(s);
    for (i, manifest) in manifests.iter().enumerate() {
        if ctx.expired() {
            ctx.note(&format!(
                "worker {w} stopped before op {i}: budget exhausted"
            ));
            break;
        }
        let (ns, hist) = timed(|| repo.history());
        let mut s = ctx.sample("read_head", i as u64, ns, &hist);
        s.process = Some(w);
        s.result = hist
            .as_ref()
            .ok()
            .and_then(|h| h.last())
            .map(|v| v.number.get());
        ctx.push(s);
        let Some(head) = hist.ok().and_then(|h| h.last().map(|v| v.number)) else {
            continue;
        };
        let operation = format!("w{w}-i{i}");
        let op_id = OperationId::parse(&operation)?;
        for attempt in 0..=MAX_RETRIES {
            let (ns, result) = timed(|| repo.compare_and_advance(&op_id, head, *manifest));
            let mut s = ctx.sample("compare_and_advance", i as u64, ns, &result);
            s.process = Some(w);
            s.attempt = attempt;
            s.operation = Some(operation.clone());
            s.expected = Some(head.get());
            s.manifest = Some(manifest.to_string());
            match &result {
                Ok(AdvanceOutcome::Accepted { version }) => {
                    s.outcome = "accepted".into();
                    s.result = Some(version.number.get());
                }
                Ok(AdvanceOutcome::Stale { actual }) => {
                    s.outcome = "stale".into();
                    s.result = Some(actual.get());
                }
                Err(_) => {}
            }
            let again = matches!(&result, Err(e) if retryable(e.code())) && attempt < MAX_RETRIES;
            ctx.push(s);
            if !again {
                break;
            }
        }
    }
    for j in 0..blobs {
        if ctx.expired() {
            ctx.note(&format!(
                "worker {w} stopped before blob {j}: budget exhausted"
            ));
            break;
        }
        let key = blob_key(w, j);
        let bytes = blob_bytes(seed, &key);
        for attempt in 0..=MAX_RETRIES {
            let (ns, result) = timed(|| repo.put_blob(&bytes));
            let mut s = ctx.sample("put_blob", j as u64, ns, &result);
            s.process = Some(w);
            s.attempt = attempt;
            s.content = Some(key.clone());
            s.bytes = Some(bytes.len() as u64);
            s.object = result.as_ref().ok().map(|id| id.to_string());
            let again = matches!(&result, Err(e) if retryable(e.code())) && attempt < MAX_RETRIES;
            ctx.push(s);
            if !again {
                break;
            }
        }
    }
    ctx.memory(Some(w));
    write_records(
        &Path::new(&out).join(format!("worker-{w}.jsonl")),
        &ctx.records,
    )?;
    Ok(())
}

pub fn run(ctx: &mut Ctx) -> R<()> {
    let p = ctx.params.clone();
    let dir = ctx.repo_dir()?;
    drop(Repository::init(&dir)?);
    let exe = std::env::current_exe()?;
    let workers_dir = ctx.out.join("contention-workers");
    std::fs::create_dir_all(&workers_dir)?;
    let mut children = Vec::new();
    for w in 0..p.procs as u32 {
        let child = Command::new(&exe)
            .args(["worker", "--repo"])
            .arg(&dir)
            .arg("--out")
            .arg(&workers_dir)
            .args(["--worker", &w.to_string(), "--seed", &p.seed.to_string()])
            .args(["--ops", &p.ops.to_string(), "--blobs", &p.blobs.to_string()])
            .args(["--deadline-unix-ns", &ctx.deadline_unix_ns.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        children.push(child);
    }
    let mut readers: Vec<_> = children
        .iter_mut()
        .map(|c| BufReader::new(c.stdout.take().expect("piped stdout")))
        .collect();
    for (w, reader) in readers.iter_mut().enumerate() {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line.trim() != "ready" {
            return Err(format!("worker {w} failed before the barrier").into());
        }
    }
    let target = unix_ns() + BARRIER_LEAD_NS;
    for child in &mut children {
        let stdin = child.stdin.as_mut().expect("piped stdin");
        writeln!(stdin, "go {target}")?;
        stdin.flush()?;
    }
    for (w, child) in children.iter_mut().enumerate() {
        let status = child.wait()?;
        if !status.success() {
            return Err(format!("worker {w} exited with {status}").into());
        }
    }
    let mut samples: Vec<Sample> = Vec::new();
    for w in 0..p.procs {
        for record in read_records(&workers_dir.join(format!("worker-{w}.jsonl")))? {
            match record {
                Record::Sample(s) => samples.push(s),
                other => ctx.records.push(other),
            }
        }
    }
    // Resolve every operation whose final attempt was an error by repeating the same
    // operation ID and input; the durable outcome (accepted or stale) is then known.
    let mut last: BTreeMap<String, Sample> = BTreeMap::new();
    for s in samples.iter().filter(|s| s.op == "compare_and_advance") {
        let key = s.operation.clone().unwrap();
        if last.get(&key).is_none_or(|l| l.attempt <= s.attempt) {
            last.insert(key, s.clone());
        }
    }
    let mut resolutions = Vec::new();
    for (operation, s) in last.iter().filter(|(_, s)| s.outcome == "error") {
        let mut repo = Repository::open(&dir)?;
        let manifest = ObjectId::parse(s.manifest.as_deref().unwrap())?;
        let op_id = OperationId::parse(operation)?;
        let expected = VersionNumber::new(s.expected.unwrap())?;
        let (ns, result) = timed(|| repo.compare_and_advance(&op_id, expected, manifest));
        let mut r = ctx.sample("resolve_unresolved", 0, ns, &result);
        r.attempt = s.attempt + 1;
        r.process = s.process;
        r.operation = Some(operation.clone());
        r.expected = Some(expected.get());
        r.manifest = Some(manifest.to_string());
        match &result {
            Ok(AdvanceOutcome::Accepted { version }) => {
                r.outcome = "accepted".into();
                r.result = Some(version.number.get());
            }
            Ok(AdvanceOutcome::Stale { actual }) => {
                r.outcome = "stale".into();
                r.result = Some(actual.get());
            }
            Err(_) => {}
        }
        resolutions.push(r);
    }
    samples.extend(resolutions);
    for s in &samples {
        ctx.records.push(Record::Sample(s.clone()));
    }
    // Post-run checks through fresh handles.
    let mut repo = Repository::open(&dir)?;
    let (ns, history) = timed(|| repo.history());
    ctx.push(ctx.sample("final_history", 0, ns, &history));
    let history = history?;
    std::fs::write(
        ctx.out.join("history.json"),
        serde_json::to_vec_pretty(
            &history
                .iter()
                .map(|v| {
                    json!({
                        "number": v.number.get(), "manifest": v.manifest.to_string(),
                        "operation": v.operation.as_ref().map(|o| o.as_str()),
                    })
                })
                .collect::<Vec<_>>(),
        )?,
    )?;
    let (ns, check) = timed(|| repo.check());
    ctx.push(ctx.sample("final_check", 0, ns, &check));
    let check = check?;
    std::fs::write(
        ctx.out.join("integrity.json"),
        serde_json::to_vec_pretty(&json!({
            "versions": check.versions, "objects": check.objects,
            "attempts": check.attempts, "layers": check.layers,
        }))?,
    )?;
    // Exact retry of every operation's final outcome must return the identical result.
    let mut finals: BTreeMap<String, Sample> = BTreeMap::new();
    for s in samples
        .iter()
        .filter(|s| s.op == "compare_and_advance" || s.op == "resolve_unresolved")
    {
        let key = s.operation.clone().unwrap();
        if finals.get(&key).is_none_or(|l| l.attempt <= s.attempt) {
            finals.insert(key, s.clone());
        }
    }
    let mut first_manifests = Vec::new();
    for (operation, fin) in &finals {
        let manifest = ObjectId::parse(fin.manifest.as_deref().unwrap())?;
        first_manifests.push((operation.clone(), manifest));
        let expected = VersionNumber::new(fin.expected.unwrap())?;
        let op_id = OperationId::parse(operation)?;
        let (ns, result) = timed(|| repo.compare_and_advance(&op_id, expected, manifest));
        let mut s = ctx.sample("exact_retry_check", 0, ns, &result);
        s.operation = Some(operation.clone());
        s.expected = Some(expected.get());
        let (kind, number) = match &result {
            Ok(AdvanceOutcome::Accepted { version }) => ("accepted", Some(version.number.get())),
            Ok(AdvanceOutcome::Stale { actual }) => ("stale", Some(actual.get())),
            Err(_) => ("error", None),
        };
        s.result = number;
        s.outcome = if kind == fin.outcome && number == fin.result {
            "ok"
        } else {
            "mismatch"
        }
        .into();
        ctx.push(s);
    }
    if let [(operation, _), .., (_, other)] = first_manifests.as_slice() {
        let fin = &finals[operation];
        let op_id = OperationId::parse(operation)?;
        let expected = VersionNumber::new(fin.expected.unwrap())?;
        let (ns, result) = timed(|| repo.compare_and_advance(&op_id, expected, *other));
        let mut s = ctx.sample("operation_id_reuse_check", 0, ns, &result);
        s.operation = Some(operation.clone());
        s.outcome = match &result {
            Err(e) if e.code() == "operation_id_reused" => "ok",
            _ => "mismatch",
        }
        .into();
        s.error_code = result.as_ref().err().map(|e| e.code().to_string());
        ctx.push(s);
    }
    // Every acknowledged blob must read back byte-for-byte equal to its generated payload.
    for s in samples
        .iter()
        .filter(|s| s.op == "put_blob" && s.outcome == "ok")
    {
        let id = ObjectId::parse(s.object.as_deref().unwrap())?;
        let key = s.content.clone().unwrap();
        let expected = blob_bytes(p.seed, &key);
        let (ns, read) = timed(|| repo.read_blob(id));
        let mut r = ctx.sample("blob_readback", s.index, ns, &read);
        r.content = Some(key);
        r.object = Some(id.to_string());
        r.process = s.process;
        if let Ok(bytes) = &read {
            r.outcome = if *bytes == expected { "ok" } else { "mismatch" }.into();
        }
        ctx.push(r);
    }
    ctx.disk("after_contention", None, Some(check.objects as u64));
    Ok(())
}
