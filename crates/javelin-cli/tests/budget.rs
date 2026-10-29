//! A node's budget with the real binary against the node's server on loopback: a push that
//! would cross it is refused, and the CLI says which budget ran out and what the owner can do;
//! `javelin node usage` shows a maintainer the use against each budget.

use std::{fs, process::Output};

use javelin_collaboration::Role;
use javelin_identity::Scope;
use javelin_node::budget::Budget;

mod wire_support;

use wire_support::{Deployment, stdout};

fn describe(output: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn a_push_past_the_budget_says_what_ran_out_and_usage_shows_where_the_node_stands() {
    // The node's owner left a few KB of storage: less than any write needs.
    let deployment = Deployment::start_with(&[("rita", Role::Read)], 0, |log| {
        log.set_budget(Budget {
            storage_bytes: 4096,
            ..Budget::default()
        });
        Vec::new()
    });
    let dir = deployment.temp.path().join("author");
    fs::create_dir_all(&dir).unwrap();
    let url = deployment.attach_url();
    let attached = deployment.javelin(
        &deployment.control_url,
        &["attach", &url, dir.to_str().unwrap()],
    );
    assert_eq!(attached.status.code(), Some(0), "{}", describe(&attached));
    let repo = dir.to_str().unwrap();
    let created = deployment.javelin(
        &deployment.control_url,
        &["workspace", "create", "work", "--repo", repo],
    );
    assert_eq!(created.status.code(), Some(0), "{}", describe(&created));
    fs::write(dir.join(".javelin/layers/work/a.txt"), "1\n").unwrap();

    let push = [
        "request",
        "open",
        "pr-1",
        "--layer",
        "work",
        "--operation",
        "open-1",
        "--repo",
        repo,
    ];
    let refused = deployment.javelin(&deployment.control_url, &push);
    assert_eq!(refused.status.code(), Some(1), "{}", describe(&refused));
    let error: serde_json::Value = serde_json::from_slice(&refused.stderr).unwrap();
    assert_eq!(error["error"]["code"], "quota_exceeded", "{error}");
    let message = error["error"]["message"].as_str().unwrap();
    // The device holds Maintain, so it sees the numbers; then what the owner can do.
    assert!(message.contains("storage_bytes: "), "{message}");
    assert!(message.contains("4096 bytes stored"), "{message}");
    assert!(message.contains("Nothing was stored"), "{message}");
    assert!(message.contains("raises storage_bytes"), "{message}");
    assert!(message.contains("budget.json"), "{message}");
    assert_eq!(
        deployment.node_head(),
        0,
        "the refused push published nothing"
    );

    // Usage, for a maintainer, and refused to a member.
    let node = deployment.node_url.as_str();
    let id = deployment.repository.as_str();
    let usage = ["node", "usage", "--node", node, "--repository", id];
    let shown = deployment.javelin(&deployment.control_url, &usage);
    assert_eq!(shown.status.code(), Some(0), "{}", describe(&shown));
    let shown = stdout(&shown);
    assert_eq!(shown["budgets"]["storage_bytes"]["limit"], 4096, "{shown}");
    assert!(
        shown["budgets"]["storage_bytes"]["used"].as_u64().unwrap() > 4096,
        "{shown}"
    );
    assert_eq!(shown["budgets"]["rows_written_per_day"]["limit"], 100_000);
    let rita = deployment.token("rita", &[Scope::Read, Scope::LayerWrite]);
    let member = deployment.javelin_as(&rita, &usage);
    assert_eq!(member.status.code(), Some(1), "{}", describe(&member));
    assert!(
        String::from_utf8_lossy(&member.stderr).contains("forbidden"),
        "{}",
        describe(&member)
    );
}
