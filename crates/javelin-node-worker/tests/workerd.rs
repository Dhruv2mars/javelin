//! The conformance suite against a Cloudflare node on `wrangler dev` (or a deployed node).
//! It needs `JAVELIN_NODE_URL` and `JAVELIN_NODE_ADMIN_TOKEN`, so it is ignored by default;
//! `tools/node-cloudflare` runs it with `bun run test`. A deployed node keeps its state, so a
//! rerun there names a fresh repository in `JAVELIN_NODE_REPOSITORY`.

mod common;

use javelin_collaboration::RepositoryId;
use javelin_control::ControlError;
use javelin_node_worker::CloudflareNode;

fn node() -> CloudflareNode {
    let url = std::env::var("JAVELIN_NODE_URL").expect("JAVELIN_NODE_URL");
    let token = std::env::var("JAVELIN_NODE_ADMIN_TOKEN").expect("JAVELIN_NODE_ADMIN_TOKEN");
    CloudflareNode::new(&url, &token).expect("node")
}

fn repository() -> RepositoryId {
    let name =
        std::env::var("JAVELIN_NODE_REPOSITORY").unwrap_or_else(|_| "repo_conformance".into());
    RepositoryId::parse(&name).expect("repository")
}

#[test]
#[ignore = "needs a running node; run through tools/node-cloudflare"]
fn workerd_node_conforms() {
    let node = node();
    let repository = repository();
    common::run(&node, &repository, &|| {
        node.snapshot(&repository).expect("state")
    });
}

#[test]
#[ignore = "needs a running node; run through tools/node-cloudflare"]
fn wrong_admin_token_is_refused() {
    let url = std::env::var("JAVELIN_NODE_URL").expect("JAVELIN_NODE_URL");
    let node = CloudflareNode::new(&url, "not-the-admin-token").expect("node");
    let repository = repository();
    let error = javelin_control::NodeLink::provision(&node, &repository)
        .expect_err("a wrong token must not provision");
    assert!(matches!(&error, ControlError::Unauthorized(_)), "{error}");
    assert!(
        node.snapshot(&repository).is_err(),
        "state reads need the admin token too"
    );
}
