//! Real multi-process contention on one repository: compare_and_advance and put_blob.
//!
//! Retry rule (from the API docs): after an uncertain `storage_error`/`io_error` the
//! caller reopens and repeats the same operation ID and input. Every attempt is a raw
//! sample; reopen cost is a separate sample. The SQLite busy timeout (5 s,
//! src/repository.rs) is documented, not tuned. A stale outcome is final: the next
//! logical operation uses a new operation ID.
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
    host::sha256_hex,
    record::{Record, Sample, read_records, timed, unix_ns, write_records},
};

pub const MAX_RETRIES: u32 = 3;
use crate::params::CONTENTION_BLOB_BYTES as BLOB_BYTES;
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

/// One timed compare_and_advance attempt, fully described for the raw record.
#[allow(clippy::too_many_arguments)]
fn advance(
    ctx: &Ctx,
    repo: &mut Repository,
    op: &str,
    operation: &str,
    expected: u64,
    manifest: ObjectId,
    index: u64,
    attempt: u32,
    process: Option<u32>,
) -> (Sample, javelin::Result<AdvanceOutcome>) {
    let started = unix_ns();
    let (ns, result) = timed(|| {
        repo.compare_and_advance(
            &OperationId::parse(operation)?,
            VersionNumber::new(expected)?,
            manifest,
        )
    });
    let mut s = ctx.sample(op, index, ns, &result);
    s.process = process;
    s.attempt = attempt;
    s.operation = Some(operation.into());
    s.expected = Some(expected);
    s.manifest = Some(manifest.to_string());
    s.start_unix_ns = Some(started);
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
    (s, result)
}

fn put_blob(
    ctx: &mut Ctx,
    repo: &mut Repository,
    op: &str,
    index: u64,
    bytes: &[u8],
) -> R<ObjectId> {
    let (ns, result) = timed(|| repo.put_blob(bytes));
    let mut s = ctx.sample(op, index, ns, &result);
    s.bytes = Some(bytes.len() as u64);
    s.object = result.as_ref().ok().map(|id| id.to_string());
    ctx.push(s);
    Ok(result?)
}

fn put_manifest(
    ctx: &mut Ctx,
    repo: &mut Repository,
    op: &str,
    index: u64,
    manifest: &Manifest,
) -> R<ObjectId> {
    let (ns, result) = timed(|| repo.put_manifest(manifest));
    let mut s = ctx.sample(op, index, ns, &result);
    s.object = result.as_ref().ok().map(|id| id.to_string());
    ctx.push(s);
    Ok(result?)
}

/// Runs a write under the documented retry rule: after an uncertain `storage_error` or
/// `io_error`, reopen (a separate sample) and repeat the same input. Every attempt is a
/// raw sample; a non-uncertain error or exhausted retries is returned to the caller.
#[allow(clippy::too_many_arguments)]
fn retried<T>(
    ctx: &mut Ctx,
    repo: &mut Repository,
    dir: &str,
    w: u32,
    op: &str,
    index: u64,
    decorate: impl Fn(&mut Sample, Option<&T>),
    call: impl Fn(&mut Repository) -> javelin::Result<T>,
) -> R<T> {
    for attempt in 0..=MAX_RETRIES {
        let (ns, result) = timed(|| call(repo));
        let mut s = ctx.sample(op, index, ns, &result);
        s.process = Some(w);
        s.attempt = attempt;
        decorate(&mut s, result.as_ref().ok());
        let again = matches!(&result, Err(e) if retryable(e.code())) && attempt < MAX_RETRIES;
        ctx.push(s);
        match result {
            Ok(value) => return Ok(value),
            Err(_) if again => reopen(ctx, repo, dir, w)?,
            Err(e) => return Err(e.into()),
        }
    }
    unreachable!("the final attempt always returns")
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
    let result = worker_body(&mut ctx, &dir, w, seed, ops, blobs);
    ctx.memory(Some(w));
    // Raw samples, including the error that ended a failed worker, are always kept.
    write_records(
        &Path::new(&out).join(format!("worker-{w}.jsonl")),
        &ctx.records,
    )?;
    result
}

