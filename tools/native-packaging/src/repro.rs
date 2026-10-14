//! Reproducibility attempt: build the same source from two different directories.
use std::{fs, path::Path};

use serde::Serialize;

use crate::{Result, build::build, read, util::list_files};

#[derive(Serialize)]
pub struct Side {
    pub source_dir: String,
    pub binary_sha256: String,
    pub size: u64,
}

#[derive(Serialize)]
pub struct ReproReport {
    pub source_date_epoch: u64,
    pub a: Side,
    pub b: Side,
    pub identical: bool,
    pub first_difference_offset: Option<u64>,
    pub differing_bytes: Option<u64>,
    pub note: String,
}

fn copy_source(source: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest.join("src"))?;
    for file in ["Cargo.toml", "Cargo.lock"] {
        fs::copy(source.join(file), dest.join(file))?;
    }
    for file in list_files(&source.join("src"))? {
        let target = dest.join("src").join(&file);
        fs::create_dir_all(target.parent().unwrap())?;
        fs::copy(source.join("src").join(&file), target)?;
    }
    Ok(())
}

pub fn repro(source: &Path, work: &Path, epoch: u64) -> Result<ReproReport> {
    fs::create_dir_all(work)?;
    let work = fs::canonicalize(work)?;
    let mut sides = Vec::new();
    for (label, dir) in [
        ("a", "alpha/javelin-tree"),
        ("b", "beta-longer-name/another-javelin-tree"),
    ] {
        let tree = work.join(dir);
        copy_source(source, &tree)?;
        let out = work.join(format!("out-{label}"));
        let record = build(&tree, &out, epoch, &[])?;
        let bytes = read(&out.join(&record.binary))?;
        sides.push((tree, record.binary_sha256, bytes));
    }
    let (b, a) = (sides.pop().unwrap(), sides.pop().unwrap());
    let identical = a.2 == b.2;
    let (first, differing) = if identical || a.2.len() != b.2.len() {
        (None, None)
    } else {
        let diffs: Vec<_> =
            a.2.iter()
                .zip(&b.2)
                .enumerate()
                .filter(|(_, (x, y))| x != y)
                .collect();
        (
            diffs.first().map(|(i, _)| *i as u64),
            Some(diffs.len() as u64),
        )
    };
    let note = if identical {
        "Two builds from different directories produced identical binary bytes on this host; this is not proof for other hosts or toolchains.".into()
    } else {
        "Binaries differ despite remap-path-prefix, SOURCE_DATE_EPOCH, and a controlled environment; the binary is not reproducible here.".into()
    };
    let side = |t: &(std::path::PathBuf, String, Vec<u8>)| Side {
        source_dir: t.0.display().to_string(),
        binary_sha256: t.1.clone(),
        size: t.2.len() as u64,
    };
    Ok(ReproReport {
        source_date_epoch: epoch,
        a: side(&a),
        b: side(&b),
        identical,
        first_difference_offset: first,
        differing_bytes: differing,
        note,
    })
}
