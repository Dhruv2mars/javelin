//! Offline re-check of a finished run directory against its raw records. Nothing here
//! opens a repository or trusts the summary; it recomputes from raw samples.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

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

fn action(op: &str) -> Option<&str> {
    match op {
        "resolve_unresolved" => Some("compare_and_advance"),
        "resolve_blob_put" => Some("put_blob"),
        "compare_and_advance"
        | "put_blob"
        | "prep_put_blob"
        | "prep_put_manifest"
        | "read_head" => Some(op),
        _ => None,
    }
}

fn same_input(a: &Sample, b: &Sample) -> bool {
    a.process == b.process
        && a.index == b.index
        && a.operation == b.operation
        && a.expected == b.expected
        && a.manifest == b.manifest
        && a.content == b.content
        && a.bytes == b.bytes
}

fn succeeded(s: &Sample) -> bool {
    if action(&s.op) == Some("compare_and_advance") {
        matches!(s.outcome.as_str(), "accepted" | "stale") && s.result.is_some()
    } else if action(&s.op) == Some("read_head") {
        s.outcome == "ok" && s.result.is_some()
    } else {
        s.outcome == "ok" && s.object.is_some()
    }
}

/// Raw order matters: each action starts at attempt 0; the next identical attempt
/// consumes its own successful reopen in the same worker, never a global count.
fn worker_retry_checks(samples: &[&Sample], run: &Value) -> (bool, bool, bool) {
    let procs = run["params"]["procs"].as_u64().unwrap_or(0);
    let ops = run["params"]["ops"].as_u64().unwrap_or(0);
    let blobs = run["params"]["blobs"].as_u64().unwrap_or(0);
    let seed = run["params"]["seed"].as_u64().unwrap_or(0);
    let explicit_inputs = run["record_format_version"].as_u64() == Some(2);
    if !(2..=crate::params::MAX_PROCS as u64).contains(&procs)
        || !(1..=crate::params::MAX_OPS as u64).contains(&ops)
        || !(1..=crate::params::MAX_BLOBS as u64).contains(&blobs)
    {
        return (false, false, false);
    }
    let worker_pids: BTreeMap<_, _> = samples
        .iter()
        .filter(|s| s.scenario == "contention" && s.op == "barrier_release")
        .map(|s| (s.process, s.pid))
        .collect();
    let parent_pid = samples
        .iter()
        .find(|s| s.scenario == "contention" && s.op == "final_history")
        .map(|s| s.pid);
    type WorkerActions<'a> = BTreeMap<(Option<u32>, &'a str, u64), Vec<(usize, &'a Sample)>>;
    let mut groups = WorkerActions::new();
    let mut input_ok = true;
    for (pos, s) in samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.scenario == "contention")
    {
        if let Some(op) = action(&s.op) {
            let resolving = s.op.starts_with("resolve_");
            input_ok &= s.process.is_some_and(|w| u64::from(w) < procs)
                && if resolving {
                    Some(s.pid) == parent_pid
                } else {
                    worker_pids.get(&s.process) == Some(&s.pid)
                };
            groups
                .entry((s.process, op, s.index))
                .or_default()
                .push((pos, s));
        }
    }
    let mut consumed = BTreeSet::new();
    let mut reopen_ok = true;
    for attempts in groups.values() {
        let first = attempts[0].1;
        input_ok &= first.attempt == 0 && !first.op.starts_with("resolve_");
        for (k, (pos, s)) in attempts.iter().enumerate() {
            input_ok &= s.attempt as usize == k
                && same_input(first, s)
                && (s.attempt <= crate::contention::MAX_RETRIES || s.op.starts_with("resolve_"));
            if k == 0 {
                continue;
            }
            let (prev_pos, prev) = attempts[k - 1];
            input_ok &= prev.outcome == "error" && retryable(prev.error_code.as_deref());
            let preceding: Vec<_> = samples[..*pos]
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    r.scenario == "contention" && r.process == s.process && r.pid == s.pid
                })
                .collect();
            let reopen = preceding.last().copied();
            let valid = reopen.is_some_and(|(rpos, r)| {
                let resolving = s.op.starts_with("resolve_");
                r.outcome == "ok"
                    && prev
                        .start_unix_ns
                        .zip(r.start_unix_ns)
                        .is_some_and(|(a, b)| a.checked_add(prev.ns).is_some_and(|end| end <= b))
                    && r.start_unix_ns
                        .zip(s.start_unix_ns)
                        .is_some_and(|(a, b)| a.checked_add(r.ns).is_some_and(|end| end <= b))
                    && rpos > prev_pos
                    && consumed.insert(rpos)
                    && if resolving {
                        r.op == "resolve_reopen"
                            && same_input(r, s)
                            && r.attempt == s.attempt
                            && prev.attempt == crate::contention::MAX_RETRIES
                            && prev.pid != s.pid
                    } else {
                        r.op == "reopen"
                            && r.attempt == 0
                            && r.index == 0
                            && preceding.len() >= 2
                            && preceding[preceding.len() - 2].0 == prev_pos
                    }
            });
            reopen_ok &= valid;
        }
        input_ok &= succeeded(attempts.last().unwrap().1);
    }
    let reopens: BTreeSet<_> = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            s.scenario == "contention" && matches!(s.op.as_str(), "reopen" | "resolve_reopen")
        })
        .map(|(pos, _)| pos)
        .collect();
    reopen_ok &= consumed == reopens;
    let mut expected = BTreeSet::new();
    let mut final_ok = true;
    for w in 0..procs as u32 {
        let ends: Vec<_> = samples
            .iter()
            .filter(|s| s.scenario == "contention" && s.op == "worker_end" && s.process == Some(w))
            .collect();
        final_ok &= ends.len() == 1
            && ends[0].outcome == "ok"
            && ends[0].param == Some(ops)
            && worker_pids.get(&Some(w)) == Some(&ends[0].pid);
        for i in 0..ops {
            for op in [
                "prep_put_blob",
                "prep_put_manifest",
                "read_head",
                "compare_and_advance",
            ] {
                expected.insert((Some(w), op, i));
            }
            let fin = |op| {
                groups
                    .get(&(Some(w), op, i))
                    .and_then(|a| a.last())
                    .map(|(_, s)| *s)
            };
            let payload = format!("contention w{w} i{i} seed{seed}");
            final_ok &= match (
                fin("prep_put_blob"),
                fin("prep_put_manifest"),
                fin("read_head"),
                fin("compare_and_advance"),
            ) {
                (Some(blob), Some(manifest), Some(head), Some(cas)) => {
                    // Old runs omitted setup content markers. Their inputs are defined
                    // by the recorded worker/index/seed and the fixed source generator.
                    let manifest_input = blob.object.as_ref().map(|id| serde_json::json!({
                        format!("contention/w{w}-i{i}"): {"kind": "regular", "content": id, "executable": false}
                    }));
                    let content_matches = manifest.content.as_ref().map_or(!explicit_inputs, |c| {
                        serde_json::from_str::<Value>(c).ok() == manifest_input
                    });
                    blob.bytes == Some(payload.len() as u64)
                        && blob
                            .content
                            .as_deref()
                            .map_or(!explicit_inputs, |c| c == payload)
                        && content_matches
                        && manifest.object == cas.manifest
                        && [
                            "prep_put_blob",
                            "prep_put_manifest",
                            "read_head",
                            "compare_and_advance",
                        ]
                        .windows(2)
                        .all(|pair| {
                            let a = &groups[&(Some(w), pair[0], i)];
                            let b = &groups[&(Some(w), pair[1], i)];
                            a.last().unwrap().0 < b[0].0
                        })
                        && head.result == cas.expected
                        && cas.operation.as_deref() == Some(format!("w{w}-i{i}").as_str())
                }
                _ => false,
            };
        }
        for i in 0..blobs {
            expected.insert((Some(w), "put_blob", i));
            let key = crate::contention::blob_key(w, i as usize);
            final_ok &= groups.get(&(Some(w), "put_blob", i)).is_some_and(|a| {
                let fin = a.last().unwrap().1;
                fin.content.as_deref() == Some(key.as_str())
                    && fin.bytes == Some(crate::params::CONTENTION_BLOB_BYTES as u64)
            });
        }
    }
    final_ok &= groups.keys().copied().collect::<BTreeSet<_>>() == expected;
    (input_ok, reopen_ok, final_ok)
}

