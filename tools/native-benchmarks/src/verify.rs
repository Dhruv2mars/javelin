//! Offline re-check of a finished run directory against its raw records. Nothing here
//! opens a repository or trusts the summary; it recomputes from raw samples.
use std::{collections::BTreeMap, path::Path};

use serde_json::Value;

use crate::{
    ctx::R,
    record::{Record, Sample, read_records, summarize},
};

pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

fn check(name: &'static str, ok: bool, detail: impl Into<String>) -> Check {
    Check {
        name,
        ok,
        detail: detail.into(),
    }
}

fn json(dir: &Path, name: &str) -> R<Value> {
    Ok(serde_json::from_slice(
        &std::fs::read(dir.join(name)).map_err(|e| format!("{name}: {e}"))?,
    )?)
}

fn retryable(code: Option<&str>) -> bool {
    matches!(code, Some("storage_error" | "io_error"))
}

pub fn verify(dir: &Path) -> R<Vec<Check>> {
    let records = read_records(&dir.join("samples.jsonl"))?;
    let summary = json(dir, "summary.json")?;
    let run = json(dir, "run.json")?;
    let inventory = json(dir, "fixture-inventory.json")?;
    let history = json(dir, "history.json")?;
    let integrity = json(dir, "integrity.json")?;
    let samples: Vec<&Sample> = records
        .iter()
        .filter_map(|r| {
            if let Record::Sample(s) = r {
                Some(s)
            } else {
                None
            }
        })
        .collect();
    let mut out = Vec::new();

    out.push(check(
        "summary_matches_raw",
        summarize(&records) == summary,
        "summary.json recomputed from samples.jsonl",
    ));
    let hex64 = |v: &Value| {
        v.as_str()
            .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    };
    let prov = &run["provenance"];
    out.push(check(
        "provenance_present",
        hex64(&prov["executable_sha256"])
            && hex64(&prov["javelin_source_sha256"])
            && hex64(&prov["tool_source_sha256"])
            && run["host"]["os"].is_string()
            && run["host"]["rustc"].is_string()
            && run["command"].is_array()
            && inventory["params"] == run["params"],
        "executable/source hashes, host, rustc, command and params consistent",
    ));
    let clean = |name: &str| {
        records
            .iter()
            .any(|r| matches!(r, Record::Scenario(s) if s.name == name && s.exit_code == Some(0)))
    };
    let missing: Vec<_> = ["ingest", "history", "layers", "contention"]
        .into_iter()
        .filter(|n| !clean(n))
        .collect();
    out.push(check(
        "scenarios_exited_cleanly",
        missing.is_empty(),
        if missing.is_empty() {
            "ingest, history, layers, contention exited 0".into()
        } else {
            format!("not clean: {missing:?}")
        },
    ));
    let stray = samples
        .iter()
        .filter(|s| s.scenario != "contention" && s.outcome == "error")
        .count();
    out.push(check(
        "no_errors_outside_contention",
        stray == 0,
        format!("{stray} error samples"),
    ));

    // Ingest: identical content must yield identical object IDs, and the inventory must agree.
    let mut by_content: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for s in samples
        .iter()
        .filter(|s| s.scenario == "ingest" && s.op.starts_with("put_blob"))
    {
        if let (Some(c), Some(o)) = (&s.content, &s.object) {
            by_content.entry(c).or_default().push(o);
        }
    }
    let stable = by_content
        .values()
        .all(|ids| ids.windows(2).all(|w| w[0] == w[1]));
    let unique: std::collections::BTreeSet<_> = samples
        .iter()
        .filter(|s| s.scenario == "ingest" && s.op == "put_blob")
        .filter_map(|s| s.object.as_deref())
        .collect();
    out.push(check(
        "ingest_ids_stable_and_inventoried",
        stable && !by_content.is_empty() && inventory["ingest"]["unique_objects"] == unique.len(),
        format!(
            "{} contents, {} unique objects",
            by_content.len(),
            unique.len()
        ),
    ));

    // History: sequential acceptance with no stale outcomes.
    let hist: Vec<_> = samples
        .iter()
        .filter(|s| s.scenario == "history" && s.op == "compare_and_advance")
        .collect();
    let sequential = hist.iter().enumerate().all(|(n, s)| {
        s.outcome == "accepted" && s.result == Some(n as u64 + 1) && s.expected == Some(n as u64)
    });
    out.push(check(
        "history_advances_sequential",
        sequential && !hist.is_empty(),
        format!("{} advances", hist.len()),
    ));

    // Layers: every capture applied; clone storage is not proportional to the shared payload.
    let captures = samples
        .iter()
        .filter(|s| s.scenario == "layers" && s.op.starts_with("capture_layer"));
    let all_applied = captures
        .clone()
        .all(|s| s.op == "capture_layer_big" || s.outcome == "applied");
    out.push(check(
        "layer_captures_applied",
        all_applied && captures.count() > 0,
        "capture outcomes",
    ));
    let disks: Vec<_> = records
        .iter()
        .filter_map(|r| {
            if let Record::Disk(d) = r {
                Some(d)
            } else {
                None
            }
        })
        .filter(|d| d.scenario == "layers")
        .collect();
    let before = disks.iter().find(|d| d.label == "before_clones");
    let clones: Vec<_> = disks
        .iter()
        .filter(|d| d.label.starts_with("after_clone_"))
        .collect();
    let (ok, detail) = match (before, clones.last()) {
        (Some(b), Some(l)) => {
            let n = clones.len() as u64;
            let payload = b.payload_bytes.unwrap_or(0);
            let growth = l.logical_bytes.saturating_sub(b.logical_bytes);
            if payload * n < 64 * 1024 {
                (
                    true,
                    format!(
                        "skipped: shared payload {payload} B x {n} clones too small to distinguish"
                    ),
                )
            } else {
                (
                    growth < payload * n / 2,
                    format!("{n} clones grew logical bytes by {growth}; payload {payload}"),
                )
            }
        }
        _ => (false, "missing before_clones or clone disk records".into()),
    };
    out.push(check("clone_storage_not_payload_proportional", ok, detail));

    // Contention.
    let cas: Vec<&Sample> = samples
        .iter()
        .copied()
        .filter(|s| s.op == "compare_and_advance" || s.op == "resolve_unresolved")
        .filter(|s| s.scenario == "contention")
        .collect();
    let mut groups: BTreeMap<&str, Vec<&Sample>> = BTreeMap::new();
    for s in &cas {
        groups
            .entry(s.operation.as_deref().unwrap_or(""))
            .or_default()
            .push(s);
    }
    for attempts in groups.values_mut() {
        attempts.sort_by_key(|s| s.attempt);
    }
    let barrier: Vec<_> = samples
        .iter()
        .filter(|s| s.op == "barrier_release")
        .collect();
    let pids: std::collections::BTreeSet<_> = barrier.iter().map(|s| s.pid).collect();
    let procs = run["params"]["procs"].as_u64().unwrap_or(0) as usize;
    out.push(check(
        "contention_used_distinct_processes",
        procs >= 2 && barrier.len() == procs && pids.len() == procs,
        format!("{} workers, {} distinct pids", barrier.len(), pids.len()),
    ));
    let retry_ok = groups.values().all(|a| {
        let same_input = a
            .iter()
            .all(|s| s.expected == a[0].expected && s.manifest == a[0].manifest);
        let seq = a.windows(2).all(|w| w[0].attempt < w[1].attempt);
        let only_after_uncertain = a[..a.len() - 1]
            .iter()
            .all(|s| s.outcome == "error" && retryable(s.error_code.as_deref()));
        same_input && seq && only_after_uncertain
    });
    out.push(check(
        "retries_repeat_exact_input_after_uncertain_errors",
        retry_ok && !groups.is_empty(),
        format!("{} operations", groups.len()),
    ));
    let finals: Vec<(&str, &Sample)> = groups
        .iter()
        .map(|(k, v)| (*k, *v.last().unwrap()))
        .collect();
    let unresolved = finals
        .iter()
        .filter(|(_, f)| f.outcome != "accepted" && f.outcome != "stale")
        .count();
    out.push(check(
        "every_operation_has_durable_outcome",
        unresolved == 0,
        format!("{unresolved} unresolved"),
    ));
    let hv = history.as_array().cloned().unwrap_or_default();
    let number = |v: &Value| v["number"].as_u64();
    let contiguous = hv
        .iter()
        .enumerate()
        .all(|(n, v)| number(v) == Some(n as u64));
    let accepted: Vec<_> = finals
        .iter()
        .filter(|(_, f)| f.outcome == "accepted")
        .collect();
    let mut winners: BTreeMap<u64, &str> = BTreeMap::new();
    let mut unique_winner = true;
    for (op, f) in &accepted {
        unique_winner &= winners.insert(f.result.unwrap_or(0), op).is_none()
            && f.result == f.expected.map(|e| e + 1);
    }
    out.push(check(
        "one_ordered_winner_per_version",
        contiguous
            && unique_winner
            && hv.len() == accepted.len() + 1
            && hv.first().is_some_and(|v| v["operation"].is_null()),
        format!(
            "{} history entries, {} accepted operations",
            hv.len(),
            accepted.len()
        ),
    ));
    let acknowledged_in_history = accepted.iter().all(|(op, f)| {
        hv.iter().any(|v| {
            number(v) == f.result
                && v["operation"].as_str() == Some(op)
                && v["manifest"].as_str() == f.manifest.as_deref()
        })
    });
    let no_unexplained = hv.iter().skip(1).all(|v| {
        v["operation"]
            .as_str()
            .is_some_and(|op| winners.values().any(|w| *w == op))
    });
    out.push(check(
        "history_matches_acknowledged_accepts",
        acknowledged_in_history && no_unexplained,
        "every accept present once with its operation and manifest; no unexplained winner",
    ));
    let head = hv.len().saturating_sub(1) as u64;
    let stale_ok = finals
        .iter()
        .filter(|(_, f)| f.outcome == "stale")
        .all(|(op, f)| {
            f.expected < f.result
                && f.result.is_some_and(|r| r <= head)
                && !hv.iter().any(|v| v["operation"].as_str() == Some(op))
        });
    out.push(check(
        "stale_outcomes_never_create_versions",
        stale_ok,
        "stale actual head > expected and operation absent from history",
    ));
    out.push(check(
        "integrity_matches_history_and_attempts",
        integrity["versions"].as_u64() == Some(hv.len() as u64)
            && integrity["attempts"].as_u64() == Some(groups.len() as u64)
            && integrity["layers"].as_u64() == Some(0),
        format!("check(): {integrity}"),
    ));
    let retries: Vec<_> = samples
        .iter()
        .filter(|s| s.op == "exact_retry_check")
        .collect();
    out.push(check(
        "same_operation_retry_returns_identical_outcome",
        retries.len() == groups.len()
            && retries.iter().all(|s| s.outcome == "ok")
            && !retries.is_empty(),
        format!("{} retried operations", retries.len()),
    ));
    let reuse: Vec<_> = samples
        .iter()
        .filter(|s| s.op == "operation_id_reuse_check")
        .collect();
    out.push(check(
        "operation_id_reuse_rejected",
        reuse.len() == 1 && reuse[0].outcome == "ok",
        "same ID with different manifest",
    ));
    let puts: Vec<_> = samples
        .iter()
        .filter(|s| s.scenario == "contention" && s.op == "put_blob")
        .collect();
    let acked = puts.iter().filter(|s| s.outcome == "ok").count();
    let read: Vec<_> = samples.iter().filter(|s| s.op == "blob_readback").collect();
    let mut ids_by_key: BTreeMap<&str, std::collections::BTreeSet<&str>> = BTreeMap::new();
    for s in puts.iter().filter(|s| s.outcome == "ok") {
        ids_by_key
            .entry(s.content.as_deref().unwrap_or(""))
            .or_default()
            .insert(s.object.as_deref().unwrap_or(""));
    }
    let distinct: std::collections::BTreeSet<_> = ids_by_key.values().flatten().collect();
    out.push(check(
        "acknowledged_blobs_read_back_exactly",
        acked > 0
            && read.len() == acked
            && read.iter().all(|s| s.outcome == "ok")
            && ids_by_key.values().all(|v| v.len() == 1)
            && distinct.len() == ids_by_key.len(),
        format!(
            "{acked} acknowledged puts, {} read back, {} distinct contents",
            read.len(),
            ids_by_key.len()
        ),
    ));
    Ok(out)
}
