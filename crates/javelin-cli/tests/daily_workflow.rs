//! GA's promise in one test: a developer does their daily work with the `javelin` CLI and a
//! self-hosted node instead of Git and GitHub. Everything a user would type runs through the
//! real binaries: `javelin control init|serve|approve`, `javelin login`, `javelin repo create`,
//! `javelin-node serve` as its own process, and the `javelin` commands of the daily loop.
//!
//! Two people, each with their own clone. The owner is signed in through the control plane.
//! The collaborator holds a token signed with a key the node trusts, because the control plane
//! has no membership API yet: inviting them is the one step done here as an operator, by
//! pushing permissions into the node's directory while it is stopped (TASKS.md lists it).
//!
//! The loop: the owner creates the repository, imports a project and publishes World 1. The
//! collaborator attaches, edits a Layer as a plain directory and opens a Publish Request. The
//! owner reads, comments and approves; the collaborator revises, the old approval stops
//! counting, the owner approves again, and the node restarts while that Publish is queued.
//! A concurrent change to the same lines conflicts cleanly, `layer take` moves a Layer to
//! the collaborator's other device, and every clone ends at the node's World, byte for byte.
//!
//! `tools/acceptance/daily-workflow.sh` is the same loop for a human against any node URL;
//! the last test here runs it against this node on Unix.

use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use javelin::Repository;
use javelin_collaboration::{PrincipalId, Role};
use javelin_control::{LocalNode, NodeLink};
use javelin_identity::{Scope, test_keys};
use javelin_node::auth;
use serde_json::Value;
use tempfile::TempDir;

const JAVELIN: &str = env!("CARGO_BIN_EXE_javelin");

/// What the collaborator's token carries: read, write their Layers, comment and approve.
const COLLABORATOR: &[Scope] = &[
    Scope::Read,
    Scope::LayerWrite,
    Scope::ReviewWrite,
    Scope::ReviewApprove,
];

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn step(text: &str) {
    eprintln!("\n== {text}");
}