pub fn verify(dir: &Path, allow_debug: bool) -> R<Vec<Check>> {
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
        "record_format_supported",
        run.get("record_format_version")
            .is_none_or(|v| matches!(v.as_u64(), Some(1 | 2))),
        "version 1 legacy generator inputs; version 2 explicit setup inputs and resolution reopens",
    ));
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
    let release = run["provenance"]["build_profile"] == "release";
    out.push(check(
        "release_build_only",
        release || allow_debug,
        format!(
            "build_profile {}; debug-build evidence needs --allow-debug-build",
            run["provenance"]["build_profile"]
        ),
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
    let disk_valid = records
        .iter()
        .filter_map(|r| {
            if let Record::Disk(d) = r {
                Some(d)
            } else {
                None
            }
        })
        .all(|d| d.stored_bytes > 0 && d.files > 0 && d.physical_bytes.is_none_or(|p| p > 0));
    out.push(check(
        "disk_probes_complete_and_nonempty",
        disk_valid,
        "initialized repositories have bytes/files; incomplete traversal fails the scenario",
    ));
    let before = disks.iter().find(|d| d.label == "before_clones");
    let clones: Vec<_> = disks
        .iter()
        .filter(|d| d.label.starts_with("after_clone_"))
        .collect();
    let (ok, detail) = match (before, clones.last()) {
        (Some(b), Some(l)) => {
            let n = clones.len() as u64;
            let payload = b.payload_bytes.unwrap_or(0);
            let mut previous = *b;
            let monotonic = clones.iter().enumerate().all(|(i, d)| {
                let ok = d.label == format!("after_clone_{}", i + 1)
                    && d.stored_bytes >= previous.stored_bytes
                    && d.files >= previous.files
                    && d.payload_bytes == b.payload_bytes;
                previous = d;
                ok
            });
            let growth = l.stored_bytes.checked_sub(b.stored_bytes);
            let valid = disk_valid
                && monotonic
                && growth.is_some()
                && clones.len() as u64 == run["params"]["clones"].as_u64().unwrap_or(0);
            if payload.saturating_mul(n) < 64 * 1024 {
                (
                    valid,
                    format!(
                        "skipped: shared payload {payload} B x {n} clones too small to distinguish"
                    ),
                )
            } else {
                (
                    valid && growth.is_some_and(|g| g < payload.saturating_mul(n) / 2),
                    format!(
                        "{n} clones grew stored bytes by {growth:?} in total; shared payload {payload}"
                    ),
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
        .filter(|s| {
            matches!(
                s.op.as_str(),
                "compare_and_advance"
                    | "resolve_unresolved"
                    | "prefill_compare_and_advance"
                    | "baseline_compare_and_advance"
            )
        })
        .filter(|s| s.scenario == "contention")
        .collect();
    let mut groups: BTreeMap<&str, Vec<&Sample>> = BTreeMap::new();
    for s in &cas {
        groups
            .entry(s.operation.as_deref().unwrap_or(""))
            .or_default()
            .push(s);
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
    let (worker_input_ok, reopen_ok, worker_final_ok) = worker_retry_checks(&samples, &run);
    let retry_ok = groups.values().all(|a| {
        a.iter()
            .enumerate()
            .all(|(k, s)| s.attempt as usize == k && same_input(a[0], s))
            && a[..a.len() - 1]
                .iter()
                .all(|s| s.outcome == "error" && retryable(s.error_code.as_deref()))
    });
    out.push(check(
        "retries_repeat_exact_input_after_uncertain_errors",
        retry_ok && worker_input_ok && !groups.is_empty(),
        "every action starts at 0 and repeats identical worker/input after its uncertain failure",
    ));
    out.push(check(
        "every_retry_followed_a_recorded_reopen",
        reopen_ok,
        "each retry consumes its own successful, ordered reopen; no orphan reopen",
    ));
    out.push(check(
        "worker_actions_match_fixture_and_final_evidence",
        worker_final_ok,
        "all worker/index actions present; setup objects, observed head and blob inputs bind final evidence",
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
            && retries.iter().all(|s| {
                s.outcome == "ok"
                    && groups
                        .get(s.operation.as_deref().unwrap_or(""))
                        .is_some_and(|a| {
                            let fin = a.last().unwrap();
                            s.expected == fin.expected
                                && s.manifest == fin.manifest
                                && s.result == fin.result
                        })
            })
            && retries
                .iter()
                .map(|s| s.operation.as_deref().unwrap_or(""))
                .collect::<std::collections::BTreeSet<_>>()
                == groups
                    .keys()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>()
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
    let mut blob_finals: BTreeMap<(Option<u32>, u64), &Sample> = BTreeMap::new();
    for s in samples.iter().filter(|s| {
        s.scenario == "contention" && (s.op == "put_blob" || s.op == "resolve_blob_put")
    }) {
        let key = (s.process, s.index);
        if blob_finals.get(&key).is_none_or(|l| l.attempt <= s.attempt) {
            blob_finals.insert(key, s);
        }
    }
    let truncated = records
        .iter()
        .any(|r| matches!(r, Record::Note { message, .. } if message.contains("budget exhausted")));
    let blobs = run["params"]["blobs"].as_u64().unwrap_or(0) as usize;
    let all_ok = blob_finals.values().all(|s| s.outcome == "ok");
    out.push(check(
        "every_blob_put_has_durable_outcome",
        all_ok && (truncated || blob_finals.len() == procs * blobs) && !blob_finals.is_empty(),
        format!(
            "{} blob puts with a final outcome; {} not acknowledged",
            blob_finals.len(),
            blob_finals.values().filter(|s| s.outcome != "ok").count()
        ),
    ));
    let acked: Vec<_> = blob_finals
        .iter()
        .filter(|(_, s)| s.outcome == "ok")
        .collect();
    let read: BTreeMap<(Option<u32>, u64), &Sample> = samples
        .iter()
        .filter(|s| s.op == "blob_readback")
        .map(|s| ((s.process, s.index), *s))
        .collect();
    let mut ids_by_key: BTreeMap<&str, std::collections::BTreeSet<&str>> = BTreeMap::new();
    for (_, s) in &acked {
        ids_by_key
            .entry(s.content.as_deref().unwrap_or(""))
            .or_default()
            .insert(s.object.as_deref().unwrap_or(""));
    }
    let distinct: std::collections::BTreeSet<_> = ids_by_key.values().flatten().collect();
    out.push(check(
        "acknowledged_blobs_read_back_exactly",
        !acked.is_empty()
            && read.len() == acked.len()
            && acked.iter().all(|(k, s)| {
                read.get(k).is_some_and(|r| {
                    r.outcome == "ok" && r.object == s.object && r.content == s.content
                })
            })
            && ids_by_key.values().all(|v| v.len() == 1)
            && distinct.len() == ids_by_key.len(),
        format!(
            "{} acknowledged puts, {} read back, {} distinct contents",
            acked.len(),
            read.len(),
            ids_by_key.len()
        ),
    ));
    let contention_errors: Vec<_> = samples
        .iter()
        .filter(|s| s.scenario == "contention" && s.outcome == "error")
        .collect();
    let busy_only = contention_errors.iter().all(|s| {
        s.sqlite_code
            .as_deref()
            .is_some_and(|c| c.starts_with("DatabaseBusy") || c.starts_with("DatabaseLocked"))
    });
    out.push(check(
        "contention_errors_are_only_busy",
        busy_only,
        format!(
            "{} error samples; any non-busy storage error or reused operation ID fails the run",
            contention_errors.len()
        ),
    ));
    let heads: BTreeMap<(Option<u32>, u64), Option<u64>> = samples
        .iter()
        .filter(|s| s.op == "read_head" && s.scenario == "contention" && s.outcome == "ok")
        .map(|s| ((s.process, s.index), s.result))
        .collect();
    let observed = samples
        .iter()
        .filter(|s| s.op == "compare_and_advance" && s.scenario == "contention" && s.attempt == 0)
        .all(|s| {
            heads
                .get(&(s.process, s.index))
                .is_some_and(|h| *h == s.expected)
        });
    out.push(check(
        "advance_expected_matches_observed_head",
        observed && !heads.is_empty(),
        "each first attempt used the head its worker had just read",
    ));
    let end = |s: &Sample| s.start_unix_ns.unwrap_or(0).saturating_add(s.ns);
    let ordered = finals
        .iter()
        .filter(|(_, f)| f.outcome == "stale")
        .all(|(_, st)| {
            accepted
                .iter()
                .all(|(_, a)| end(a) > st.start_unix_ns.unwrap_or(0) || st.result >= a.result)
        });
    out.push(check(
        "stale_actual_respects_real_time_order",
        ordered,
        "stale actual is at least every version acknowledged before that attempt started",
    ));
    let put_ops = [
        "put_blob",
        "prep_put_blob",
        "prep_put_manifest",
        "prefill_put_blob",
        "prefill_put_manifest",
        "resolve_blob_put",
        "baseline_put_blob",
        "baseline_put_manifest",
    ];
    let recorded: std::collections::BTreeSet<_> = samples
        .iter()
        .filter(|s| {
            s.scenario == "contention" && put_ops.contains(&s.op.as_str()) && s.outcome == "ok"
        })
        .filter_map(|s| s.object.as_deref())
        .collect();
    out.push(check(
        "integrity_objects_match_recorded_puts",
        integrity["objects"].as_u64() == Some(recorded.len() as u64 + 1),
        format!(
            "{} distinct recorded objects plus the World 0 manifest vs check() {}",
            recorded.len(),
            integrity["objects"]
        ),
    ));
    let unchanged: Vec<_> = samples
        .iter()
        .filter(|s| s.op == "replay_database_unchanged")
        .collect();
    out.push(check(
        "replay_left_repository_files_unchanged",
        unchanged.len() == 1 && unchanged[0].outcome == "ok",
        "repository file hash before and after replaying every operation",
    ));
    let ops_per_proc = run["params"]["ops"].as_u64().unwrap_or(0) as usize;
    let ends: Vec<_> = samples.iter().filter(|s| s.op == "worker_end").collect();
    let executed: u64 = ends.iter().filter_map(|s| s.param).sum();
    let worker_ops = groups
        .keys()
        .filter(|k| k.starts_with('w') && k.contains("-i"))
        .count() as u64;
    out.push(check(
        "workers_completed_and_all_operations_recorded",
        ends.len() == procs
            && executed == worker_ops
            && (truncated || executed == (procs * ops_per_proc) as u64),
        format!(
            "{} worker_end markers; {executed} executed vs {worker_ops} recorded operations",
            ends.len()
        ),
    ));
    out.push(check(
        "not_truncated_by_budget",
        !truncated
            && summary["truncated_scenarios"]
                .as_array()
                .is_some_and(|a| a.is_empty()),
        "a truncated run is incomplete evidence",
    ));
    let mismatches = samples.iter().filter(|s| s.outcome == "mismatch").count();
    out.push(check(
        "no_mismatch_outcomes",
        mismatches == 0,
        format!("{mismatches} mismatch samples"),
    ));
    let clone_checks = samples
        .iter()
        .filter(|s| s.op == "clone_state_check")
        .count();
    let layer_count = samples
        .iter()
        .filter(|s| s.op == "layer_count_check" && s.outcome == "ok")
        .count();
    out.push(check(
        "clone_heads_and_layer_count_checked",
        clone_checks as u64 == run["params"]["clones"].as_u64().unwrap_or(u64::MAX)
            && layer_count == 1,
        format!("{clone_checks} clone checks, {layer_count} layer count checks"),
    ));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::write_records;

    fn copied_run(name: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("evidence/macos-4149da9")
            .join(name);
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), tmp.path().join(entry.file_name())).unwrap();
        }
        assert_valid(tmp.path());
        tmp
    }

    fn save(dir: &Path, records: &[Record]) {
        write_records(&dir.join("samples.jsonl"), records).unwrap();
        std::fs::write(
            dir.join("summary.json"),
            serde_json::to_vec(&summarize(records)).unwrap(),
        )
        .unwrap();
    }

    fn assert_valid(dir: &Path) {
        let checks = verify(dir, true).unwrap();
        let failures: Vec<_> = checks
            .iter()
            .filter(|c| !c.ok)
            .map(|c| (c.name, &c.detail))
            .collect();
        assert!(failures.is_empty(), "{failures:?}");
    }

    fn assert_rejected(dir: &Path, name: &str) {
        let checks = verify(dir, true).unwrap();
        assert!(
            checks
                .iter()
                .any(|c| c.name == "summary_matches_raw" && c.ok)
        );
        assert!(
            checks.iter().any(|c| c.name == name && !c.ok),
            "missing failure: {name}"
        );
    }

    #[test]
    fn retained_busy_setup_chains_reject_erased_failures_gaps_and_wrong_reopens() {
        for tamper in [
            "erase",
            "attempt",
            "worker",
            "failed_reopen",
            "order",
            "input",
        ] {
            let tmp = copied_run("local-run-1");
            let mut records = read_records(&tmp.path().join("samples.jsonl")).unwrap();
            if tamper == "erase" {
                records.retain(|r| !matches!(r, Record::Sample(s) if s.outcome == "error"));
            } else {
                for r in &mut records {
                    if let Record::Sample(s) = r {
                        if s.op == "reopen" {
                            match tamper {
                                "worker" => {
                                    s.process = Some(999);
                                    s.pid = 1;
                                }
                                "failed_reopen" => {
                                    s.outcome = "error".into();
                                    s.error_code = Some("storage_error".into());
                                }
                                "order" => s.start_unix_ns = Some(0),
                                _ => {}
                            }
                        }
                        if s.attempt > 0
                            && matches!(s.op.as_str(), "prep_put_blob" | "prep_put_manifest")
                        {
                            match tamper {
                                "attempt" => s.attempt = 99,
                                "input" => s.content = Some("changed input".into()),
                                _ => {}
                            }
                        }
                    }
                }
            }
            save(tmp.path(), &records);
            assert_rejected(
                tmp.path(),
                if ["worker", "failed_reopen", "order"].contains(&tamper) {
                    "every_retry_followed_a_recorded_reopen"
                } else {
                    "retries_repeat_exact_input_after_uncertain_errors"
                },
            );
        }
    }

    #[test]
    fn every_worker_action_requires_its_own_ordered_retry_chain_and_final_input() {
        for op in [
            "prep_put_blob",
            "prep_put_manifest",
            "read_head",
            "compare_and_advance",
            "put_blob",
        ] {
            let tmp = copied_run("ci-run-1");
            let mut records = read_records(&tmp.path().join("samples.jsonl")).unwrap();
            let i = records
                .iter()
                .position(
                    |r| matches!(r, Record::Sample(s) if s.scenario == "contention" && s.op == op),
                )
                .unwrap();
            let Record::Sample(original) = records[i].clone() else {
                unreachable!()
            };
            let start = original.start_unix_ns.unwrap();
            let mut failure = original.clone();
            failure.outcome = "error".into();
            failure.error_code = Some("storage_error".into());
            failure.sqlite_code = Some("DatabaseBusy/5".into());
            failure.object = None;
            failure.result = None;
            failure.ns = 0;
            failure.start_unix_ns = Some(start - 2);
            let reopen = Sample {
                scenario: "contention".into(),
                op: "reopen".into(),
                process: original.process,
                pid: original.pid,
                outcome: "ok".into(),
                start_unix_ns: Some(start - 1),
                ..Default::default()
            };
            let mut retry = original;
            retry.attempt = 1;
            records.splice(
                i..=i,
                [
                    Record::Sample(failure),
                    Record::Sample(reopen),
                    Record::Sample(retry),
                ],
            );
            save(tmp.path(), &records);
            assert_valid(tmp.path());
            // Changing a successful retry's input must fail even with a matching summary.
            let Record::Sample(s) = &mut records[i + 2] else {
                unreachable!()
            };
            s.bytes = Some(999);
            save(tmp.path(), &records);
            assert_rejected(
                tmp.path(),
                "retries_repeat_exact_input_after_uncertain_errors",
            );
            // Deleting both the failure and reopen still cannot leave attempt 1 as initial.
            records.drain(i..i + 2);
            save(tmp.path(), &records);
            assert_rejected(
                tmp.path(),
                "retries_repeat_exact_input_after_uncertain_errors",
            );
        }
        let tmp = copied_run("ci-run-1");
        let mut records = read_records(&tmp.path().join("samples.jsonl")).unwrap();
        records.retain(|r| !matches!(r, Record::Sample(s) if s.scenario == "contention" && s.op == "prep_put_manifest" && s.process == Some(0) && s.index == 0));
        save(tmp.path(), &records);
        assert_rejected(
            tmp.path(),
            "worker_actions_match_fixture_and_final_evidence",
        );
    }

    #[test]
    fn parent_resolution_requires_exhausted_chain_and_its_successful_reopen() {
        for op in ["compare_and_advance", "put_blob"] {
            let tmp = copied_run("ci-run-1");
            let mut records = read_records(&tmp.path().join("samples.jsonl")).unwrap();
            let i = records.iter().position(|r| matches!(r, Record::Sample(s) if s.scenario == "contention" && s.op == op && (op == "put_blob" || s.outcome == "accepted"))).unwrap();
            let Record::Sample(original) = records[i].clone() else {
                unreachable!()
            };
            let mut attempts = Vec::new();
            for attempt in 0..=crate::contention::MAX_RETRIES {
                let mut failure = original.clone();
                failure.attempt = attempt;
                failure.outcome = "error".into();
                failure.error_code = Some("storage_error".into());
                failure.sqlite_code = Some("DatabaseBusy/5".into());
                failure.object = None;
                failure.result = None;
                failure.ns = 0;
                failure.start_unix_ns =
                    Some(original.start_unix_ns.unwrap() - 8 + u64::from(attempt) * 2);
                let mut reopen = Sample {
                    scenario: "contention".into(),
                    op: "reopen".into(),
                    process: original.process,
                    pid: original.pid,
                    outcome: "ok".into(),
                    start_unix_ns: failure.start_unix_ns.map(|n| n + 1),
                    ..Default::default()
                };
                attempts.push(Record::Sample(failure));
                if attempt < crate::contention::MAX_RETRIES {
                    attempts.push(Record::Sample(reopen));
                } else {
                    let parent = records
                        .iter()
                        .find_map(|r| match r {
                            Record::Sample(s) if s.op == "final_history" => Some(s),
                            _ => None,
                        })
                        .unwrap();
                    let mut resolution = original.clone();
                    resolution.op = if op == "put_blob" {
                        "resolve_blob_put"
                    } else {
                        "resolve_unresolved"
                    }
                    .into();
                    resolution.attempt = attempt + 1;
                    resolution.pid = parent.pid;
                    resolution.ns = 0;
                    resolution.start_unix_ns = parent.start_unix_ns.map(|n| n - 1);
                    reopen = resolution.clone();
                    reopen.op = "resolve_reopen".into();
                    reopen.outcome = "ok".into();
                    reopen.start_unix_ns = parent.start_unix_ns.map(|n| n - 2);
                    let insert = records
                        .iter()
                        .position(|r| matches!(r, Record::Sample(s) if s.op == "final_history"))
                        .unwrap();
                    records.splice(
                        insert..insert,
                        [Record::Sample(reopen), Record::Sample(resolution)],
                    );
                }
            }
            records.splice(i..=i, attempts);
            save(tmp.path(), &records);
            assert_valid(tmp.path());
            let r = records
                .iter_mut()
                .find_map(|r| match r {
                    Record::Sample(s) if s.op == "resolve_reopen" => Some(s),
                    _ => None,
                })
                .unwrap();
            r.outcome = "error".into();
            r.error_code = Some("storage_error".into());
            save(tmp.path(), &records);
            assert_rejected(tmp.path(), "every_retry_followed_a_recorded_reopen");
        }
    }

    #[test]
    fn disk_evidence_rejects_zero_shrink_and_missing_clones_with_matching_summary() {
        for tamper in ["zero", "shrink", "missing", "duplicate"] {
            let tmp = copied_run("ci-run-1");
            let mut records = read_records(&tmp.path().join("samples.jsonl")).unwrap();
            let i = records
                .iter()
                .position(|r| matches!(r, Record::Disk(d) if d.label == "after_clone_1"))
                .unwrap();
            match tamper {
                "zero" | "shrink" => {
                    let Record::Disk(d) = &mut records[i] else {
                        unreachable!()
                    };
                    d.stored_bytes = if tamper == "zero" {
                        0
                    } else {
                        d.stored_bytes - 1
                    };
                    if tamper == "zero" {
                        d.files = 0;
                        d.physical_bytes = Some(0);
                    }
                }
                "missing" => {
                    records.remove(i);
                }
                "duplicate" => {
                    records.insert(i, records[i].clone());
                }
                _ => unreachable!(),
            }
            save(tmp.path(), &records);
            assert_rejected(tmp.path(), "clone_storage_not_payload_proportional");
        }
    }
}
