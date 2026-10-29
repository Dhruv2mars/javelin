//! The conformance suite against a Cloudflare node on `wrangler dev` (or a deployed node),
//! its admin routes and its protocol version 2 endpoint.
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
fn workerd_node_serves_the_wire() {
    let node = node();
    let repository = RepositoryId::parse(&format!("{}_wire", repository().as_str())).expect("id");
    let url = std::env::var("JAVELIN_NODE_URL").expect("JAVELIN_NODE_URL");
    common::run(&node, &repository, &|| {
        node.snapshot(&repository).expect("state")
    });
    javelin_control::NodeLink::trust(&node, &repository, &javelin_identity::test_keys::keys())
        .expect("trust the test key");
    let wire = common::Wire::new(&url, &repository);
    let absent = RepositoryId::parse(&format!("{}_absent", repository.as_str())).expect("id");
    common::run_wire(&repository, &wire, &common::Wire::new(&url, &absent));
    common::run_budget(&wire, &|value| {
        node.budget(&repository, Some(value))
            .expect("set the budget");
    });
    let state = node.budget(&repository, None).expect("read the budget");
    assert_eq!(state["budget"]["storage_bytes"], 1_000_000_000u64);
    assert!(
        state["usage"]["storage_bytes"]["used"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "{state}"
    );
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

#[test]
#[ignore = "needs a running node; run through tools/node-cloudflare"]
fn workerd_node_forks_across_two_objects() {
    let node = node();
    let url = std::env::var("JAVELIN_NODE_URL").expect("JAVELIN_NODE_URL");
    let prefix = repository().as_str().to_string();
    common::run_fork(&common::ForkNode {
        node: &node,
        name: &|suffix| RepositoryId::parse(&format!("{prefix}_{suffix}")).expect("repository"),
        serve: &|repository, run| run(&common::Wire::new(&url, repository)),
        budget: &|repository, value| {
            node.budget(repository, Some(value))
                .expect("set the budget");
        },
    });
}
