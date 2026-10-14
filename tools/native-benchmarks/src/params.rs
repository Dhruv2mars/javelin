//! Bounded profiles and the hard caps that keep every run small and safe.
use serde::{Deserialize, Serialize};

use crate::fixture::Fixture;

pub const MAX_FILES: usize = 5000;
pub const MAX_HISTORY: usize = 200;
pub const MAX_LAYER_FILES: usize = 2000;
pub const MAX_CLONES: usize = 64;
pub const MAX_PROCS: usize = 8;
pub const MAX_OPS: usize = 200;
pub const MAX_BLOBS: usize = 200;
pub const MAX_BIG_BLOB: usize = 16 * 1024 * 1024;
pub const MAX_INGEST_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_MANIFEST_ENTRIES: usize = 200_000;
pub const MAX_LAYER_CHANGES_TOTAL: usize = 4000;
pub const MAX_BUDGET_SECS: u64 = 3600;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Params {
    pub profile: String,
    pub seed: u64,
    pub files: usize,
    pub binary_max: usize,
    pub history: usize,
    pub changed_per_version: usize,
    pub layer_files: usize,
    pub layer_rounds: usize,
    pub layer_changed: usize,
    pub clones: usize,
    pub big_blob_bytes: usize,
    pub procs: usize,
    pub ops: usize,
    pub blobs: usize,
    pub prefill_versions: usize,
    pub prefill_files: usize,
    pub budget_secs: u64,
}

impl Params {
    pub fn profile(name: &str, seed: u64) -> Result<Self, String> {
        let base = Self {
            profile: name.into(),
            seed,
            files: 20,
            binary_max: 8192,
            history: 4,
            changed_per_version: 3,
            layer_files: 12,
            layer_rounds: 2,
            layer_changed: 3,
            clones: 3,
            big_blob_bytes: 4096,
            procs: 3,
            ops: 4,
            blobs: 4,
            prefill_versions: 0,
            prefill_files: 0,
            budget_secs: 60,
        };
        Ok(match name {
            "smoke" => base,
            "ci" => Self {
                files: 300,
                binary_max: 262_144,
                history: 30,
                changed_per_version: 10,
                layer_files: 200,
                layer_rounds: 6,
                layer_changed: 10,
                clones: 24,
                big_blob_bytes: 1 << 20,
                procs: 4,
                ops: 25,
                blobs: 20,
                prefill_versions: 10,
                prefill_files: 100,
                budget_secs: 600,
                ..base
            },
            "local" => Self {
                files: 2000,
                binary_max: 262_144,
                history: 100,
                changed_per_version: 20,
                layer_files: 500,
                layer_rounds: 8,
                layer_changed: 25,
                clones: 24,
                big_blob_bytes: 1 << 20,
                procs: 8,
                ops: 50,
                blobs: 40,
                prefill_versions: 30,
                prefill_files: 200,
                budget_secs: 1200,
                ..base
            },
            // Heavier contention to try to reach the 5 s busy timeout; not run in CI.
            "stress" => Self {
                files: 300,
                binary_max: 262_144,
                history: 30,
                changed_per_version: 10,
                layer_files: 200,
                layer_rounds: 6,
                layer_changed: 10,
                clones: 24,
                big_blob_bytes: 1 << 20,
                procs: 8,
                ops: 50,
                blobs: 40,
                prefill_versions: 100,
                prefill_files: 300,
                budget_secs: 1800,
                ..base
            },
            other => {
                return Err(format!(
                    "unknown profile {other:?}; use smoke, ci, local or stress"
                ));
            }
        })
    }

    /// Rejects anything beyond the documented hard caps instead of clamping silently.
    pub fn validate(&self) -> Result<(), String> {
        let within = |name: &str, value: usize, min: usize, max: usize| {
            if (min..=max).contains(&value) {
                Ok(())
            } else {
                Err(format!("{name}={value} outside hard cap {min}..={max}"))
            }
        };
        within("files", self.files, 1, MAX_FILES)?;
        within("history", self.history, 1, MAX_HISTORY)?;
        within(
            "changed-per-version",
            self.changed_per_version,
            1,
            self.files,
        )?;
        within("layer-files", self.layer_files, 1, MAX_LAYER_FILES)?;
        within("layer-rounds", self.layer_rounds, 1, 16)?;
        within("layer-changed", self.layer_changed, 1, MAX_LAYER_FILES)?;
        within("clones", self.clones, 1, MAX_CLONES)?;
        within("big-blob-bytes", self.big_blob_bytes, 1, MAX_BIG_BLOB)?;
        within("procs", self.procs, 2, MAX_PROCS)?;
        within("ops", self.ops, 1, MAX_OPS)?;
        within("blobs", self.blobs, 1, MAX_BLOBS)?;
        within("binary-max", self.binary_max, 2, 4 << 20)?;
        within(
            "budget-secs",
            self.budget_secs as usize,
            1,
            MAX_BUDGET_SECS as usize,
        )?;
        if self.layer_changed * self.layer_rounds > self.layer_files {
            return Err("layer-changed * layer-rounds exceeds layer-files".into());
        }
        if self.layer_changed * self.layer_rounds * (self.layer_rounds + 1) / 2
            > MAX_LAYER_CHANGES_TOTAL
        {
            return Err(format!(
                "cumulative Layer changes exceed {MAX_LAYER_CHANGES_TOTAL}"
            ));
        }
        if self.files * self.history > MAX_MANIFEST_ENTRIES {
            return Err(format!(
                "files * history exceeds {MAX_MANIFEST_ENTRIES} manifest entries"
            ));
        }
        let fixture = Fixture::new(self.seed, self.binary_max);
        let bytes: u64 = (0..self.files).map(|i| fixture.size(i) as u64).sum();
        if bytes > MAX_INGEST_BYTES {
            return Err(format!(
                "fixture payload {bytes} bytes exceeds {MAX_INGEST_BYTES}"
            ));
        }
        Ok(())
    }
}
