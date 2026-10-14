//! Raw record schema (one JSON object per line) and the summary derived from it.
use std::{collections::BTreeMap, path::Path, time::Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Sample {
    pub scenario: String,
    pub op: String,
    pub index: u64,
    pub attempt: u32,
    pub process: Option<u32>,
    pub pid: u32,
    pub ns: u64,
    /// "ok", "accepted", "stale" or "error".
    pub outcome: String,
    pub error_code: Option<String>,
    pub error: Option<String>,
    /// SQLite primary/extended result code when the error is a storage failure.
    pub sqlite_code: Option<String>,
    pub operation: Option<String>,
    pub expected: Option<u64>,
    pub result: Option<u64>,
    pub manifest: Option<String>,
    pub object: Option<String>,
    pub content: Option<String>,
    pub bytes: Option<u64>,
    pub param: Option<u64>,
    pub start_unix_ns: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Disk {
    pub scenario: String,
    pub label: String,
    pub physical_bytes: Option<u64>,
    /// Sum of file lengths under `.javelin` (stored bytes, not payload).
    pub stored_bytes: u64,
    pub files: u64,
    pub payload_bytes: Option<u64>,
    pub objects: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Memory {
    pub scenario: String,
    pub process: Option<u32>,
    pub pid: u32,
    pub peak_rss_bytes: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Scenario {
    pub name: String,
    pub wall_ns: u64,
    pub exit_code: Option<i32>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum Record {
    Sample(Sample),
    Disk(Disk),
    Memory(Memory),
    Scenario(Scenario),
    Note { scenario: String, message: String },
}

pub fn read_records(path: &Path) -> Result<Vec<Record>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    text.lines()
        .enumerate()
        .map(|(n, line)| {
            serde_json::from_str(line).map_err(|e| format!("{}:{}: {e}", path.display(), n + 1))
        })
        .collect()
}

pub fn write_records(path: &Path, records: &[Record]) -> std::io::Result<()> {
    let mut text = String::new();
    for record in records {
        text.push_str(&serde_json::to_string(record).expect("record serializes"));
        text.push('\n');
    }
    std::fs::write(path, text)
}

pub fn timed<T>(f: impl FnOnce() -> javelin::Result<T>) -> (u64, javelin::Result<T>) {
    let start = Instant::now();
    let result = f();
    (start.elapsed().as_nanos() as u64, result)
}

pub fn unix_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Nearest-rank percentile over sorted values: rank = ceil(p/100 * n).
pub fn percentile(sorted: &[u64], p: u64) -> u64 {
    let rank = (p * sorted.len() as u64).div_ceil(100).max(1);
    sorted[rank as usize - 1]
}

#[derive(Default)]
struct Acc {
    points: Vec<(u64, u64)>,
    params: std::collections::BTreeSet<u64>,
    expected: std::collections::BTreeSet<u64>,
}

fn group_json(scenario: &str, op: &str, outcome: &str, acc: &mut Acc) -> Value {
    acc.points.sort_by_key(|p| p.0);
    let ordered: Vec<u64> = acc.points.iter().map(|p| p.1).collect();
    let mut ns = ordered.clone();
    ns.sort_unstable();
    let n = ns.len() as u64;
    let sum: u64 = ns.iter().sum();
    let state_points = acc.params.len().max(acc.expected.len());
    let quarter = ordered.len() / 4;
    let trend = (ordered.len() >= 8).then(|| {
        let mut first = ordered[..quarter].to_vec();
        let mut last = ordered[ordered.len() - quarter..].to_vec();
        first.sort_unstable();
        last.sort_unstable();
        json!({"first_quarter_p50_ns": percentile(&first, 50), "last_quarter_p50_ns": percentile(&last, 50)})
    });
    json!({
        "scenario": scenario, "op": op, "outcome": outcome, "n": n,
        "min_ns": ns[0], "p50_ns": percentile(&ns, 50), "p95_ns": percentile(&ns, 95),
        "max_ns": ns[ns.len() - 1], "mean_ns": sum / n, "sum_ns": sum,
        "p95_reliable": n >= 20,
        "distinct_state_points": state_points,
        "pooled_across_changing_state": state_points > 1,
        "time_ordered_trend": trend,
    })
}

/// Deterministic aggregate of the raw records. `verify` recomputes it and compares.
/// Pooled percentiles are flagged when the repository state changed within a group;
/// the raw `param`/`expected` fields are the state series to read instead.
pub fn summarize(records: &[Record]) -> Value {
    let mut groups: BTreeMap<(String, String, String), Acc> = BTreeMap::new();
    let mut chains: BTreeMap<(String, String), (u32, u64, String)> = BTreeMap::new();
    let mut errors: BTreeMap<String, u64> = BTreeMap::new();
    let (mut disk, mut memory, mut scenarios, mut notes) = (vec![], vec![], vec![], vec![]);
    let mut truncated = std::collections::BTreeSet::new();
    for (seq, record) in records.iter().enumerate() {
        match record {
            Record::Sample(s) => {
                let acc = groups
                    .entry((s.scenario.clone(), s.op.clone(), s.outcome.clone()))
                    .or_default();
                acc.points
                    .push((s.start_unix_ns.unwrap_or(seq as u64), s.ns));
                acc.params.extend(s.param);
                acc.expected.extend(s.expected);
                if s.outcome == "error" {
                    let key = format!(
                        "{}/{}: {}",
                        s.scenario,
                        s.op,
                        s.sqlite_code
                            .as_deref()
                            .or(s.error_code.as_deref())
                            .unwrap_or("unknown")
                    );
                    *errors.entry(key).or_default() += 1;
                }
                if s.scenario == "contention"
                    && matches!(s.op.as_str(), "compare_and_advance" | "resolve_unresolved")
                {
                    let chain = chains
                        .entry((s.scenario.clone(), s.operation.clone().unwrap_or_default()))
                        .or_insert((0, 0, String::new()));
                    chain.1 += s.ns;
                    if chain.2.is_empty() || s.attempt >= chain.0 {
                        chain.0 = s.attempt;
                        chain.2 = s.outcome.clone();
                    }
                }
            }
            Record::Disk(d) => disk.push(json!(d)),
            Record::Memory(m) => memory.push(json!(m)),
            Record::Scenario(s) => scenarios.push(json!(s)),
            Record::Note { scenario, message } => {
                if message.contains("budget exhausted") {
                    truncated.insert(scenario.clone());
                }
                notes.push(json!({"scenario": scenario, "message": message}));
            }
        }
    }
    for ((scenario, _), (_, sum, outcome)) in chains {
        let acc = groups
            .entry((scenario, "compare_and_advance_end_to_end".into(), outcome))
            .or_default();
        acc.points.push((acc.points.len() as u64, sum));
    }
    let groups: Vec<Value> = groups
        .into_iter()
        .map(|((scenario, op, outcome), mut acc)| group_json(&scenario, &op, &outcome, &mut acc))
        .collect();
    let starts: Vec<u64> = records
        .iter()
        .filter_map(|r| match r {
            Record::Sample(s) if s.op == "barrier_release" => s.start_unix_ns,
            _ => None,
        })
        .collect();
    let late: Vec<u64> = records
        .iter()
        .filter_map(|r| match r {
            Record::Sample(s) if s.op == "barrier_release" => {
                Some(s.start_unix_ns?.saturating_sub(s.param?))
            }
            _ => None,
        })
        .collect();
    let barrier = json!({
        "workers": starts.len(),
        "release_skew_ns": starts.iter().max().zip(starts.iter().min()).map(|(a, b)| a - b),
        "max_lateness_after_target_ns": late.iter().max(),
    });
    json!({
        "barrier": barrier,
        "percentile_method": "nearest-rank ceil(p/100*n) over raw nanosecond samples; p95 equals the maximum when n < 20 (p95_reliable=false); groups with pooled_across_changing_state=true mix different repository states, so read the raw param/expected series and time_ordered_trend instead of pooled percentiles",
        "groups": groups,
        "error_counts": errors,
        "truncated_scenarios": truncated,
        "disk": disk,
        "memory": memory,
        "scenarios": scenarios,
        "notes": notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles() {
        let v: Vec<u64> = (1..=10).collect();
        assert_eq!(percentile(&v, 50), 5);
        assert_eq!(percentile(&v, 95), 10);
        assert_eq!(percentile(&[7], 50), 7);
        let hundred: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&hundred, 95), 95);
        assert_eq!(percentile(&hundred, 50), 50);
    }

    #[test]
    fn summary_flags_small_p95_and_counts_errors() {
        let mk = |ns, outcome: &str| {
            Record::Sample(Sample {
                scenario: "s".into(),
                op: "o".into(),
                ns,
                outcome: outcome.into(),
                error_code: (outcome == "error").then(|| "storage_error".into()),
                ..Default::default()
            })
        };
        let summary = summarize(&[mk(5, "ok"), mk(1, "ok"), mk(9, "error")]);
        let groups = summary["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[1]["outcome"], "ok");
        assert_eq!(groups[1]["p95_ns"], 5);
        assert_eq!(groups[1]["p95_reliable"], false);
        assert_eq!(summary["error_counts"]["s/o: storage_error"], 1);
    }
}
