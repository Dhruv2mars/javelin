//! Streaming checkpoints from the CLI, end to end with the real binary against the node's
//! server on loopback, with real tokens: the laptop is a device signed in through the control
//! plane, and the desktop is another device of the same owner. Every capture uploads its
//! checkpoint; a node outage queues them and they replay; `layer take` on another clone gets
//! the latest capture, not a revision; a device that lost the writer lease stops uploading and
//! keeps its work; and an upload never reaches a path or a reader outside its audience
//! (`docs/direction.md`, Layers follow their owner; `docs/online-design.md` section 4.2).

use std::{
    fs,
    path::{Path, PathBuf},
    process::Output,
    time::{Duration, Instant},
};

use javelin::{FileEntry, LayerId, LayerState, ObjectId, RepoPath, Repository};
use javelin_collaboration::Role;
use javelin_identity::Scope;
use javelin_protocol::{
    Message,
    codec::{Kind, decode, encode},
};
use serde_json::Value;

mod wire_support;

use wire_support::{Deployment, closed_url, stdout};

const DEVICE: &[Scope] = &[Scope::Read, Scope::LayerWrite];
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

/// A path a checkpoint changes and the bytes it now holds, or `None` when it is deleted.
type File = (String, Option<Vec<u8>>);

struct Owner {
    deployment: Deployment,
    /// The owner's second device: a token from another device than the signed-in one.
    desktop: String,
}

impl Owner {
    fn start(roles: &[(&str, Role)]) -> Self {
        let deployment = Deployment::start_with(roles, 0, |_| Vec::new());
        let desktop = deployment.token(&deployment.principal, DEVICE);
        Self {
            deployment,
            desktop,
        }
    }

    /// Runs the CLI on the laptop, the device signed in through the control plane.
    fn laptop(&self, args: &[&str]) -> Output {
        self.deployment.javelin(&self.deployment.control_url, args)
    }

    fn desktop(&self, args: &[&str]) -> Output {
        self.deployment.javelin_as(&self.desktop, args)
    }

    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.deployment.temp.path().join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Attaches `name` with `run`, the device that will use it.
    fn attach(&self, name: &str, run: impl Fn(&[&str]) -> Output) -> PathBuf {
        let dir = self.dir(name);
        let url = self.deployment.attach_url();
        ok(run(&["attach", url.as_str(), s(&dir)]));
        dir
    }

    /// The node's head for Layer `name`: its revision and the bytes of each file it changes.
    fn node_layer(&self, name: &str) -> (u64, Vec<File>) {
        let repo = self.deployment.node_repo();
        let LayerState::Active { head } = repo.layer_state(&layer(name)).unwrap() else {
            panic!("the node's Layer {name} is not active");
        };
        let checkpoint = repo.read_checkpoint(head.checkpoint()).unwrap();
        let files = checkpoint
            .changes()
            .entries()
            .iter()
            .map(|(path, entry)| {
                let bytes = entry
                    .as_ref()
                    .and_then(FileEntry::content)
                    .map(|id| repo.read_blob(id).unwrap());
                (path.as_str().to_owned(), bytes)
            })
            .collect();
        (head.revision(), files)
    }
}

fn layer(name: &str) -> LayerId {
    LayerId::parse(name).unwrap()
}

fn file(name: &str, bytes: &str) -> File {
    (name.into(), Some(bytes.as_bytes().to_vec()))
}

/// Captures Layer `name` in `dir` with `run` and returns the capture's upload report.
fn checkpoint(run: impl Fn(&[&str]) -> Output, dir: &Path, name: &str) -> Value {
    let captured = ok(run(&["checkpoint", name, "--repo", s(dir)]));
    captured["capture"]["upload"].clone()
}

