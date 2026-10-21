//! The conformance suite against the self-hosted node (`LocalNode`), which runs in CI.

mod common;

use std::fs;

use javelin::Repository;
use javelin_collaboration::RepositoryId;
use javelin_control::LocalNode;
use javelin_node_worker::Snapshot;

#[test]
fn local_node_conforms() {
    let root = tempfile::tempdir().expect("tempdir");
    let node = LocalNode::new(root.path().join("nodes"));
    let repository = RepositoryId::parse("repo_conformance").expect("repository");
    let dir = node.repository_dir(&repository);
    let snapshot = || Snapshot {
        genesis: Repository::open(&dir).ok().map(|repo| {
            let head = repo.head().expect("head");
            (head.id.to_string(), head.tree.to_string())
        }),
        permissions: fs::read(dir.join("permissions.json"))
            .ok()
            .map(|bytes| serde_json::from_slice(&bytes).expect("permissions.json")),
    };
    common::run(&node, &snapshot);
}
