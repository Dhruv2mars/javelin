//! Members over the wire, with the real binaries and real tokens: `javelin control serve`,
//! `javelin-node serve`, and people signed in through the control plane. A role change the
//! owner makes with `javelin member` decides the collaborator's next request while the node
//! keeps serving, and work a removed member queued while the node was down is refused when it
//! replays, without changing anything on the node.

use std::{fs, path::Path};

mod daily_support;

use daily_support::{Person, Site, ok, owner_site, refused, step};

fn write(dir: &Path, path: &str, text: &str) {
    let path = dir.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Opens request `id` from a new Layer of the same name holding `files`.
fn open_request(person: &Person, clone: &Path, id: &str, files: &[(&str, &str)]) {
    let layer = person.layer(clone, id);
    for (path, text) in files {
        write(&layer, path, text);
    }
    let opened = ok(person.at(
        clone,
        &[
            "request",
            "open",
            id,
            "--layer",
            id,
            "--operation",
            &format!("{id}-open"),
        ],
    ));
    assert_eq!(opened["revision"], 1, "{opened}");
}

fn approve(person: &Person, clone: &Path, id: &str, operation: &str) -> std::process::Output {
    person.at(
        clone,
        &[
            "request",
            "approve",
            id,
            "--revision",
            "1",
            "--operation",
            operation,
        ],
    )
}

/// The owner's repository with World 1 published, and cora invited as `role`, signed in on
/// her laptop with a clone of her own.
fn with_cora(role: &str) -> (Site, Person, std::path::PathBuf, Person, std::path::PathBuf) {
    let (site, owner) = owner_site();
    let ours = owner.attach(&site, "owner");
    open_request(&owner, &ours, "import", &[("README.md", "# app\n")]);
    ok(owner.at(
        &ours,
        &[
            "request",
            "publish",
            "import",
            "--revision",
            "1",
            "--operation",
            "import-publish",
        ],
    ));
    let code = owner.invite("acme/app", "cora", role);
    let cora = site.person("cora-home");
    cora.accept(&code);
    let theirs = cora.attach(&site, "cora");
    (site, owner, ours, cora, theirs)
}

#[test]
fn a_role_change_decides_the_next_request_on_a_serving_node() {
    step("owner invites cora as a reviewer; she approves the owner's request");
    let (site, owner, ours, cora, theirs) = with_cora("review");
    open_request(&owner, &ours, "docs", &[("docs/a.md", "a\n")]);
    open_request(&owner, &ours, "more", &[("docs/b.md", "b\n")]);
    ok(approve(&cora, &theirs, "docs", "cora-approve-docs"));

    step("cora cannot change members; the owner lowers her to read with the node serving");
    assert_eq!(
        refused(cora.run(&["member", "role", "acme/app", "cora", "maintain"])),
        "forbidden"
    );
    let lowered = ok(owner.run(&["member", "role", "acme/app", "cora", "read"]));
    assert_eq!(lowered["changed"], true, "{lowered}");
    // Her cached token is still valid; the node decides with the new role.
    assert_eq!(
        refused(approve(&cora, &theirs, "more", "cora-approve-more")),
        "forbidden"
    );
    let listed = ok(cora.at(&theirs, &["request", "list"]));
    assert_eq!(listed["requests"].as_array().unwrap().len(), 3, "{listed}");

    step("raised back to review, she approves on the next request");
    ok(owner.run(&["member", "role", "acme/app", "cora", "review"]));
    ok(approve(&cora, &theirs, "more", "cora-approve-more"));

    step("removed, she reads nothing; the repository keeps its maintainer");
    let removed = ok(owner.run(&["member", "remove", "acme/app", "cora"]));
    assert_eq!(removed["members"].as_array().unwrap().len(), 1, "{removed}");
    assert_eq!(refused(cora.at(&theirs, &["request", "list"])), "forbidden");
    assert_eq!(
        refused(cora.run(&["member", "list", "acme/app"])),
        "forbidden"
    );
    assert_eq!(
        refused(owner.run(&["member", "remove", "acme/app", "admin"])),
        "invalid_input"
    );
    site.assert_no_git();
}

#[test]
fn queued_work_of_a_removed_member_is_refused_when_it_replays() {
    let (mut site, owner, ours, cora, theirs) = with_cora("review");
    open_request(&cora, &theirs, "feature", &[("src/lib.rs", "v1\n")]);

    step("the node is down: cora's revision queues");
    site.stop_node();
    let layer = theirs.join(".javelin/layers/feature");
    write(&layer, "src/lib.rs", "v2\n");
    let queued = ok(cora.at(
        &theirs,
        &[
            "request",
            "revise",
            "feature",
            "--revision",
            "1",
            "--operation",
            "feature-revise",
        ],
    ));
    assert_eq!(queued["queued"], true, "{queued}");

    step("the owner removes cora while the node is down; the node starts again");
    ok(owner.run(&["member", "remove", "acme/app", "cora"]));
    site.start_node();

    step("the replay is refused, stays queued, and the node is unchanged");
    // The replay goes first and is refused; the fetch after it is refused the same way.
    assert_eq!(refused(cora.at(&theirs, &["sync"])), "forbidden");
    let replay = cora.at(&theirs, &["outbox", "replay", "--remote", &site.repository]);
    assert_eq!(replay.status.code(), Some(1), "{replay:?}");
    let replay: serde_json::Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(replay["stalled"]["code"], "forbidden", "{replay}");
    assert_eq!(replay["delivered"], serde_json::json!([]), "{replay}");
    // A queued revision is its uploads, the Layer append and the revise; all stay queued.
    let pending = replay["pending"].as_u64().unwrap();
    assert!(pending >= 1, "{replay}");
    let status = ok(cora.at(&theirs, &["outbox", "status", "--remote", &site.repository]));
    assert_eq!(
        status["pending"].as_array().map(|p| p.len() as u64),
        Some(pending),
        "{status}"
    );
    let shown = ok(owner.at(&ours, &["request", "show", "feature"]));
    assert_eq!(
        shown["revisions"].as_array().unwrap().len(),
        1,
        "the queued revision never reached the node: {shown}"
    );
    site.assert_no_git();
}
