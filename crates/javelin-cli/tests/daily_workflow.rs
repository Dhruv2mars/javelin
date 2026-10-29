//! GA's promise in one test: a developer does their daily work with the `javelin` CLI and a
//! self-hosted node instead of Git and GitHub. Everything a user would type runs through the
//! real binaries: `javelin control init|serve|approve`, `javelin login`, `javelin repo
//! create|settings`, `javelin member invite`,
//! `javelin-node serve` as its own process, and the `javelin` commands of the daily loop.
//!
//! Two people, each with their own clone, both signed in through the control plane. The owner
//! invites the collaborator with `javelin member invite`; the collaborator signs in with
//! `javelin login --invite CODE` and works at once, with the node still serving. Their second
//! device signs in with `javelin login`, approved from the first.
//!
//! The loop: the owner creates the repository, imports a project straight onto the node as
//! World 1 and publishes a Layer directly as World 2 (Maintain, no approvals required). Once
//! Publish needs an approval, a direct Publish says to open a request. The collaborator
//! attaches, edits a Layer as a plain directory and opens a Publish Request. The owner reads,
//! comments and approves; the collaborator revises, the old approval stops counting, the
//! owner approves again, and the node restarts while that Publish is queued.
//! A concurrent change to the same lines conflicts cleanly, `layer take` moves a Layer to
//! the collaborator's other device, and every clone ends at the node's World, byte for byte,
//! printing the same World numbers and IDs.
//!
//! `tools/acceptance/daily-workflow.sh` is the same loop for a human against any node URL;
//! the last test here runs it against this node on Unix.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use javelin::Repository;
use serde_json::Value;

mod daily_support;

use daily_support::{
    JAVELIN, Person, Site, describe, exit3, files, node_binary, ok, owner_site, refused, step,
};

