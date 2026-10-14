//! Per-scenario context: where records accumulate and when the budget expires.
use std::path::PathBuf;

use crate::{
    fixture::Fixture,
    host::{disk_usage, peak_rss_bytes},
    params::Params,
    record::{Disk, Memory, Record, Sample, unix_ns},
};

pub type R<T> = Result<T, Box<dyn std::error::Error>>;

pub struct Ctx {
    pub name: String,
    pub params: Params,
    pub out: PathBuf,
    pub dir: PathBuf,
    pub deadline_unix_ns: u64,
    pub records: Vec<Record>,
}

impl Ctx {
    pub fn fixture(&self) -> Fixture {
        Fixture::new(self.params.seed, self.params.binary_max)
    }

    pub fn expired(&self) -> bool {
        unix_ns() >= self.deadline_unix_ns
    }

    pub fn repo_dir(&self) -> R<PathBuf> {
        std::fs::create_dir_all(&self.dir)?;
        Ok(self.dir.clone())
    }

    pub fn note(&mut self, message: &str) {
        self.records.push(Record::Note {
            scenario: self.name.clone(),
            message: message.into(),
        });
    }

    /// A sample whose outcome reflects the API result; callers add operation details.
    pub fn sample<T>(&self, op: &str, index: u64, ns: u64, result: &javelin::Result<T>) -> Sample {
        let sqlite_code = match result {
            Err(javelin::Error::Storage(rusqlite::Error::SqliteFailure(f, _))) => {
                Some(format!("{:?}/{}", f.code, f.extended_code))
            }
            _ => None,
        };
        let (outcome, error_code, error) = match result {
            Ok(_) => ("ok".to_string(), None, None),
            Err(e) => (
                "error".to_string(),
                Some(e.code().to_string()),
                Some(e.to_string()),
            ),
        };
        Sample {
            scenario: self.name.clone(),
            op: op.into(),
            index,
            pid: std::process::id(),
            ns,
            start_unix_ns: Some(crate::record::unix_ns().saturating_sub(ns)),
            outcome,
            error_code,
            error,
            sqlite_code,
            ..Default::default()
        }
    }

    pub fn push(&mut self, sample: Sample) {
        self.records.push(Record::Sample(sample));
    }

    pub fn disk(&mut self, label: &str, payload: Option<u64>, objects: Option<u64>) {
        let usage = disk_usage(&self.dir.join(".javelin"));
        self.records.push(Record::Disk(Disk {
            scenario: self.name.clone(),
            label: label.into(),
            physical_bytes: usage.physical,
            stored_bytes: usage.logical,
            files: usage.files,
            payload_bytes: payload,
            objects,
        }));
    }

    pub fn memory(&mut self, process: Option<u32>) {
        self.records.push(Record::Memory(Memory {
            scenario: self.name.clone(),
            process,
            pid: std::process::id(),
            peak_rss_bytes: peak_rss_bytes(),
        }));
    }
}
