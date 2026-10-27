//! The conformance suite against the self-hosted node: `LocalNode` for the control plane's
//! pushes, and `javelin_node::serve` on loopback for protocol version 2.

mod common;

use std::{fs, thread};

use javelin::Repository;
use javelin_collaboration::RepositoryId;
use javelin_control::{LocalNode, NodeLink};
use javelin_identity::test_keys;
use javelin_node::{FileWorldLog, auth::Gate, bind_loopback, serve};
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
    common::run(&node, &repository, &snapshot);

    // A serving node reads its keys when it starts, so they are installed first.
    node.trust(&repository, &test_keys::keys()).expect("trust");
    let listener = bind_loopback("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let mut log = FileWorldLog::open(&dir).expect("node");
    let gate = Gate::load(&dir, repository.as_str()).expect("gate");
    let name = repository.as_str().to_string();
    thread::spawn(move || serve(&listener, &name, &mut log, &gate, None));
    let wire = common::Wire::new(&base, &repository);
    let absent = common::Wire::new(&base, &RepositoryId::parse("repo_absent").expect("id"));
    common::run_wire(&node, &repository, &wire, &absent);
}