fn write(dir: &Path, path: &str, text: &str) {
    let path = dir.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

const LIB_V1: &str = "pub fn greet(name: &str) -> String {\n    format!(\"hello {name}\")\n}\n";

/// A small project, as the owner has it before Javelin: a crate with a README.
fn project(dir: &Path) {
    write(dir, "README.md", "# app\n\nGreets people.\n");
    write(
        dir,
        "Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    write(dir, "src/lib.rs", LIB_V1);
    write(
        dir,
        "src/main.rs",
        "fn main() {\n    println!(\"{}\", app::greet(\"world\"));\n}\n",
    );
    write(dir, ".javelinignore", "target/\n");
    // Build output stays out of the World.
    write(dir, "target/debug/app", "binary");
}

fn paths(changes: &Value) -> Vec<String> {
    changes["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["path"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn two_people_do_a_days_work_with_javelin_and_a_self_hosted_node() {
    step("owner: control plane, sign-in, repository, node");
    let (mut site, owner) = owner_site();

    step("owner: attach, import the project straight onto the node as World 1");
    let ours = owner.attach(&site, "owner");
    let source = site.dir("app-before-javelin");
    project(&source);
    let import = ["adopt", source.to_str().unwrap(), "--operation", "import"];
    let imported = ok(owner.at(&ours, &import));
    assert_eq!(imported["published"]["number"], 1, "{imported}");
    assert_eq!(imported["head"]["number"], 1, "the clone follows the node");
    assert_eq!(imported["files"], 5, "build output is ignored: {imported}");
    assert_eq!(site.node_head().number.get(), 1);
    // A retry replays the import; the project directory was never written.
    let again = ok(owner.at(&ours, &import));
    assert_eq!(again["published"], imported["published"]);
    assert!(source.join("target/debug/app").exists());
    let mut imported: Vec<_> = files(&owner.layer(&ours, "look")).into_keys().collect();
    imported.sort();
    assert_eq!(
        imported,
        [
            ".javelinignore",
            "Cargo.toml",
            "README.md",
            "src/lib.rs",
            "src/main.rs"
        ],
        "build output is ignored"
    );

    step("owner: edit a Layer and publish it directly: Maintain, no approvals required");
    let readme = owner.layer(&ours, "readme");
    write(&readme, "README.md", "# app\n\nGreets people by name.\n");
    let direct = ok(owner.at(
        &ours,
        &["workspace", "publish", "readme", "--operation", "readme-1"],
    ));
    assert_eq!(direct["published"]["number"], 2, "{direct}");
    assert_eq!(direct["head"]["number"], 2, "{direct}");
    assert_eq!(direct["closed"], true, "{direct}");
    assert!(
        !readme.exists(),
        "a published Layer's directory is reclaimed"
    );
    // The local World follows the node, so sync has nothing to refuse.
    let synced = owner.sync(&ours);
    assert_eq!(synced["head"]["number"], 2, "{synced}");
    assert_eq!(synced["advanced"], false);

    step("owner: invite cora as a reviewer; Publish now needs one approval");
    let code = owner.invite("acme/app", "cora", "review");
    let settings = ok(owner.run(&["repo", "settings", "acme/app", "--required-approvals", "1"]));
    assert_eq!(settings["required_approvals"], 1, "{settings}");
    let cora = site.person("cora-laptop-home");
    let accepted = cora.accept(&code);
    assert_eq!(
        (&accepted["principal"], &accepted["role"]),
        (&"cora".into(), &"review".into()),
        "{accepted}"
    );
    assert_eq!(accepted["node_error"], Value::Null, "{accepted}");
    let members = ok(owner.run(&["member", "list", "acme/app"]));
    assert_eq!(members["members"].as_array().unwrap().len(), 2, "{members}");
    assert_eq!(members["epoch"], 3, "{members}");

    step("owner: a direct Publish now says to open a request");
    let blocked = owner.layer(&ours, "owner-direct");
    write(&blocked, "CHANGELOG.md", "- greet by name\n");
    let refusal = owner.at(
        &ours,
        &[
            "workspace",
            "publish",
            "owner-direct",
            "--operation",
            "od-1",
        ],
    );
    let message = String::from_utf8_lossy(&refusal.stderr).to_string();
    assert_eq!(refused(refusal), "review_required", "{message}");
    assert!(message.contains("javelin request open"), "{message}");
    assert!(blocked.exists(), "a refused Layer keeps its directory");
    assert_eq!(site.node_head().number.get(), 2);

    step("cora: attach a second clone, edit a Layer as a plain directory, open a request");
    let theirs = cora.attach(&site, "cora");
    let feature = cora.layer(&theirs, "greeting");
    assert_eq!(
        fs::read_to_string(feature.join("src/lib.rs")).unwrap(),
        LIB_V1
    );
    assert!(
        !feature.join("target").exists(),
        "ignored files never left the owner"
    );
    write(
        &feature,
        "src/lib.rs",
        "pub fn greet(name: &str) -> String {\n    format!(\"hello, {name}!\")\n}\n",
    );
    write(&feature, "docs/usage.md", "Run `app`.\n");
    // A reviewer does not publish directly either.
    let refusal = cora.at(
        &theirs,
        &[
            "workspace",
            "publish",
            "greeting",
            "--operation",
            "cora-direct",
        ],
    );
    let message = String::from_utf8_lossy(&refusal.stderr).to_string();
    assert_eq!(refused(refusal), "insufficient_role", "{message}");
    assert!(message.contains("javelin request open"), "{message}");
    let opened = ok(cora.at(
        &theirs,
        &[
            "request",
            "open",
            "greeting",
            "--layer",
            "greeting",
            "--operation",
            "cora-open",
        ],
    ));
    assert_eq!(opened["revision"], 1, "{opened}");

    step("owner: read the request, comment, approve revision 1");
    let listed = ok(owner.at(&ours, &["request", "list"]));
    let open: Vec<_> = listed["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["state"] == "open")
        .collect();
    assert_eq!(open.len(), 1, "{listed}");
    assert_eq!(open[0]["author"], "cora");
    let shown = ok(owner.at(&ours, &["request", "show", "greeting"]));
    assert_eq!(paths(&shown["changes"]), ["docs/usage.md", "src/lib.rs"]);
    assert_eq!(shown["required_approvals"], 1);
    ok(owner.at(
        &ours,
        &[
            "request",
            "comment",
            "greeting",
            "--revision",
            "1",
            "--body",
            "Mention the binary name in the docs",
            "--operation",
            "owner-comment-1",
        ],
    ));
    ok(owner.at(
        &ours,
        &[
            "request",
            "approve",
            "greeting",
            "--revision",
            "1",
            "--operation",
            "owner-approve-1",
        ],
    ));
    // The author cannot approve their own work.
    let code = refused(cora.at(
        &theirs,
        &[
            "request",
            "approve",
            "greeting",
            "--revision",
            "1",
            "--operation",
            "cora-self",
        ],
    ));
    assert_eq!(code, "self_approval");

    step("cora: read the comment, revise");
    let shown = ok(cora.at(&theirs, &["request", "show", "greeting"]));
    assert_eq!(
        shown["comments"][0]["body"],
        "Mention the binary name in the docs"
    );
    assert_eq!(shown["revisions"][0]["approvals"][0], "admin");
    write(&feature, "docs/usage.md", "Run `app` to greet the world.\n");
    let revised = ok(cora.at(
        &theirs,
        &[
            "request",
            "revise",
            "greeting",
            "--revision",
            "1",
            "--operation",
            "cora-revise-1",
        ],
    ));
    assert_eq!(revised["revision"], 2, "{revised}");

    step("owner: the approval of revision 1 no longer counts");
    let shown = ok(owner.at(&ours, &["request", "show", "greeting"]));
    assert_eq!(shown["revision"], 2);
    assert_eq!(shown["revisions"][1]["approvals"], serde_json::json!([]));
    let code = refused(owner.at(
        &ours,
        &[
            "request",
            "publish",
            "greeting",
            "--revision",
            "2",
            "--operation",
            "too-early",
        ],
    ));
    assert_eq!(code, "not_enough_approvals");
    let stale = exit3(owner.at(
        &ours,
        &[
            "request",
            "publish",
            "greeting",
            "--revision",
            "1",
            "--operation",
            "old-revision",
        ],
    ));
    assert_eq!(stale["code"], "stale_revision");
    assert_eq!(site.node_head().number.get(), 2);
    ok(owner.at(
        &ours,
        &[
            "request",
            "approve",
            "greeting",
            "--revision",
            "2",
            "--operation",
            "owner-approve-2",
        ],
    ));

    step("the node restarts mid-loop: the owner's Publish queues, then replays");
    site.stop_node();
    let base = shown["revisions"][1]["base"].as_u64().unwrap().to_string();
    let queued = ok(owner.at(
        &ours,
        &[
            "request",
            "publish",
            "greeting",
            "--revision",
            "2",
            "--base",
            &base,
            "--operation",
            "owner-publish-2",
        ],
    ));
    assert_eq!(queued["queued"], true, "{queued}");
    assert_eq!(queued["code"], "node_unavailable");
    let code = refused(cora.at(&theirs, &["request", "show", "greeting"]));
    assert_eq!(code, "node_unavailable");
    site.start_node();
    let synced = owner.sync(&ours);
    assert_eq!(
        synced["outbox"]["delivered"].as_array().unwrap().len(),
        1,
        "{synced}"
    );
    assert_eq!(synced["outbox"]["pending"], 0);
    assert_eq!(synced["head"]["number"], 3, "{synced}");
    // Nothing the node acknowledged before the restart was lost.
    let shown = ok(cora.at(&theirs, &["request", "show", "greeting"]));
    assert_eq!(shown["state"], "published", "{shown}");
    assert_eq!(shown["comments"].as_array().unwrap().len(), 1);
    assert_eq!(shown["revisions"][0]["approvals"][0], "admin");
    assert_eq!(shown["revisions"][1]["approvals"][0], "admin");
    // A retry of the queued command returns the delivered outcome.
    let replayed = ok(owner.at(
        &ours,
        &[
            "request",
            "publish",
            "greeting",
            "--revision",
            "2",
            "--base",
            &base,
            "--operation",
            "owner-publish-2",
        ],
    ));
    assert_eq!(replayed["published"]["number"], 3, "{replayed}");

    step("both clones sync to World 3, byte for byte, and number their Worlds alike");
    assert_eq!(cora.sync(&theirs)["head"]["number"], 3);
    let world = same_world(
        &site,
        &[(&owner, &ours), (&cora, &theirs)],
        "after-greeting",
    );
    assert_eq!(world["docs/usage.md"], b"Run `app` to greet the world.\n");
    same_numbers(&[(&owner, &ours), (&cora, &theirs)], 3);

    step("a concurrent change to the same lines conflicts cleanly");
    let mine = owner.layer(&ours, "owner-tweak");
    let yours = cora.layer(&theirs, "cora-tweak");
    let line = |text: &str| format!("pub fn greet(name: &str) -> String {{\n    {text}\n}}\n");
    write(&mine, "src/lib.rs", &line("format!(\"Hello, {name}!\")"));
    write(&yours, "src/lib.rs", &line("format!(\"hi {name}\")"));
    ok(owner.at(
        &ours,
        &[
            "request",
            "open",
            "owner-tweak",
            "--layer",
            "owner-tweak",
            "--operation",
            "o-open",
        ],
    ));
    ok(cora.at(
        &theirs,
        &[
            "request",
            "open",
            "cora-tweak",
            "--layer",
            "cora-tweak",
            "--operation",
            "c-open",
        ],
    ));
    ok(cora.at(
        &theirs,
        &[
            "request",
            "approve",
            "owner-tweak",
            "--revision",
            "1",
            "--operation",
            "c-approve",
        ],
    ));
    ok(owner.at(
        &ours,
        &[
            "request",
            "approve",
            "cora-tweak",
            "--revision",
            "1",
            "--operation",
            "o-approve",
        ],
    ));
    let first = ok(owner.at(
        &ours,
        &[
            "request",
            "publish",
            "owner-tweak",
            "--revision",
            "1",
            "--operation",
            "o-publish",
        ],
    ));
    assert_eq!(first["published"]["number"], 4);
    let conflict = exit3(owner.at(
        &ours,
        &[
            "request",
            "publish",
            "cora-tweak",
            "--revision",
            "1",
            "--operation",
            "o-publish-c",
        ],
    ));
    assert_eq!(conflict["code"], "conflict", "{conflict}");
    assert_eq!(conflict["paths"], serde_json::json!(["src/lib.rs"]));
    assert_eq!(
        site.node_head().number.get(),
        4,
        "a conflict publishes nothing"
    );
    // The text names the way out; a retry with the same operation replays the conflict.
    let text = owner
        .command()
        .args(["request", "publish", "cora-tweak", "--revision", "1"])
        .args([
            "--operation",
            "o-publish-c",
            "--repo",
            ours.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(text.status.code(), Some(3), "{}", describe(&text));
    assert!(
        String::from_utf8_lossy(&text.stdout).contains(
            "`javelin request revise cora-tweak --revision 1 --layer NEW --operation OP`"
        ),
        "{}",
        describe(&text)
    );

    step("cora: see the conflict coming, redo the change over World 4, revise");
    cora.sync(&theirs);
    let radar = ok(cora.at(&theirs, &["layer", "radar", "cora-tweak"]));
    assert_eq!(radar["clear"], false);
    assert_eq!(
        radar["radar"]["world"]["paths"],
        serde_json::json!(["src/lib.rs"])
    );
    let redo = cora.layer(&theirs, "cora-tweak-2");
    assert_eq!(
        fs::read_to_string(redo.join("src/lib.rs")).unwrap(),
        line("format!(\"Hello, {name}!\")")
    );
    write(
        &redo,
        "src/lib.rs",
        &line("format!(\"Hello, {name}! hi!\")"),
    );
    let revised = ok(cora.at(
        &theirs,
        &[
            "request",
            "revise",
            "cora-tweak",
            "--revision",
            "1",
            "--layer",
            "cora-tweak-2",
            "--operation",
            "c-revise",
        ],
    ));
    assert_eq!(revised["revision"], 2, "{revised}");
    ok(owner.at(
        &ours,
        &[
            "request",
            "approve",
            "cora-tweak",
            "--revision",
            "2",
            "--operation",
            "o-approve-2",
        ],
    ));
    let second = ok(owner.at(
        &ours,
        &[
            "request",
            "publish",
            "cora-tweak",
            "--revision",
            "2",
            "--operation",
            "o-publish-2",
        ],
    ));
    assert_eq!(second["published"]["number"], 5, "{second}");

    step("cora: layer take moves a Layer from the laptop to the desktop");
    cora.sync(&theirs);
    let notes = cora.layer(&theirs, "notes");
    write(&notes, "NOTES.md", "- started on the laptop\n");
    ok(cora.at(
        &theirs,
        &[
            "request",
            "open",
            "notes",
            "--layer",
            "notes",
            "--operation",
            "notes-open",
        ],
    ));
    // Cora's second device signs in as cora, approved from the first.
    let desktop = site.person("cora-desktop-home");
    desktop.login_from(&cora);
    let other = desktop.attach(&site, "cora-desktop");
    // Only the Layer's owner can take it.
    let code = refused(owner.at(&ours, &["layer", "take", "notes", "--operation", "o-take"]));
    assert_eq!(code, "forbidden");
    let taken = ok(desktop.at(
        &other,
        &["layer", "take", "notes", "--operation", "take-notes"],
    ));
    assert_eq!(taken["holds_lease"], true, "{taken}");
    let moved = PathBuf::from(taken["root"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(moved.join("NOTES.md")).unwrap(),
        "- started on the laptop\n"
    );
    // The laptop no longer writes to the Layer. Its late edit is refused as layer_stale, and
    // stays here: `layer clone` makes a new Layer of it.
    write(
        &notes,
        "NOTES.md",
        "- started on the laptop\n- late laptop edit\n",
    );
    let late = cora.at(
        &theirs,
        &[
            "request",
            "revise",
            "notes",
            "--revision",
            "1",
            "--operation",
            "late-revise",
        ],
    );
    let message = String::from_utf8_lossy(&late.stderr).to_string();
    assert_eq!(refused(late), "layer_stale", "{message}");
    let status = ok(cora.at(&theirs, &["outbox", "status", "--remote", &site.repository]));
    let local_head = status["stale"][0]["local_head"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains(&local_head), "{message}");
    ok(cora.at(
        &theirs,
        &[
            "layer",
            "clone",
            "notes-laptop",
            "--checkpoint",
            &local_head,
        ],
    ));
    let kept = ok(cora.at(&theirs, &["workspace", "open", "notes-laptop"]));
    assert_eq!(
        fs::read_to_string(Path::new(kept["workspace"]["root"].as_str().unwrap()).join("NOTES.md"))
            .unwrap(),
        "- started on the laptop\n- late laptop edit\n"
    );
    write(
        &moved,
        "NOTES.md",
        "- started on the laptop\n- finished on the desktop\n",
    );
    let revised = ok(desktop.at(
        &other,
        &[
            "request",
            "revise",
            "notes",
            "--revision",
            "1",
            "--layer",
            "notes",
            "--operation",
            "desktop-revise",
        ],
    ));
    assert_eq!(revised["revision"], 2, "{revised}");
    ok(owner.at(
        &ours,
        &[
            "request",
            "approve",
            "notes",
            "--revision",
            "2",
            "--operation",
            "o-approve-notes",
        ],
    ));
    let third = ok(owner.at(
        &ours,
        &[
            "request",
            "publish",
            "notes",
            "--revision",
            "2",
            "--operation",
            "o-publish-notes",
        ],
    ));
    assert_eq!(third["published"]["number"], 6, "{third}");

    step("every clone syncs and holds the node's World, byte for byte");
    let clones = [(&owner, &ours), (&cora, &theirs), (&desktop, &other)];
    for (person, clone) in clones {
        assert_eq!(person.sync(clone)["head"]["number"], 6);
    }
    same_numbers(&clones, 6);
    let world = same_world(&site, &clones, "final");
    assert_eq!(
        world["NOTES.md"],
        b"- started on the laptop\n- finished on the desktop\n"
    );
    assert_eq!(world["docs/usage.md"], b"Run `app` to greet the world.\n");
    assert!(!world.contains_key("target/debug/app"));
    // History reads the clone like every other command, with --repo, in the node's IDs.
    let history = ok(owner.at(&ours, &["history"]));
    let node = site.node_head();
    let last = history["versions"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["number"], node.number.get(), "{history}");
    assert_eq!(last["id"], node.id.to_string(), "{history}");
    assert_eq!(last["tree"], node.tree.to_string(), "{history}");
    site.assert_no_git();
}

/// Clones of one repository agree on World numbers: `history` gives every World a clone holds
/// the node's number and ID, the same in each clone, and ends at the node's head `head`; a
/// Layer created in each says it is over that World.
fn same_numbers(clones: &[(&Person, &PathBuf)], head: u64) {
    let mut seen: BTreeMap<u64, String> = BTreeMap::new();
    for (person, clone) in clones {
        let history = ok(person.at(clone, &["history"]));
        let versions = history["versions"].as_array().unwrap();
        assert_eq!(versions.last().unwrap()["number"], head, "{history}");
        for version in versions {
            let number = version["number"].as_u64().expect("a node number");
            let id = version["id"].as_str().unwrap().to_string();
            assert_eq!(
                seen.entry(number).or_insert_with(|| id.clone()),
                &id,
                "{history}"
            );
        }
        let text = person
            .command()
            .args(["workspace", "create", &format!("numbers-{head}")])
            .args(["--repo", clone.to_str().unwrap()])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&text.stdout).to_string();
        assert!(text.contains(&format!("Base World {head}:")), "{text}");
        let lines = person
            .command()
            .args(["history", "--repo", clone.to_str().unwrap()])
            .output()
            .unwrap();
        let lines = String::from_utf8_lossy(&lines.stdout).to_string();
        assert!(
            lines
                .lines()
                .last()
                .unwrap()
                .starts_with(&format!("World {head}  ")),
            "{lines}"
        );
    }
}

/// Every clone is attached at the node's head and holds its tree, and a Layer opened over it
/// in each clone holds the same files with the same bytes, which it returns.
fn same_world(
    site: &Site,
    clones: &[(&Person, &PathBuf)],
    layer: &str,
) -> BTreeMap<String, Vec<u8>> {
    let node = site.node_head();
    let mut trees = Vec::new();
    for (person, clone) in clones {
        let attached: Value =
            serde_json::from_slice(&fs::read(clone.join(".javelin/attach.json")).unwrap()).unwrap();
        assert_eq!(
            attached["head"]["id"],
            node.id.to_string(),
            "{}",
            clone.display()
        );
        assert_eq!(attached["head"]["number"], node.number.get());
        let head = Repository::open(clone).unwrap().head().unwrap();
        assert_eq!(
            head.tree,
            node.tree,
            "{} holds the node's tree",
            clone.display()
        );
        trees.push(files(&person.layer(clone, layer)));
    }
    for tree in &trees[1..] {
        assert_eq!(tree, &trees[0]);
    }
    trees.remove(0)
}

/// `tools/acceptance/daily-workflow.sh` runs the same loop against this node, restarting it
/// through `NODE_SERVE`: it invites the collaborator itself (`INVITE`), signs in their second
/// device for `layer take`, and never runs Git either.
#[cfg(unix)]
#[test]
fn the_shell_version_runs_the_same_loop_against_a_node() {
    let (mut site, owner) = owner_site();
    ok(owner.run(&["repo", "settings", "acme/app", "--required-approvals", "1"]));
    // The script starts and restarts the node itself.
    site.stop_node();
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/acceptance/daily-workflow.sh");
    let serve = format!(
        "{} serve {} {} 127.0.0.1:{}",
        node_binary().display(),
        site.node_dir().display(),
        site.repository,
        site.node_port()
    );
    let output = Command::new("bash")
        .arg(script)
        .env("PATH", site.path())
        .arg(site.attach_url())
        .env("JAVELIN", JAVELIN)
        .env("WORKDIR", site.dir("script"))
        .env("OWNER_HOME", &owner.home)
        .env("JAVELIN_CONTROL_URL", &site.control_url)
        .env("INVITE", "cora")
        .env("COLLAB_HOME", site.dir("script-cora-laptop"))
        .env("DESKTOP_HOME", site.dir("script-cora-desktop"))
        .env("NODE_SERVE", serve)
        .env_remove("JAVELIN_TOKEN")
        .env_remove("JAVELIN_ADMIN_TOKEN")
        .env_remove("JAVELIN_CONTROL_DIR")
        .output()
        .unwrap();
    let printed = describe(&output);
    eprintln!("{printed}");
    assert_eq!(output.status.code(), Some(0), "{printed}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("== PASS"), "{printed}");
    assert!(stdout.contains("== 0. owner: invite"), "{printed}");
    assert!(!stdout.contains("SKIP"), "{printed}");
    site.assert_no_git();
}
