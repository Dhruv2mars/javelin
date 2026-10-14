//! `run`: validates bounds, records provenance, runs each scenario in its own process
//! (so peak memory is per scenario), merges raw records, then verifies the result.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

use serde_json::{Value, json};

use crate::{
    contention,
    ctx::{Ctx, R},
    host::{executable_sha256, host_info},
    params::Params,
    record::{Record, Scenario, read_records, summarize, unix_ns, write_records},
    scenarios, verify,
};

pub const SCENARIOS: [&str; 4] = ["ingest", "history", "layers", "contention"];

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn number<T: std::str::FromStr>(args: &[String], name: &str, slot: &mut T) -> R<()> {
    if let Some(v) = opt(args, name) {
        *slot = v
            .parse()
            .map_err(|_| format!("{name} expects a number, got {v:?}"))?;
    }
    Ok(())
}

pub fn parse_params(args: &[String]) -> R<Params> {
    let seed: u64 = opt(args, "--seed")
        .map(|s| s.parse())
        .transpose()
        .map_err(|_| "--seed expects a number")?
        .unwrap_or(1);
    let mut p = Params::profile(&opt(args, "--profile").unwrap_or_else(|| "ci".into()), seed)?;
    number(args, "--files", &mut p.files)?;
    number(args, "--history", &mut p.history)?;
    number(args, "--changed-per-version", &mut p.changed_per_version)?;
    number(args, "--layer-files", &mut p.layer_files)?;
    number(args, "--layer-rounds", &mut p.layer_rounds)?;
    number(args, "--layer-changed", &mut p.layer_changed)?;
    number(args, "--clones", &mut p.clones)?;
    number(args, "--big-blob-bytes", &mut p.big_blob_bytes)?;
    number(args, "--procs", &mut p.procs)?;
    number(args, "--ops", &mut p.ops)?;
    number(args, "--blobs", &mut p.blobs)?;
    number(args, "--prefill-versions", &mut p.prefill_versions)?;
    number(args, "--prefill-files", &mut p.prefill_files)?;
    number(args, "--baseline-ops", &mut p.baseline_ops)?;
    number(args, "--budget-secs", &mut p.budget_secs)?;
    p.validate()?;
    Ok(p)
}