/// `javelin status` for Layer `name`.
fn layer_status(run: impl Fn(&[&str]) -> Output, dir: &Path, name: &str) -> Value {
    let status = ok(run(&["status", "--repo", s(dir)]));
    status["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["layer"] == name)
        .unwrap_or_else(|| panic!("no Layer {name} in {status}"))
        .clone()
}

/// The appends `dir`'s outbox has ever journaled.
fn appends(dir: &Path, repository: &str) -> usize {
    javelin::outbox::Outbox::open(dir, repository)
        .unwrap()
        .entries()
        .filter(|entry| entry.kind == javelin::outbox::Kind::Append)
        .count()
}

fn workspace(dir: &Path, name: &str) -> PathBuf {
    dir.join(".javelin").join("layers").join(name)
}

#[test]
fn captures_upload_and_another_device_takes_the_latest_one() {
    let owner = Owner::start(&[]);
    let laptop_dir = owner.attach("laptop", |args| owner.laptop(args));
    let laptop = |args: &[&str]| owner.laptop(args);

    ok(laptop(&[
        "workspace",
        "create",
        "work",
        "--repo",
        s(&laptop_dir),
    ]));
    let work = workspace(&laptop_dir, "work");
    fs::write(work.join("notes.txt"), "1\n").unwrap();
    assert_eq!(
        checkpoint(laptop, &laptop_dir, "work")["status"],
        "uploaded"
    );
    assert_eq!(
        owner.node_layer("work"),
        (1, vec![file("notes.txt", "1\n")])
    );

    // A Layer command captures first, and that capture uploads too.
    fs::write(work.join("notes.txt"), "2\n").unwrap();
    let state = ok(laptop(&[
        "layer",
        "state",
        "work",
        "--repo",
        s(&laptop_dir),
    ]));
    assert_eq!(state["capture"]["upload"]["status"], "uploaded", "{state}");
    assert_eq!(
        owner.node_layer("work"),
        (2, vec![file("notes.txt", "2\n")])
    );
    let status = layer_status(laptop, &laptop_dir, "work");
    assert_eq!(status["status"], "uploaded", "{status}");
    assert_eq!(status["revision"], 2);

    // No request was ever opened: the desktop takes the Layer at the latest capture.
    let desktop_dir = owner.attach("desktop", |args| owner.desktop(args));
    let desktop = |args: &[&str]| owner.desktop(args);
    let taken = ok(desktop(&[
        "layer",
        "take",
        "work",
        "--repo",
        s(&desktop_dir),
    ]));
    assert_eq!(taken["holds_lease"], true, "{taken}");
    assert_eq!(taken["node"]["seq"], 2);
    let taken_work = PathBuf::from(taken["root"].as_str().unwrap());
    assert_eq!(fs::read(taken_work.join("notes.txt")).unwrap(), b"2\n");

    // The desktop continues the Layer, and its captures upload after the node's head.
    fs::write(taken_work.join("notes.txt"), "3\n").unwrap();
    assert_eq!(
        checkpoint(desktop, &desktop_dir, "work")["status"],
        "uploaded"
    );
    assert_eq!(
        owner.node_layer("work"),
        (3, vec![file("notes.txt", "3\n")])
    );

    // The laptop no longer holds the lease: its next upload is refused as stale, and it
    // stops uploading. Its checkpoints stay local and the node keeps the desktop's work.
    fs::write(work.join("notes.txt"), "laptop 4\n").unwrap();
    let stale = checkpoint(laptop, &laptop_dir, "work");
    assert_eq!(stale["status"], "layer_stale", "{stale}");
    assert!(
        stale["detail"]
            .as_str()
            .unwrap()
            .contains("javelin layer clone NEW --checkpoint"),
        "{stale}"
    );
    let journaled = appends(&laptop_dir, &owner.deployment.repository);
    fs::write(work.join("notes.txt"), "laptop 5\n").unwrap();
    let held = checkpoint(laptop, &laptop_dir, "work");
    assert_eq!(held["status"], "layer_stale", "{held}");
    assert_eq!(
        appends(&laptop_dir, &owner.deployment.repository),
        journaled,
        "a stale device queues nothing"
    );
    let status = layer_status(laptop, &laptop_dir, "work");
    assert_eq!(status["status"], "layer_stale", "{status}");
    assert_eq!(
        owner.node_layer("work"),
        (3, vec![file("notes.txt", "3\n")])
    );
    let repo = Repository::open(&laptop_dir).unwrap();
    let LayerState::Active { head } = repo.layer_state(&layer("work")).unwrap() else {
        panic!("the laptop's Layer is not active");
    };
    let manifest = repo.checkpoint_manifest(head.checkpoint()).unwrap();
    let notes = manifest.entries()[&RepoPath::parse("notes.txt").unwrap()]
        .content()
        .unwrap();
    assert_eq!(repo.read_blob(notes).unwrap(), b"laptop 5\n");

    // The report's command keeps the laptop's work as a new Layer.
    let rescued = ok(laptop(&[
        "layer",
        "clone",
        "rescued",
        "--checkpoint",
        &head.checkpoint().object().to_string(),
        "--repo",
        s(&laptop_dir),
    ]));
    assert_eq!(rescued["ok"], true);
}

#[test]
fn reverted_contents_upload_as_new_checkpoints() {
    let owner = Owner::start(&[]);
    let laptop_dir = owner.attach("laptop", |args| owner.laptop(args));
    let laptop = |args: &[&str]| owner.laptop(args);
    ok(laptop(&[
        "workspace",
        "create",
        "work",
        "--repo",
        s(&laptop_dir),
    ]));
    let work = workspace(&laptop_dir, "work");
    for (n, content) in ["A\n", "B\n", "A\n", "B\n"].into_iter().enumerate() {
        fs::write(work.join("a.txt"), content).unwrap();
        assert_eq!(
            checkpoint(laptop, &laptop_dir, "work")["status"],
            "uploaded"
        );
        assert_eq!(
            owner.node_layer("work"),
            (n as u64 + 1, vec![file("a.txt", content)])
        );
    }
}

#[test]
fn a_node_outage_queues_captures_and_they_replay() {
    let owner = Owner::start(&[]);
    let node = owner.deployment.node_url.clone();
    let down = closed_url();
    let laptop_dir = owner.attach("laptop", |args| owner.laptop(args));
    let laptop = |args: &[&str]| owner.laptop(args);
    let repository = owner.deployment.repository.clone();

    ok(laptop(&[
        "workspace",
        "create",
        "work",
        "--repo",
        s(&laptop_dir),
    ]));
    let work = workspace(&laptop_dir, "work");
    fs::write(work.join("a.txt"), "1\n").unwrap();
    assert_eq!(
        checkpoint(laptop, &laptop_dir, "work")["status"],
        "uploaded"
    );

    // The node goes down: captures still succeed, and their uploads queue.
    set_node(&laptop_dir, &down);
    for n in ["2\n", "3\n"] {
        fs::write(work.join("a.txt"), n).unwrap();
        let queued = checkpoint(laptop, &laptop_dir, "work");
        assert_eq!(queued["status"], "queued", "{queued}");
        assert!(
            queued["detail"]
                .as_str()
                .unwrap()
                .contains("node_unavailable"),
            "{queued}"
        );
    }
    let status = layer_status(laptop, &laptop_dir, "work");
    assert_eq!(status["status"], "queued", "{status}");
    assert_eq!(status["queued"], 2);
    // A Layer the node has never seen cannot name its parent yet; its captures wait for the
    // node, every one of them.
    ok(laptop(&[
        "workspace",
        "create",
        "fresh",
        "--repo",
        s(&laptop_dir),
    ]));
    for n in ["b1\n", "b2\n"] {
        fs::write(workspace(&laptop_dir, "fresh").join("b.txt"), n).unwrap();
        let waiting = checkpoint(laptop, &laptop_dir, "fresh");
        assert_eq!(waiting["status"], "not_queued", "{waiting}");
    }
    assert_eq!(
        layer_status(laptop, &laptop_dir, "fresh")["status"],
        "not_uploaded"
    );
    assert_eq!(owner.node_layer("work"), (1, vec![file("a.txt", "1\n")]));

    // Back up: a capture that records nothing still delivers what waited, in order.
    set_node(&laptop_dir, &node);
    assert_eq!(
        checkpoint(laptop, &laptop_dir, "work")["status"],
        "uploaded"
    );
    assert_eq!(owner.node_layer("work"), (3, vec![file("a.txt", "3\n")]));
    fs::write(work.join("a.txt"), "4\n").unwrap();
    assert_eq!(
        checkpoint(laptop, &laptop_dir, "work")["status"],
        "uploaded"
    );
    assert_eq!(owner.node_layer("work"), (4, vec![file("a.txt", "4\n")]));
    assert_eq!(
        javelin::outbox::Outbox::open(&laptop_dir, &repository)
            .unwrap()
            .pending()
            .count(),
        0
    );

    // A node that accepts connections and never answers holds a capture only for the upload
    // budget (2 s); the upload queues. Then `javelin sync` replays the queue and uploads the
    // Layer that never reached the node.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    set_node(
        &laptop_dir,
        &format!("http://{}", silent.local_addr().unwrap()),
    );
    fs::write(work.join("a.txt"), "5\n").unwrap();
    let started = Instant::now();
    assert_eq!(checkpoint(laptop, &laptop_dir, "work")["status"], "queued");
    let waited = started.elapsed();
    assert!(
        waited < Duration::from_secs(10),
        "the capture waited {waited:?}"
    );
    drop(silent);
    set_node(&laptop_dir, &node);
    let synced = ok(laptop(&["sync", "--repo", s(&laptop_dir)]));
    assert_eq!(synced["outbox"]["pending"], 0, "{synced}");
    assert_eq!(synced["uploads"][0]["layer"], "fresh", "{synced}");
    assert_eq!(synced["uploads"][0]["status"], "uploaded", "{synced}");
    assert_eq!(owner.node_layer("work"), (5, vec![file("a.txt", "5\n")]));
    assert_eq!(owner.node_layer("fresh"), (2, vec![file("b.txt", "b2\n")]));
    for name in ["work", "fresh"] {
        assert_eq!(
            layer_status(laptop, &laptop_dir, name)["status"],
            "uploaded"
        );
    }
    // A second sync has nothing left to upload.
    let again = ok(laptop(&["sync", "--repo", s(&laptop_dir)]));
    assert_eq!(again["uploads"], Value::Array(Vec::new()), "{again}");
}