fn worker_body(ctx: &mut Ctx, dir: &str, w: u32, seed: u64, ops: usize, blobs: usize) -> R<()> {
    let (ns, opened) = timed(|| Repository::open(dir));
    let mut s = ctx.sample("open", 0, ns, &opened);
    s.process = Some(w);
    ctx.push(s);
    let mut repo = opened?;
    // Prepare one distinct manifest per operation so every winner is traceable.
    let mut manifests = Vec::new();
    for i in 0..ops {
        let payload = format!("contention w{w} i{i} seed{seed}");
        let blob = retried(
            ctx,
            &mut repo,
            dir,
            w,
            "prep_put_blob",
            i as u64,
            |s: &mut Sample, id: Option<&ObjectId>| {
                s.bytes = Some(payload.len() as u64);
                s.content = Some(payload.clone());
                s.object = id.map(|id| id.to_string());
            },
            |r| r.put_blob(payload.as_bytes()),
        )?;
        let manifest = Manifest::new([(
            RepoPath::parse(&format!("contention/w{w}-i{i}"))?,
            FileEntry::Regular {
                content: blob,
                executable: false,
            },
        )])?;
        let input = serde_json::to_string(manifest.entries())?;
        manifests.push(retried(
            ctx,
            &mut repo,
            dir,
            w,
            "prep_put_manifest",
            i as u64,
            |s: &mut Sample, id: Option<&ObjectId>| {
                s.content = Some(input.clone());
                s.object = id.map(|id| id.to_string());
            },
            |r| r.put_manifest(&manifest),
        )?);
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
    ctx.push(Sample {
        scenario: "contention".into(),
        op: "barrier_release".into(),
        pid: std::process::id(),
        process: Some(w),
        outcome: "ok".into(),
        start_unix_ns: Some(unix_ns()),
        param: Some(target),
        ..Default::default()
    });
    let mut executed = 0u64;
    for (i, manifest) in manifests.iter().enumerate() {
        if ctx.expired() {
            ctx.note(&format!(
                "worker {w} stopped before op {i}: budget exhausted"
            ));
            break;
        }
        let history = retried(
            ctx,
            &mut repo,
            dir,
            w,
            "read_head",
            i as u64,
            |s: &mut Sample, h: Option<&Vec<javelin::WorldVersion>>| {
                s.result = h.and_then(|h| h.last()).map(|v| v.number.get());
                s.param = s.result;
            },
            |r| r.history(),
        )?;
        let head = history.last().map(|v| v.number).ok_or("empty history")?;
        executed += 1;
        let operation = format!("w{w}-i{i}");
        for attempt in 0..=MAX_RETRIES {
            let (s, result) = advance(
                ctx,
                &mut repo,
                "compare_and_advance",
                &operation,
                head.get(),
                *manifest,
                i as u64,
                attempt,
                Some(w),
            );
            let again = matches!(&result, Err(e) if retryable(e.code())) && attempt < MAX_RETRIES;
            ctx.push(s);
            if !again {
                break;
            }
            reopen(ctx, &mut repo, dir, w)?;
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
        // An exhausted uncertain error is a measured outcome; the parent resolves it.
        let _ = retried(
            ctx,
            &mut repo,
            dir,
            w,
            "put_blob",
            j as u64,
            |s: &mut Sample, id: Option<&ObjectId>| {
                s.content = Some(key.clone());
                s.bytes = Some(bytes.len() as u64);
                s.object = id.map(|id| id.to_string());
            },
            |r| r.put_blob(&bytes),
        );
    }
    ctx.push(Sample {
        scenario: "contention".into(),
        op: "worker_end".into(),
        pid: std::process::id(),
        process: Some(w),
        outcome: "ok".into(),
        param: Some(executed),
        ..Default::default()
    });
    Ok(())
}

/// Reopens after an uncertain error; its cost is a separate sample, not part of any retry.
fn reopen(ctx: &mut Ctx, repo: &mut Repository, dir: &str, w: u32) -> R<()> {
    let (ns, opened) = timed(|| Repository::open(dir));
    let mut s = ctx.sample("reopen", 0, ns, &opened);
    s.process = Some(w);
    ctx.push(s);
    *repo = opened?;
    Ok(())
}

fn resolve_reopen(ctx: &mut Ctx, dir: &Path, prior: &Sample) -> R<Repository> {
    let (ns, opened) = timed(|| Repository::open(dir));
    let mut s = ctx.sample("resolve_reopen", prior.index, ns, &opened);
    s.process = prior.process;
    s.attempt = prior.attempt + 1;
    s.operation = prior.operation.clone();
    s.expected = prior.expected;
    s.manifest = prior.manifest.clone();
    s.content = prior.content.clone();
    s.bytes = prior.bytes;
    ctx.push(s);
    Ok(opened?)
}

pub fn run(ctx: &mut Ctx) -> R<()> {
    let p = ctx.params.clone();
    let dir = ctx.repo_dir()?;
    let mut seeded = Vec::new();
    {
        let mut repo = Repository::init(&dir)?;
        let head = prefill(ctx, &mut repo, &mut seeded)?;
        baseline(ctx, &mut repo, head, &mut seeded)?;
    }
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
    // A worker that cannot reach the barrier keeps its raw error samples; the rest proceed.
    let mut failures = Vec::new();
    let mut ready = Vec::new();
    for (w, reader) in readers.iter_mut().enumerate() {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        ready.push(line.trim() == "ready");
        if !ready[w] {
            failures.push(format!("worker {w} failed before the barrier"));
        }
    }
    let target = unix_ns() + BARRIER_LEAD_NS;
    for (child, ok) in children.iter_mut().zip(&ready) {
        if *ok {
            let stdin = child.stdin.as_mut().expect("piped stdin");
            writeln!(stdin, "go {target}")?;
            stdin.flush()?;
        }
    }
    for (w, child) in children.iter_mut().enumerate() {
        drop(child.stdin.take());
        let status = child.wait()?;
        if !status.success() && ready[w] {
            failures.push(format!("worker {w} exited with {status}"));
        }
    }
    let mut samples: Vec<Sample> = Vec::new();
    for w in 0..p.procs {
        let file = workers_dir.join(format!("worker-{w}.jsonl"));
        if !file.exists() {
            failures.push(format!("worker {w} wrote no records"));
            continue;
        }
        for record in read_records(&file)? {
            match record {
                Record::Sample(s) => samples.push(s),
                other => ctx.records.push(other),
            }
        }
    }
    for s in &samples {
        ctx.records.push(Record::Sample(s.clone()));
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
        let mut repo = resolve_reopen(ctx, &dir, s)?;
        let manifest = ObjectId::parse(s.manifest.as_deref().unwrap())?;
        let (mut r, _) = advance(
            ctx,
            &mut repo,
            "resolve_unresolved",
            operation,
            s.expected.unwrap(),
            manifest,
            0,
            s.attempt + 1,
            s.process,
        );
        r.index = s.index;
        ctx.push(r.clone());
        resolutions.push(r);
    }
    // A blob put whose final attempt was an uncertain error is re-put with the same
    // bytes (content-addressed, so safe) to learn whether it was stored.
    let mut last_put: BTreeMap<(Option<u32>, u64), Sample> = BTreeMap::new();
    for s in samples.iter().filter(|s| s.op == "put_blob") {
        let key = (s.process, s.index);
        if last_put.get(&key).is_none_or(|l| l.attempt <= s.attempt) {
            last_put.insert(key, s.clone());
        }
    }
    for s in last_put.values().filter(|s| s.outcome == "error") {
        let mut repo = resolve_reopen(ctx, &dir, s)?;
        let key = s.content.clone().unwrap_or_default();
        let bytes = blob_bytes(p.seed, &key);
        let (ns, result) = timed(|| repo.put_blob(&bytes));
        let mut r = ctx.sample("resolve_blob_put", s.index, ns, &result);
        r.process = s.process;
        r.attempt = s.attempt + 1;
        r.content = Some(key);
        r.bytes = Some(bytes.len() as u64);
        r.object = result.as_ref().ok().map(|id| id.to_string());
        ctx.push(r.clone());
        resolutions.push(r);
    }
    samples.extend(resolutions);
    samples.extend(seeded);
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
    // Replaying every operation's final outcome must return the identical result and
    // leave the repository files byte-identical.
    let mut finals: BTreeMap<String, Sample> = BTreeMap::new();
    for s in samples
        .iter()
        .filter(|s| s.op.ends_with("compare_and_advance") || s.op == "resolve_unresolved")
    {
        let key = s.operation.clone().unwrap();
        if finals.get(&key).is_none_or(|l| l.attempt <= s.attempt) {
            finals.insert(key, s.clone());
        }
    }
    let before = dir_digest(&dir.join(".javelin"));
    let mut first_manifests = Vec::new();
    for (operation, fin) in &finals {
        let manifest = ObjectId::parse(fin.manifest.as_deref().unwrap())?;
        first_manifests.push((operation.clone(), manifest));
        let (mut s, result) = advance(
            ctx,
            &mut repo,
            "exact_retry_check",
            operation,
            fin.expected.unwrap(),
            manifest,
            0,
            0,
            None,
        );
        let (kind, number) = match &result {
            Ok(AdvanceOutcome::Accepted { version }) => ("accepted", Some(version.number.get())),
            Ok(AdvanceOutcome::Stale { actual }) => ("stale", Some(actual.get())),
            Err(_) => ("error", None),
        };
        s.outcome = if kind == fin.outcome && number == fin.result {
            "ok"
        } else {
            "mismatch"
        }
        .into();
        ctx.push(s);
    }
    let unchanged = before == dir_digest(&dir.join(".javelin"));
    ctx.push(Sample {
        scenario: ctx.name.clone(),
        op: "replay_database_unchanged".into(),
        pid: std::process::id(),
        outcome: if unchanged { "ok" } else { "mismatch" }.into(),
        param: Some(finals.len() as u64),
        ..Default::default()
    });
    if let [(operation, _), .., (_, other)] = first_manifests.as_slice() {
        let fin = &finals[operation];
        let (mut s, result) = advance(
            ctx,
            &mut repo,
            "operation_id_reuse_check",
            operation,
            fin.expected.unwrap(),
            *other,
            0,
            0,
            None,
        );
        s.outcome = match &result {
            Err(e) if e.code() == "operation_id_reused" => "ok",
            _ => "mismatch",
        }
        .into();
        ctx.push(s);
    }
    // Every acknowledged blob must read back byte-for-byte equal to its generated payload.
    let mut blob_finals: BTreeMap<(Option<u32>, u64), &Sample> = BTreeMap::new();
    for s in samples
        .iter()
        .filter(|s| s.op == "put_blob" || s.op == "resolve_blob_put")
    {
        let key = (s.process, s.index);
        if blob_finals.get(&key).is_none_or(|l| l.attempt <= s.attempt) {
            blob_finals.insert(key, s);
        }
    }
    for s in blob_finals.values().filter(|s| s.outcome == "ok") {
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
    ctx.disk("after_contention", None, Some(check.objects as u64))?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; ").into())
    }
}

/// Hash of every repository file; proves replays do not modify stored bytes.
fn dir_digest(dir: &Path) -> String {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    names.sort();
    let mut all = String::new();
    for name in names {
        let bytes = std::fs::read(&name).unwrap_or_default();
        all.push_str(&format!(
            "{}:{};",
            name.file_name().unwrap().to_string_lossy(),
            sha256_hex(&bytes)
        ));
    }
    sha256_hex(all.as_bytes())
}

/// Builds an accepted history before the barrier so each contended advance has a
/// non-trivial World to validate while holding the write lock. Same public APIs only.
fn prefill(ctx: &mut Ctx, repo: &mut Repository, out: &mut Vec<Sample>) -> R<u64> {
    let (versions, files) = (ctx.params.prefill_versions, ctx.params.prefill_files);
    if versions == 0 {
        return Ok(0);
    }
    let fx = ctx.fixture();
    let blob = |i: usize, v: u64| {
        let mut c = fx.content(i, v);
        c.truncate(BLOB_BYTES);
        c
    };
    let paths: Vec<RepoPath> = (0..files)
        .map(|i| RepoPath::parse(&format!("prefill/p{i}")))
        .collect::<Result<_, _>>()?;
    let mut ids = Vec::with_capacity(files);
    for i in 0..files {
        ids.push(put_blob(
            ctx,
            repo,
            "prefill_put_blob",
            i as u64,
            &blob(i, 0),
        )?);
    }
    let changed = (files / 10).max(1);
    for v in 1..=versions {
        if ctx.expired() {
            ctx.note("budget exhausted during contention prefill; run is truncated");
            return Ok(v as u64 - 1);
        }
        for j in 0..changed {
            let i = (v * changed + j) % files;
            ids[i] = put_blob(
                ctx,
                repo,
                "prefill_put_blob",
                (v * 1000 + j) as u64,
                &blob(i, v as u64),
            )?;
        }
        let manifest =
            Manifest::new(
                paths
                    .iter()
                    .cloned()
                    .zip(ids.iter().map(|id| FileEntry::Regular {
                        content: *id,
                        executable: false,
                    })),
            )?;
        let manifest = put_manifest(ctx, repo, "prefill_put_manifest", v as u64, &manifest)?;
        let (s, result) = advance(
            ctx,
            repo,
            "prefill_compare_and_advance",
            &format!("prefill-{v}"),
            v as u64 - 1,
            manifest,
            v as u64,
            0,
            None,
        );
        ctx.push(s.clone());
        out.push(s);
        result?;
    }
    Ok(versions as u64)
}

/// Uncontended single-process advances on the same pre-barrier state: the write-lock
/// hold time to read against any busy outcome under contention.
fn baseline(ctx: &mut Ctx, repo: &mut Repository, mut head: u64, out: &mut Vec<Sample>) -> R<()> {
    for i in 0..ctx.params.baseline_ops {
        if ctx.expired() {
            ctx.note("budget exhausted during contention baseline; run is truncated");
            break;
        }
        let payload = format!("baseline i{i} seed{}", ctx.params.seed);
        let blob = put_blob(ctx, repo, "baseline_put_blob", i as u64, payload.as_bytes())?;
        let manifest = Manifest::new([(
            RepoPath::parse(&format!("baseline/b{i}"))?,
            FileEntry::Regular {
                content: blob,
                executable: false,
            },
        )])?;
        let manifest = put_manifest(ctx, repo, "baseline_put_manifest", i as u64, &manifest)?;
        let (s, result) = advance(
            ctx,
            repo,
            "baseline_compare_and_advance",
            &format!("baseline-{i}"),
            head,
            manifest,
            i as u64,
            0,
            None,
        );
        ctx.push(s.clone());
        out.push(s);
        result?;
        head += 1;
    }
    Ok(())
}