pub fn run(args: &[String]) -> R<bool> {
    let params = parse_params(args)?;
    let out: PathBuf = opt(args, "--out").ok_or("--out DIR is required")?.into();
    let release = env!("NB_PROFILE") == "release";
    if !release && !args.iter().any(|a| a == "--allow-debug-build") {
        return Err("refusing a non-release build; use cargo build --release (or --allow-debug-build for tests)".into());
    }
    if out.exists() && fs::read_dir(&out)?.next().is_some() {
        return Err(format!("{} is not empty; refusing to mix evidence", out.display()).into());
    }
    fs::create_dir_all(out.join("records"))?;
    fs::create_dir_all(out.join("logs"))?;
    let out = fs::canonicalize(&out)?;
    let workdir: PathBuf = opt(args, "--workdir")
        .map(PathBuf::from)
        .unwrap_or_else(|| out.join("work"));
    if workdir.exists() && fs::read_dir(&workdir)?.next().is_some() {
        return Err(format!("{} is not empty", workdir.display()).into());
    }
    fs::create_dir_all(&workdir)?;
    let workdir = fs::canonicalize(&workdir)?;
    let host = host_info(&workdir);
    let started = unix_ns();
    let deadline = started + params.budget_secs * 1_000_000_000;
    let mut run = json!({
        "tool": "javelin-native-benchmarks 0.1.0",
        "command": std::env::args().collect::<Vec<_>>(),
        "params": params,
        "host": host,
        "provenance": {
            "source_commit_supplied_by_caller": opt(args, "--source-commit"),
            "javelin_source_sha256": env!("NB_JAVELIN_SOURCE_SHA256"),
            "tool_source_sha256": env!("NB_TOOL_SOURCE_SHA256"),
            "executable_sha256": executable_sha256(),
            "build_target": env!("NB_TARGET"),
            "build_profile": env!("NB_PROFILE"),
            "debug_build_allowed": !release,
            "git": "never invoked; commit identity is caller-supplied and unverified by this tool",
        },
        "documented_settings": {
            "sqlite_busy_timeout_secs": 5,
            "sqlite_synchronous": "EXTRA",
            "sqlite_fullfsync": "ON",
            "source": "src/repository.rs connect() at the measured source; read, not set, by this tool",
            "contention_retries": contention::MAX_RETRIES,
            "bundled_sqlite_library_version": rusqlite::version(),
            "journal_mode": "DELETE (enforced by connect(); not readable through the public API)",
            "durability_note": "synchronous=EXTRA and fullfsync=ON are set per connection; fullfsync only affects macOS, so macOS and Linux latencies are not durability-equivalent",
            "os_cache": "not dropped; samples are warm-cache unless the operation writes",
        },
        "estimated_validation_bytes": params.estimated_validation_bytes(),
        "estimate_note": "estimate of stored bytes a write may re-read, from reading the source; not measured, not a stated cause of any latency",
        "procs_exceed_logical_cpus": std::thread::available_parallelism().map(|n| params.procs > n.get()).ok(),
        "started_unix_ns": started,
        "limits": [
            "public API only; no filesystem-throughput, hosted-provider, free-tier, scale or power-loss claim",
            "no timing thresholds; samples are small fixed-seed baselines, not requirements",
            "Windows unmeasured: this tool records peak memory and physical disk on Unix only",
        ],
    });
    fs::write(out.join("run.json"), serde_json::to_vec_pretty(&run)?)?;
    let exe = std::env::current_exe()?;
    let mut records: Vec<Record> = Vec::new();
    let mut ok = true;
    for name in SCENARIOS {
        let log = fs::File::create(out.join("logs").join(format!("{name}.stderr.log")))?;
        let wall = Instant::now();
        let status = Command::new(&exe)
            .args(["scenario", name, "--out"])
            .arg(&out)
            .arg("--workdir")
            .arg(&workdir)
            .args(["--params-json", &serde_json::to_string(&params)?])
            .args(["--deadline-unix-ns", &deadline.to_string()])
            .stdout(Stdio::null())
            .stderr(log)
            .status()?;
        let wall_ns = wall.elapsed().as_nanos() as u64;
        let rec = out.join("records").join(format!("{name}.jsonl"));
        if rec.exists() {
            records.extend(read_records(&rec)?);
        } else {
            records.push(Record::Note {
                scenario: name.into(),
                message: "scenario wrote no records".into(),
            });
        }
        ok &= status.success();
        records.push(Record::Scenario(Scenario {
            name: name.into(),
            wall_ns,
            exit_code: status.code(),
        }));
    }
    write_records(&out.join("samples.jsonl"), &records)?;
    let summary = summarize(&records);
    fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    let ingest: Value = fs::read(out.join("inventory-ingest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    let inventory = json!({
        "params": params,
        "ingest": ingest,
        "history": {"files": params.files, "versions": params.history, "changed_per_version": params.changed_per_version,
                    "manifest_entries_total": params.files * params.history},
        "layers": {"base_files": params.layer_files, "rounds": params.layer_rounds, "changed_per_round": params.layer_changed,
                   "clones": params.clones, "shared_blob_bytes": params.big_blob_bytes, "change_blob_max_bytes": 16384},
        "contention": {"processes": params.procs, "advance_ops_per_process": params.ops, "blobs_per_process": params.blobs,
                       "blob_bytes": 4096, "prefill_versions": params.prefill_versions, "prefill_files": params.prefill_files, "baseline_ops": params.baseline_ops, "shared_blobs": "even indexes identical across processes; odd indexes unique"},
    });
    fs::write(
        out.join("fixture-inventory.json"),
        serde_json::to_vec_pretty(&inventory)?,
    )?;
    let finished = unix_ns();
    run["finished_unix_ns"] = json!(finished);
    run["wall_ns"] = json!(finished - started);
    run["truncated_by_budget"] = json!(records.iter().any(
        |r| matches!(r, Record::Note { message, .. } if message.contains("budget exhausted"))
    ));
    fs::write(out.join("run.json"), serde_json::to_vec_pretty(&run)?)?;
    if !args.iter().any(|a| a == "--keep-repos") {
        let _ = fs::remove_dir_all(&workdir);
    }
    let report = verify_and_print(&out, args.iter().any(|a| a == "--allow-debug-build"))?;
    Ok(ok && report)
}

pub fn verify_and_print(dir: &Path, allow_debug: bool) -> R<bool> {
    let checks = verify::verify(dir, allow_debug)?;
    let mut all = true;
    for c in &checks {
        println!(
            "{} {}: {}",
            if c.ok { "PASS" } else { "FAIL" },
            c.name,
            c.detail
        );
        all &= c.ok;
    }
    println!(
        "{}",
        if all {
            "VERIFY: all recorded invariants hold"
        } else {
            "VERIFY: FAILED"
        }
    );
    Ok(all)
}

/// Internal: run one scenario in this process. Records are written even when it fails.
pub fn scenario(args: &[String]) -> R<()> {
    let name = args.get(1).ok_or("scenario name required")?.clone();
    let out: PathBuf = opt(args, "--out").ok_or("--out required")?.into();
    let workdir: PathBuf = opt(args, "--workdir").ok_or("--workdir required")?.into();
    let params: Params =
        serde_json::from_str(&opt(args, "--params-json").ok_or("--params-json required")?)?;
    let deadline: u64 = opt(args, "--deadline-unix-ns")
        .ok_or("--deadline-unix-ns required")?
        .parse()?;
    let mut ctx = Ctx {
        name: name.clone(),
        params,
        out: out.clone(),
        dir: workdir.join(&name),
        deadline_unix_ns: deadline,
        records: vec![],
    };
    let result = match name.as_str() {
        "ingest" => scenarios::ingest(&mut ctx),
        "history" => scenarios::history(&mut ctx),
        "layers" => scenarios::layers(&mut ctx),
        "contention" => contention::run(&mut ctx),
        other => Err(format!("unknown scenario {other}").into()),
    };
    ctx.memory(None);
    write_records(
        &out.join("records").join(format!("{name}.jsonl")),
        &ctx.records,
    )?;
    result
}