fn describe(output: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn json(bytes: &[u8], output: &Output) -> Value {
    serde_json::from_slice(bytes).unwrap_or_else(|_| panic!("not JSON: {}", describe(output)))
}

/// The JSON of a command that must succeed.
fn ok(output: Output) -> Value {
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    json(&output.stdout, &output)
}

/// The error code of a command the node or the CLI refused (exit 1, nothing on stdout).
fn refused(output: Output) -> String {
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert!(output.stdout.is_empty(), "{}", describe(&output));
    json(&output.stderr, &output)["error"]["code"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The stdout of a conflict or stale outcome (exit 3, `ok: false`).
fn exit3(output: Output) -> Value {
    assert_eq!(output.status.code(), Some(3), "{}", describe(&output));
    let value = json(&output.stdout, &output);
    assert_eq!(value["ok"], false, "{value}");
    value
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wait_listening(port: u16, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "{what} never listened on {port}");
        thread::sleep(Duration::from_millis(50));
    }
}

/// The `javelin-node` binary beside `javelin`. A workspace test run builds it for the node's
/// own tests; a run of this package alone builds it here.
fn node_binary() -> PathBuf {
    let path =
        Path::new(JAVELIN).with_file_name(format!("javelin-node{}", std::env::consts::EXE_SUFFIX));
    if !path.exists() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let mut build = Command::new(cargo);
        build.args([
            "build",
            "--locked",
            "-p",
            "javelin-node",
            "--bin",
            "javelin-node",
        ]);
        if path.parent().and_then(Path::file_name) == Some("release".as_ref()) {
            build.arg("--release");
        }
        assert!(
            build.status().unwrap().success(),
            "cargo build javelin-node"
        );
    }
    path
}

/// Every file under `dir`, except `.javelin`, with its bytes.
fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == ".javelin" {
                continue;
            }
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                walk(root, &path, out);
            } else {
                let name = path.strip_prefix(root).unwrap().to_string_lossy();
                out.insert(name.replace('\\', "/"), fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// The control plane and the repository's node, each a process of the real binaries.
struct Site {
    temp: TempDir,
    control_dir: PathBuf,
    control_url: String,
    admin: String,
    control: Child,
    repository: String,
    node_port: u16,
    node: Option<Child>,
}

impl Site {
    /// `javelin control init`, then `javelin control serve` on loopback.
    fn start() -> Self {
        let temp = TempDir::new().unwrap();
        let control_dir = temp.path().join("control");
        let init = ok(Command::new(JAVELIN)
            .args(["--json", "control", "init"])
            .env("JAVELIN_CONTROL_DIR", &control_dir)
            .env_remove("JAVELIN_CONTROL_URL")
            .output()
            .unwrap());
        let admin = init["admin_token"].as_str().unwrap().to_string();
        let port = free_port();
        let control = Command::new(JAVELIN)
            .args(["control", "serve", &format!("127.0.0.1:{port}")])
            .env("JAVELIN_CONTROL_DIR", &control_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        wait_listening(port, "javelin control serve");
        Self {
            temp,
            control_dir,
            control_url: format!("http://127.0.0.1:{port}"),
            admin,
            control,
            repository: String::new(),
            node_port: 0,
            node: None,
        }
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.temp.path().join(name)
    }

    /// Where the control plane provisioned the self-hosted node's data.
    fn node_dir(&self) -> PathBuf {
        self.control_dir.join("nodes").join(&self.repository)
    }

    fn attach_url(&self) -> String {
        format!("http://127.0.0.1:{}/{}", self.node_port, self.repository)
    }

    /// `javelin-node serve DIR REPOSITORY 127.0.0.1:PORT`, on the same port every time, so
    /// every clone's `attach.json` keeps working across restarts.
    fn start_node(&mut self) {
        assert!(self.node.is_none());
        if self.node_port == 0 {
            self.node_port = free_port();
        }
        let mut node = Command::new(node_binary())
            .arg("serve")
            .arg(self.node_dir())
            .arg(&self.repository)
            .arg(format!("127.0.0.1:{}", self.node_port))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // It prints one line once it listens.
        let mut line = String::new();
        BufReader::new(node.stderr.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert!(line.contains("javelin-node serving"), "node said: {line}");
        self.node = Some(node);
    }

    /// Kills the node process, as a crash or a reboot would.
    fn stop_node(&mut self) {
        let mut node = self.node.take().expect("the node is running");
        node.kill().unwrap();
        node.wait().unwrap();
    }

    /// The operator's stand-in for a membership API: with the node stopped, trust the key that
    /// signs `principal`'s tokens, grant `role`, and require `required_approvals`.
    fn invite(&mut self, principal: &str, role: Role, required_approvals: u32) {
        self.stop_node();
        let dir = self.node_dir();
        let mut keys = auth::read_keys(&dir).unwrap();
        keys.trust(test_keys::KID, test_keys::signing_key(1).verifying_key())
            .unwrap();
        auth::write_keys(&dir, &keys).unwrap();
        let mut permissions = auth::read_permissions(&dir).unwrap().unwrap();
        permissions.epoch += 1;
        permissions
            .roles
            .insert(PrincipalId::parse(principal).unwrap(), role);
        permissions.required_approvals = required_approvals;
        LocalNode::new(self.control_dir.join("nodes"))
            .apply(&permissions)
            .unwrap();
        self.start_node();
    }

    /// A token the node accepts for `principal` on `device`.
    fn token(&self, principal: &str, device: &str) -> String {
        let mut claims = test_keys::claims(
            principal,
            &self.repository,
            COLLABORATOR,
            now(),
            now() + 3600,
            &format!("{principal}-{device}"),
        );
        claims.dev = device.into();
        test_keys::issuer().issue(&claims).unwrap()
    }

    /// The node's head, read from its directory.
    fn node_head(&self) -> javelin::WorldVersion {
        Repository::open(self.node_dir()).unwrap().head().unwrap()
    }
}

impl Drop for Site {
    fn drop(&mut self) {
        for child in self.node.iter_mut().chain([&mut self.control]) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// One person on one device: their own `JAVELIN_HOME`, and either a control-plane sign-in or
/// a token.
struct Person {
    home: PathBuf,
    token: Option<String>,
    control_url: String,
}

impl Person {
    fn command(&self) -> Command {
        let mut command = Command::new(JAVELIN);
        command
            .env("JAVELIN_HOME", &self.home)
            .env("JAVELIN_CONTROL_URL", &self.control_url)
            .env_remove("JAVELIN_ADMIN_TOKEN")
            .env_remove("JAVELIN_CONTROL_DIR");
        match &self.token {
            Some(token) => command.env("JAVELIN_TOKEN", token),
            None => command.env_remove("JAVELIN_TOKEN"),
        };
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().arg("--json").args(args).output().unwrap()
    }

    /// Runs `args` in `clone`.
    fn at(&self, clone: &Path, args: &[&str]) -> Output {
        let mut args = args.to_vec();
        args.extend(["--repo", clone.to_str().unwrap()]);
        self.run(&args)
    }

    /// `javelin login`, approved with `javelin control approve` and the admin token.
    fn login(&self, site: &Site) {
        let mut login = self
            .command()
            .arg("login")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stderr = BufReader::new(login.stderr.take().unwrap());
        let mut line = String::new();
        let code = loop {
            line.clear();
            assert!(
                stderr.read_line(&mut line).unwrap() > 0,
                "login printed no code"
            );
            if let Some((_, code)) = line.trim().rsplit_once("javelin control approve ") {
                break code.to_string();
            }
        };
        let approved = Command::new(JAVELIN)
            .args(["control", "approve", &code])
            .env("JAVELIN_HOME", &self.home)
            .env("JAVELIN_CONTROL_URL", &site.control_url)
            .env("JAVELIN_ADMIN_TOKEN", &site.admin)
            .output()
            .unwrap();
        assert!(approved.status.success(), "{}", describe(&approved));
        assert!(login.wait().unwrap().success(), "login failed");
    }

    /// `javelin attach URL DIR`; the directory need not exist.
    fn attach(&self, site: &Site, name: &str) -> PathBuf {
        let clone = site.dir(name);
        let attached = ok(self.run(&["attach", &site.attach_url(), clone.to_str().unwrap()]));
        assert_eq!(attached["ok"], true, "{attached}");
        clone
    }

    /// Opens a Layer over the clone's World and returns its plain directory.
    fn layer(&self, clone: &Path, id: &str) -> PathBuf {
        let created = ok(self.at(clone, &["workspace", "create", id]));
        PathBuf::from(
            created["workspace"]["root"]
                .as_str()
                .expect("workspace root"),
        )
    }

    fn sync(&self, clone: &Path) -> Value {
        ok(self.at(clone, &["sync"]))
    }
}

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
    let mut site = Site::start();
    let owner = Person {
        home: site.dir("owner-home"),
        token: None,
        control_url: site.control_url.clone(),
    };
    owner.login(&site);
    let created = ok(owner.run(&[
        "repo",
        "create",
        "acme/app",
        "--operation",
        "create-app",
        "--visibility",
        "private",
        "--publish",
        "requests",
    ]));
    site.repository = created["repository"].as_str().unwrap().to_string();
    site.start_node();

    step("owner: attach, import the project, publish World 1");
    let ours = owner.attach(&site, "owner");
    let import = owner.layer(&ours, "import");
    project(&import);
    let opened = ok(owner.at(
        &ours,
        &[
            "request",
            "open",
            "import",
            "--layer",
            "import",
            "--operation",
            "import-open",
        ],
    ));
    let mut imported = paths(&ok(owner.at(&ours, &["request", "show", "import"]))["changes"]);
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
    let published = ok(owner.at(
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
    assert_eq!(published["published"]["number"], 1, "{opened} {published}");
    assert_eq!(owner.sync(&ours)["head"]["number"], 1);

    step("owner: invite cora as a reviewer; Publish now needs one approval");
    site.invite("cora", Role::Review, 1);
    let cora = Person {
        home: site.dir("cora-laptop-home"),
        token: Some(site.token("cora", "cora-laptop")),
        control_url: closed_control_url(),
    };

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
    assert_eq!(site.node_head().number.get(), 1);
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
    assert_eq!(synced["head"]["number"], 2, "{synced}");
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
    assert_eq!(replayed["published"]["number"], 2, "{replayed}");

    step("both clones sync to World 2, byte for byte");
    assert_eq!(cora.sync(&theirs)["head"]["number"], 2);
    let world = same_world(
        &site,
        &[(&owner, &ours), (&cora, &theirs)],
        "after-greeting",
    );
    assert_eq!(world["docs/usage.md"], b"Run `app` to greet the world.\n");

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
    assert_eq!(first["published"]["number"], 3);
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
        3,
        "a conflict publishes nothing"
    );

    step("cora: see the conflict coming, redo the change over World 3, revise");
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
    assert_eq!(second["published"]["number"], 4, "{second}");

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
    let desktop = Person {
        home: site.dir("cora-desktop-home"),
        token: Some(site.token("cora", "cora-desktop")),
        control_url: closed_control_url(),
    };
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
    assert_eq!(third["published"]["number"], 5, "{third}");

    step("every clone syncs and holds the node's World, byte for byte");
    let clones = [(&owner, &ours), (&cora, &theirs), (&desktop, &other)];
    for (person, clone) in clones {
        assert_eq!(person.sync(clone)["head"]["number"], 5);
    }
    let world = same_world(&site, &clones, "final");
    assert_eq!(
        world["NOTES.md"],
        b"- started on the laptop\n- finished on the desktop\n"
    );
    assert_eq!(world["docs/usage.md"], b"Run `app` to greet the world.\n");
    assert!(!world.contains_key("target/debug/app"));
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

/// A control-plane URL with nothing behind it: these people never need one.
fn closed_control_url() -> String {
    format!("http://127.0.0.1:{}", free_port())
}
