//! The Publish Request loop and `javelin sync`, end to end with the real binary against the
//! node's server on loopback, with real tokens: the author is a device signed in through the
//! control plane, and the reviewer and maintainer hold tokens signed with a key the node trusts.
//! Exact-revision rules hold over the wire, revise and publish queue during a node outage while
//! open, approve and comment refuse (D7), and sync advances a second clone from its verified
//! head, refusing a local World that moved and a chain that does not pass through the anchor.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Output,
};

use javelin::Repository;
use javelin_collaboration::Role;
use javelin_identity::Scope;
use serde_json::Value;

mod wire_support;

use wire_support::{Deployment, closed_url, error_code, stdout};

const REVIEWER: &[Scope] = &[Scope::Read, Scope::ReviewWrite, Scope::ReviewApprove];
const MAINTAINER: &[Scope] = &[
    Scope::Read,
    Scope::LayerWrite,
    Scope::Publish,
    Scope::ReviewWrite,
    Scope::ReviewApprove,
];

fn describe(output: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The JSON of a command that must succeed.
fn ok(output: Output) -> Value {
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    stdout(&output)
}

/// The error code of a command that must fail with exit 1.
fn refused(output: Output) -> String {
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert!(output.stdout.is_empty(), "{}", describe(&output));
    error_code(&output)
}

struct Team {
    deployment: Deployment,
    rita: String,
    mona: String,
}

impl Team {
    /// `rita` reviews and `mona` maintains, beside the signed-in author.
    fn start(required_approvals: u32) -> Self {
        let deployment = Deployment::start_with(
            &[("rita", Role::Review), ("mona", Role::Maintain)],
            required_approvals,
            |_| Vec::new(),
        );
        let rita = deployment.token("rita", REVIEWER);
        let mona = deployment.token("mona", MAINTAINER);
        Self {
            deployment,
            rita,
            mona,
        }
    }

    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.deployment.temp.path().join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Runs the CLI as the author, the device signed in through the control plane.
    fn author(&self, args: &[&str]) -> Output {
        self.deployment.javelin(&self.deployment.control_url, args)
    }

    fn as_token(&self, token: &str, args: &[&str]) -> Output {
        self.deployment.javelin_as(token, args)
    }

    /// Attaches `name` as the author, or as the holder of `token`.
    fn attach(&self, name: &str, token: Option<&str>) -> PathBuf {
        let dir = self.dir(name);
        let url = self.deployment.attach_url();
        let args = ["attach", url.as_str(), dir.to_str().unwrap()];
        match token {
            Some(token) => ok(self.as_token(token, &args)),
            None => ok(self.author(&args)),
        };
        dir
    }
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// Points the clone at `node`: a closed URL is an outage, as the CLI sees one.
fn set_node(dir: &Path, node: &str) {
    let path = dir.join(".javelin").join("attach.json");
    let mut attachment: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    attachment["node"] = node.into();
    fs::write(&path, serde_json::to_vec(&attachment).unwrap()).unwrap();
}

fn attachment(dir: &Path) -> Value {
    serde_json::from_slice(&fs::read(dir.join(".javelin").join("attach.json")).unwrap()).unwrap()
}

fn pending(dir: &Path, repository: &str) -> usize {
    javelin::outbox::Outbox::open(dir, repository)
        .unwrap()
        .pending()
        .count()
}

#[test]
fn the_request_loop_publishes_exactly_the_approved_revision_and_a_second_clone_syncs_it() {
    let team = Team::start(1);
    let author = team.attach("author", None);
    let maintainer = team.attach("maintainer", Some(&team.mona));
    let mona = |args: &[&str]| team.as_token(&team.mona, args);
    let rita = |args: &[&str]| team.as_token(&team.rita, args);

    ok(team.author(&["workspace", "create", "work", "--repo", s(&author)]));
    let work = author.join(".javelin").join("layers").join("work");
    fs::write(work.join("hello.txt"), "hello\n").unwrap();

    let opened = ok(team.author(&[
        "request",
        "open",
        "pr-1",
        "--layer",
        "work",
        "--operation",
        "open-1",
        "--repo",
        s(&author),
    ]));
    assert_eq!(opened["revision"], 1, "{opened}");
    assert_eq!(opened["base"]["number"], 0);
    // A retry with the same operation returns the original outcome.
    let again = ok(team.author(&[
        "request",
        "open",
        "pr-1",
        "--layer",
        "work",
        "--operation",
        "open-1",
        "--repo",
        s(&author),
    ]));
    assert_eq!(again["revision"], 1);
    assert_eq!(again["checkpoint"], opened["checkpoint"]);

    // The reviewer reads the request and the paths it changes.
    let shown = ok(rita(&["request", "show", "pr-1", "--repo", s(&maintainer)]));
    assert_eq!(shown["state"], "open");
    assert_eq!(shown["author"], team.deployment.principal.as_str());
    assert_eq!(shown["layer"], "work");
    assert_eq!(shown["changes"]["paths"][0]["path"], "hello.txt");
    assert_eq!(shown["revisions"][0]["checkpoint"], opened["checkpoint"]);

    // The author cannot approve their own request.
    let code = refused(team.author(&[
        "request",
        "approve",
        "pr-1",
        "--revision",
        "1",
        "--operation",
        "self-1",
        "--repo",
        s(&author),
    ]));
    assert_eq!(code, "self_approval");

    ok(rita(&[
        "request",
        "approve",
        "pr-1",
        "--revision",
        "1",
        "--operation",
        "rita-approve-1",
        "--repo",
        s(&maintainer),
    ]));
    ok(rita(&[
        "request",
        "comment",
        "pr-1",
        "--revision",
        "1",
        "--body",
        "say hello louder",
        "--operation",
        "rita-comment-1",
        "--repo",
        s(&maintainer),
    ]));

    // Only the author revises: the maintainer's own Layer cannot become a revision.
    ok(mona(&[
        "workspace",
        "create",
        "takeover",
        "--repo",
        s(&maintainer),
    ]));
    fs::write(
        maintainer.join(".javelin/layers/takeover/hello.txt"),
        "mine\n",
    )
    .unwrap();
    let code = refused(mona(&[
        "request",
        "revise",
        "pr-1",
        "--revision",
        "1",
        "--layer",
        "takeover",
        "--operation",
        "mona-revise-1",
        "--repo",
        s(&maintainer),
    ]));
    assert_eq!(code, "not_author");

    fs::write(work.join("hello.txt"), "HELLO\n").unwrap();
    let revised = ok(team.author(&[
        "request",
        "revise",
        "pr-1",
        "--revision",
        "1",
        "--operation",
        "revise-1",
        "--repo",
        s(&author),
    ]));
    assert_eq!(revised["revision"], 2, "{revised}");
    assert_eq!(revised["queued"], false);

    // Approval of revision 1 does not count for revision 2, and revision 1 is stale.
    let code = refused(mona(&[
        "request",
        "publish",
        "pr-1",
        "--revision",
        "2",
        "--operation",
        "publish-early",
        "--repo",
        s(&maintainer),
    ]));
    assert_eq!(code, "not_enough_approvals");
    let stale = mona(&[
        "request",
        "publish",
        "pr-1",
        "--revision",
        "1",
        "--operation",
        "publish-stale",
        "--repo",
        s(&maintainer),
    ]);
    assert_eq!(stale.status.code(), Some(3), "{}", describe(&stale));
    assert_eq!(stdout(&stale)["code"], "stale_revision");
    assert_eq!(team.deployment.node_head(), 0);

    ok(rita(&[
        "request",
        "approve",
        "pr-1",
        "--revision",
        "2",
        "--operation",
        "rita-approve-2",
        "--repo",
        s(&maintainer),
    ]));
    let published = ok(mona(&[
        "request",
        "publish",
        "pr-1",
        "--revision",
        "2",
        "--operation",
        "publish-2",
        "--repo",
        s(&maintainer),
    ]));
    assert_eq!(published["published"]["number"], 1, "{published}");
    let replayed = ok(mona(&[
        "request",
        "publish",
        "pr-1",
        "--revision",
        "2",
        "--operation",
        "publish-2",
        "--repo",
        s(&maintainer),
    ]));
    assert_eq!(replayed["published"], published["published"]);
    assert_eq!(team.deployment.node_head(), 1);

    let shown = ok(rita(&["request", "show", "pr-1", "--repo", s(&maintainer)]));
    assert_eq!(shown["state"], "published");
    assert_eq!(shown["revisions"][0]["approvals"][0], "rita");
    assert_eq!(shown["revisions"][1]["approvals"][0], "rita");
    assert_eq!(shown["comments"][0]["body"], "say hello louder");
    assert_eq!(shown["comments"][0]["revision"], 1);
    let listed = ok(rita(&["request", "list", "--repo", s(&maintainer)]));
    assert_eq!(listed["requests"].as_array().unwrap().len(), 1);
    assert_eq!(listed["requests"][0]["state"], "published");

    // The second clone syncs to the published World, verified from its attached head.
    let synced = ok(mona(&["sync", "--repo", s(&maintainer)]));
    assert_eq!(synced["advanced"], true, "{synced}");
    assert_eq!(synced["from"]["number"], 0);
    assert_eq!(synced["head"]["number"], 1);
    assert_eq!(attachment(&maintainer)["head"], synced["head"]);
    let node = team.deployment.node_repo().head().unwrap();
    let local = Repository::open(&maintainer).unwrap().head().unwrap();
    assert_eq!(local.tree, node.tree);
    ok(mona(&[
        "workspace",
        "create",
        "next",
        "--repo",
        s(&maintainer),
    ]));
    assert_eq!(
        fs::read_to_string(maintainer.join(".javelin/layers/next/hello.txt")).unwrap(),
        "HELLO\n"
    );
    let current = ok(mona(&["sync", "--repo", s(&maintainer)]));
    assert_eq!(current["advanced"], false);
    assert_eq!(
        ok(team.author(&["sync", "--repo", s(&author)]))["head"]["number"],
        1
    );
}

#[test]
fn revise_and_publish_queue_through_an_outage_while_open_approve_and_comment_refuse() {
    let team = Team::start(1);
    let repository = team.deployment.repository.clone();
    let node = team.deployment.node_url.clone();
    let author = team.attach("author", None);
    let reviewer = team.attach("reviewer", Some(&team.rita));
    let maintainer = team.attach("maintainer", Some(&team.mona));
    let rita = |args: &[&str]| team.as_token(&team.rita, args);
    let mona = |args: &[&str]| team.as_token(&team.mona, args);

    ok(team.author(&["workspace", "create", "work", "--repo", s(&author)]));
    let work = author.join(".javelin/layers/work");
    fs::write(work.join("a.txt"), "one\n").unwrap();
    ok(team.author(&[
        "request",
        "open",
        "pr-1",
        "--layer",
        "work",
        "--operation",
        "open-1",
        "--repo",
        s(&author),
    ]));

    // The node is down for every clone.
    let down = closed_url();
    for dir in [&author, &reviewer, &maintainer] {
        set_node(dir, &down);
    }
    fs::write(work.join("a.txt"), "two\n").unwrap();
    fs::write(work.join("b.txt"), "new\n").unwrap();
    let queued = ok(team.author(&[
        "request",
        "revise",
        "pr-1",
        "--revision",
        "1",
        "--operation",
        "revise-1",
        "--repo",
        s(&author),
    ]));
    assert_eq!(queued["queued"], true, "{queued}");
    assert_eq!(queued["code"], "node_unavailable");
    let waiting = pending(&author, &repository);
    assert!(waiting >= 3, "uploads, append and revise wait: {waiting}");

    // Open, approve and comment need a live answer, and queue nothing.
    let code = refused(team.author(&[
        "request",
        "open",
        "pr-2",
        "--layer",
        "work",
        "--operation",
        "open-2",
        "--repo",
        s(&author),
    ]));
    assert_eq!(code, "node_unavailable");
    assert_eq!(pending(&author, &repository), waiting);
    let code = refused(rita(&[
        "request",
        "approve",
        "pr-1",
        "--revision",
        "1",
        "--operation",
        "approve-1",
        "--repo",
        s(&reviewer),
    ]));
    assert_eq!(code, "node_unavailable");
    let code = refused(rita(&[
        "request",
        "comment",
        "pr-1",
        "--body",
        "hm",
        "--operation",
        "comment-1",
        "--repo",
        s(&reviewer),
    ]));
    assert_eq!(code, "node_unavailable");
    assert_eq!(pending(&reviewer, &repository), 0);

    // The node is back: sync delivers the queued revision, which names the checkpoint the
    // node then records.
    set_node(&author, &node);
    let synced = ok(team.author(&["sync", "--repo", s(&author)]));
    assert_eq!(synced["outbox"]["pending"], 0, "{synced}");
    assert_eq!(pending(&author, &repository), 0);
    set_node(&reviewer, &node);
    let shown = ok(rita(&["request", "show", "pr-1", "--repo", s(&reviewer)]));
    assert_eq!(shown["revision"], 2, "{shown}");
    let paths: Vec<_> = shown["changes"]["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| path["path"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(paths, ["a.txt", "b.txt"]);
    // A retry of the delivered revision returns its outcome.
    let retried = ok(team.author(&[
        "request",
        "revise",
        "pr-1",
        "--revision",
        "1",
        "--operation",
        "revise-1",
        "--repo",
        s(&author),
    ]));
    assert_eq!(retried["revision"], 2);
    ok(rita(&[
        "request",
        "approve",
        "pr-1",
        "--revision",
        "2",
        "--operation",
        "approve-2",
        "--repo",
        s(&reviewer),
    ]));

    // Publish queues too. Without a base it cannot be built offline.
    let code = refused(mona(&[
        "request",
        "publish",
        "pr-1",
        "--revision",
        "2",
        "--operation",
        "publish-2",
        "--repo",
        s(&maintainer),
    ]));
    assert_eq!(code, "node_unavailable");
    let queued = ok(mona(&[
        "request",
        "publish",
        "pr-1",
        "--revision",
        "2",
        "--base",
        "0",
        "--operation",
        "publish-2",
        "--repo",
        s(&maintainer),
    ]));
    assert_eq!(queued["queued"], true, "{queued}");
    assert_eq!(team.deployment.node_head(), 0);

    // Sync delivers the Publish first, then fetches the World it made.
    set_node(&maintainer, &node);
    let synced = ok(mona(&["sync", "--repo", s(&maintainer)]));
    assert_eq!(synced["outbox"]["delivered"].as_array().unwrap().len(), 1);
    assert_eq!(synced["head"]["number"], 1, "{synced}");
    assert_eq!(team.deployment.node_head(), 1);
    let published = ok(mona(&[
        "request",
        "publish",
        "pr-1",
        "--revision",
        "2",
        "--base",
        "0",
        "--operation",
        "publish-2",
        "--repo",
        s(&maintainer),
    ]));
    assert_eq!(published["published"]["number"], 1);
}

#[test]
fn sync_refuses_a_local_world_that_moved_and_a_chain_that_misses_the_anchor() {
    let team = Team::start(0);
    let author = team.attach("author", None);
    let behind = team.attach("behind", Some(&team.mona));
    let mona = |args: &[&str]| team.as_token(&team.mona, args);

    ok(team.author(&["workspace", "create", "work", "--repo", s(&author)]));
    fs::write(author.join(".javelin/layers/work/a.txt"), "a\n").unwrap();
    ok(team.author(&[
        "request",
        "open",
        "pr-1",
        "--layer",
        "work",
        "--operation",
        "open-1",
        "--repo",
        s(&author),
    ]));
    ok(team.author(&[
        "request",
        "publish",
        "pr-1",
        "--revision",
        "1",
        "--operation",
        "publish-1",
        "--repo",
        s(&author),
    ]));
    assert_eq!(team.deployment.node_head(), 1);

    // A head the node's chain does not pass through is refused, and nothing changes.
    let before = attachment(&behind);
    let mut forged = before.clone();
    forged["head"]["id"] = javelin::ObjectId::for_blob(b"forged").to_string().into();
    fs::write(
        behind.join(".javelin/attach.json"),
        serde_json::to_vec(&forged).unwrap(),
    )
    .unwrap();
    assert_eq!(
        refused(mona(&["sync", "--repo", s(&behind)])),
        "chain_mismatch"
    );
    assert_eq!(attachment(&behind), forged);
    assert_eq!(
        Repository::open(&behind)
            .unwrap()
            .head()
            .unwrap()
            .number
            .get(),
        0
    );
    fs::write(
        behind.join(".javelin/attach.json"),
        serde_json::to_vec(&before).unwrap(),
    )
    .unwrap();
    assert_eq!(
        ok(mona(&["sync", "--repo", s(&behind)]))["head"]["number"],
        1
    );

    // A Publish made in this clone moves its World away from the node's: sync refuses it.
    ok(team.author(&["workspace", "create", "local", "--repo", s(&author)]));
    fs::write(author.join(".javelin/layers/local/b.txt"), "b\n").unwrap();
    ok(team.author(&[
        "workspace",
        "publish",
        "local",
        "--operation",
        "local-publish",
        "--repo",
        s(&author),
    ]));
    let head = Repository::open(&author).unwrap().head().unwrap();
    let attached = attachment(&author);
    let output = team.author(&["sync", "--repo", s(&author)]);
    assert_eq!(refused(output), "world_diverged");
    assert_eq!(Repository::open(&author).unwrap().head().unwrap(), head);
    assert_eq!(attachment(&author), attached);
}