/// Asks the node whether `token`'s holder can have blob `id`: `true` when the node answers
/// that it lacks it, as it answers for an object the caller may not read.
fn absent_for(deployment: &Deployment, token: &str, id: ObjectId) -> bool {
    let body = encode(&Message::Want(vec![(Kind::Blob, id)])).unwrap();
    let mut reply = ureq::post(format!(
        "{}/v2/r/{}",
        deployment.node_url, deployment.repository
    ))
    .header("authorization", &format!("Bearer {token}"))
    .header("content-type", "application/octet-stream")
    .send(&body[..])
    .unwrap();
    match decode(&reply.body_mut().read_to_vec().unwrap()).unwrap() {
        Message::Missing(missing) => missing == [(Kind::Blob, id)],
        other => panic!("unexpected reply {other:?}"),
    }
}

#[test]
fn uploads_carry_only_what_the_writer_may_write_and_never_widen_an_audience() {
    let owner = Owner::start(&[("rita", Role::Read), ("sam", Role::Read)]);
    let mona = owner
        .deployment
        .token(&owner.deployment.principal, MAINTAINER);
    let rita = owner.deployment.token("rita", DEVICE);
    let sam = owner.deployment.token("sam", DEVICE);
    let maintainer = |args: &[&str]| owner.deployment.javelin_as(&mona, args);
    let member = |args: &[&str]| owner.deployment.javelin_as(&rita, args);

    // The owner, a maintainer, publishes a policy that hides `*.key` from members.
    let maintainer_dir = owner.attach("maintainer", maintainer);
    ok(maintainer(&[
        "workspace",
        "create",
        "policy",
        "--repo",
        s(&maintainer_dir),
    ]));
    let policy = workspace(&maintainer_dir, "policy");
    fs::write(policy.join(".javelinpolicy"), "*.key maintainers\n").unwrap();
    fs::write(policy.join("a.key"), "k1\n").unwrap();
    fs::write(policy.join("readme"), "public\n").unwrap();
    for args in [
        &[
            "request",
            "open",
            "pr-policy",
            "--layer",
            "policy",
            "--operation",
            "open-policy",
        ][..],
        &[
            "request",
            "publish",
            "pr-policy",
            "--revision",
            "1",
            "--operation",
            "publish-policy",
        ][..],
    ] {
        let mut args = args.to_vec();
        args.extend(["--repo", s(&maintainer_dir)]);
        ok(maintainer(&args));
    }
    ok(maintainer(&["sync", "--repo", s(&maintainer_dir)]));

    // The maintainer edits the hidden file in a new Layer; it uploads, and a member cannot
    // have its bytes from the node.
    ok(maintainer(&[
        "workspace",
        "create",
        "keys",
        "--repo",
        s(&maintainer_dir),
    ]));
    let keys = workspace(&maintainer_dir, "keys");
    fs::write(keys.join("a.key"), "k2\n").unwrap();
    assert_eq!(
        checkpoint(maintainer, &maintainer_dir, "keys")["status"],
        "uploaded"
    );
    assert_eq!(owner.node_layer("keys"), (1, vec![file("a.key", "k2\n")]));
    let k2 = ObjectId::for_blob(b"k2\n");
    assert!(!absent_for(&owner.deployment, &mona, k2));
    assert!(absent_for(&owner.deployment, &rita, k2));

    // A member's new file at a maintainers-only path is refused as a whole: the node's Layer
    // takes none of the capture, the checkpoint stays local, and the bytes the member sent
    // stay absent to every other member.
    let member_dir = owner.attach("member", member);
    ok(member(&[
        "workspace",
        "create",
        "draft",
        "--repo",
        s(&member_dir),
    ]));
    let draft = workspace(&member_dir, "draft");
    fs::write(draft.join("b.key"), "member key\n").unwrap();
    fs::write(draft.join("notes"), "n1\n").unwrap();
    let refused = checkpoint(member, &member_dir, "draft");
    assert_eq!(refused["status"], "forbidden", "{refused}");
    assert_eq!(owner.node_layer("draft"), (0, Vec::new()));
    assert!(absent_for(
        &owner.deployment,
        &sam,
        ObjectId::for_blob(b"member key\n")
    ));
    assert_eq!(
        layer_status(member, &member_dir, "draft")["status"],
        "forbidden"
    );

    // Without that path, the next capture tries again from the node's head and uploads.
    fs::remove_file(draft.join("b.key")).unwrap();
    assert_eq!(
        checkpoint(member, &member_dir, "draft")["status"],
        "uploaded"
    );
    assert_eq!(owner.node_layer("draft"), (1, vec![file("notes", "n1\n")]));
}
