//! Gate-2 fixture setup: accepts the next World with exactly the paths in a JSON spec.
//!
//! cargo run --release --example gate2_seed -- REPO SPEC.json
//!
//! Clone-backed workspaces have no Publish yet, so tools/workspace-probe/gate2.mjs seeds
//! its Worlds here, through the same library calls the workspace tests use. The spec maps
//! each path to `{"from": FILE}`, `{"text": STRING}` or `{"link": TARGET}`, with an
//! optional `"exec": true`. Inline text lets a spec name paths that the seeding volume
//! itself could not hold side by side, such as `Foo` and `foo`.
use std::{collections::BTreeMap, fs};

use javelin::{AdvanceOutcome, FileEntry, Manifest, OperationId, RepoPath, Repository};
use serde_json::{Value, json};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [repo, spec] = args.as_slice() else {
        eprintln!("usage: gate2_seed REPO SPEC.json");
        std::process::exit(2);
    };
    let spec: BTreeMap<String, Value> =
        serde_json::from_slice(&fs::read(spec).expect("read spec")).expect("parse spec");
    let mut repo = Repository::open(repo).expect("open repository");
    let mut entries = Vec::with_capacity(spec.len());
    for (path, entry) in &spec {
        let entry = if let Some(target) = entry["link"].as_str() {
            FileEntry::Symlink {
                target: repo.put_blob(target.as_bytes()).expect("store link"),
            }
        } else {
            let bytes = match (entry["from"].as_str(), entry["text"].as_str()) {
                (Some(from), None) => fs::read(from).expect("read source file"),
                (None, Some(text)) => text.as_bytes().to_vec(),
                _ => panic!("{path}: need exactly one of from, text or link"),
            };
            FileEntry::Regular {
                content: repo.put_blob(&bytes).expect("store blob"),
                executable: entry["exec"].as_bool().unwrap_or(false),
            }
        };
        entries.push((RepoPath::parse(path).expect("valid path"), entry));
    }
    let manifest = Manifest::new(entries).expect("valid manifest");
    let manifest = repo.put_manifest(&manifest).expect("store manifest");
    let head = repo.history().expect("history").last().expect("World 0").number;
    let operation = OperationId::parse(&format!("gate2-seed-{}", head.get() + 1)).unwrap();
    match repo.compare_and_advance(&operation, head, manifest).expect("advance") {
        AdvanceOutcome::Accepted { version } => {
            println!("{}", json!({"ok": true, "version": version.number.get()}))
        }
        other => panic!("World did not advance: {other:?}"),
    }
}
