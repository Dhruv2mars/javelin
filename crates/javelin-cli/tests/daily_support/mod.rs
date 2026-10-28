//! The daily workflow's deployment: a control plane and a self-hosted node, each a process of the
//! real binaries, and the people who use them. Every product process runs with a failing `git`
//! first on PATH; [`Site::assert_no_git`] checks that none of them ran it.

use std::{
    collections::BTreeMap,
    ffi::OsString,
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

pub const JAVELIN: &str = env!("CARGO_BIN_EXE_javelin");

/// What a collaborator's token carries: read, write their Layers, comment and approve.
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

pub fn step(text: &str) {
    eprintln!("\n== {text}");
}

pub fn describe(output: &Output) -> String {
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
pub fn ok(output: Output) -> Value {
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    json(&output.stdout, &output)
}

/// The error code of a command the node or the CLI refused (exit 1, nothing on stdout).
pub fn refused(output: Output) -> String {
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert!(output.stdout.is_empty(), "{}", describe(&output));
    json(&output.stderr, &output)["error"]["code"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The stdout of a conflict or stale outcome (exit 3, `ok: false`).
pub fn exit3(output: Output) -> Value {
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

/// A control-plane URL with nothing behind it, for people who never need one.
pub fn closed_url() -> String {
    format!("http://127.0.0.1:{}", free_port())
}

fn wait_listening(port: u16, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "{what} never listened on {port}");
        thread::sleep(Duration::from_millis(50));
    }
}

/// The `javelin-node` binary beside `javelin`, built once per test run. Cargo builds only this
/// package's binaries for its tests, so a node left over from an older build would serve an
/// older storage format; the build is a no-op when it is current. It runs outside the `git`
/// stand-in: building is development, not a product workflow.
pub fn node_binary() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let path = Path::new(JAVELIN)
                .with_file_name(format!("javelin-node{}", std::env::consts::EXE_SUFFIX));
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
            let built = build.output().unwrap();
            assert!(built.status.success(), "{}", describe(&built));
            assert!(path.exists(), "no {}", path.display());
            path
        })
        .clone()
}

/// Every file under `dir`, except `.javelin`, with its bytes.
pub fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
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
pub struct Site {
    temp: TempDir,
    /// PATH for product processes: the failing `git` first.
    path: OsString,
    git_marker: PathBuf,
    control_dir: PathBuf,
    pub control_url: String,
    admin: String,
    control: Child,
    pub repository: String,
    node_port: u16,
    node: Option<Child>,
}

impl Site {
    /// The `git` stand-in and its positive control, then `javelin control init` and
    /// `javelin control serve` on loopback.
    pub fn start() -> Self {
        let temp = TempDir::new().unwrap();
        let (path, git_marker) = git_stand_in(temp.path());
        let control_dir = temp.path().join("control");
        let init = ok(Command::new(JAVELIN)
            .args(["--json", "control", "init"])
            .env("PATH", &path)
            .env("JAVELIN_CONTROL_DIR", &control_dir)
            .env_remove("JAVELIN_CONTROL_URL")
            .output()
            .unwrap());
        let admin = init["admin_token"].as_str().unwrap().to_string();
        let port = free_port();
        let control = Command::new(JAVELIN)
            .args(["control", "serve", &format!("127.0.0.1:{port}")])
            .env("PATH", &path)
            .env("JAVELIN_CONTROL_DIR", &control_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        wait_listening(port, "javelin control serve");
        Self {
            temp,
            path,
            git_marker,
            control_dir,
            control_url: format!("http://127.0.0.1:{port}"),
            admin,
            control,
            repository: String::new(),
            node_port: 0,
            node: None,
        }
    }

    pub fn dir(&self, name: &str) -> PathBuf {
        self.temp.path().join(name)
    }

    /// PATH for a product process, with the failing `git` first.
    pub fn path(&self) -> &OsString {
        &self.path
    }

    /// No product process ran `git`.
    pub fn assert_no_git(&self) {
        assert!(!self.git_marker.exists(), "a product process ran git");
    }

    /// Where the control plane provisioned the self-hosted node's data.
    pub fn node_dir(&self) -> PathBuf {
        self.control_dir.join("nodes").join(&self.repository)
    }

    pub fn node_port(&self) -> u16 {
        self.node_port
    }

    pub fn attach_url(&self) -> String {
        format!("http://127.0.0.1:{}/{}", self.node_port, self.repository)
    }

    /// The owner, signed in through the control plane.
    pub fn owner(&self) -> Person {
        Person {
            home: self.dir("owner-home"),
            token: None,
            control_url: self.control_url.clone(),
            path: self.path.clone(),
        }
    }

    /// `principal` on `device`, with a token the node accepts.
    pub fn member(&self, principal: &str, device: &str) -> Person {
        Person {
            home: self.dir(&format!("{principal}-{device}-home")),
            token: Some(self.token(principal, device)),
            control_url: closed_url(),
            path: self.path.clone(),
        }
    }

    /// `javelin-node serve DIR REPOSITORY 127.0.0.1:PORT`, on the same port every time, so
    /// every clone's `attach.json` keeps working across restarts.
    pub fn start_node(&mut self) {
        assert!(self.node.is_none());
        if self.node_port == 0 {
            self.node_port = free_port();
        }
        let mut node = Command::new(node_binary())
            .arg("serve")
            .arg(self.node_dir())
            .arg(&self.repository)
            .arg(format!("127.0.0.1:{}", self.node_port))
            .env("PATH", &self.path)
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
    pub fn stop_node(&mut self) {
        let mut node = self.node.take().expect("the node is running");
        node.kill().unwrap();
        node.wait().unwrap();
    }

    /// The operator's stand-in for a membership API: with the node stopped, trust the key that
    /// signs `principal`'s tokens, grant `role`, and require `required_approvals`.
    pub fn invite(&mut self, principal: &str, role: Role, required_approvals: u32) {
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
    pub fn token(&self, principal: &str, device: &str) -> String {
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
    pub fn node_head(&self) -> javelin::WorldVersion {
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

/// A `git` that leaves a marker and fails, first on PATH, after a positive control proves it
/// runs. Returns that PATH and the marker. Windows has no runner, so there it is PATH as is.
fn git_stand_in(dir: &Path) -> (OsString, PathBuf) {
    let marker = dir.join("git-ran");
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("git-stand-in");
        fs::create_dir(&bin).unwrap();
        let git = bin.join("git");
        fs::write(
            &git,
            format!("#!/bin/sh\n: > '{}'\nexit 99\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).unwrap();
        let path =
            std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&inherited)))
                .unwrap();
        // Linux refuses to run a file another thread's fork still holds open for writing
        // (ETXTBSY) until that child execs, so the positive control retries briefly.
        let control = (0..200)
            .find_map(|_| match Command::new("git").env("PATH", &path).status() {
                Err(error) if error.raw_os_error() == Some(26) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    None
                }
                status => Some(status.unwrap()),
            })
            .expect("the git stand-in stayed busy");
        assert_eq!(control.code(), Some(99), "the git stand-in did not run");
        assert!(marker.exists());
        fs::remove_file(&marker).unwrap();
        (path, marker)
    }
    #[cfg(not(unix))]
    (inherited, marker)
}

/// One person on one device: their own `JAVELIN_HOME`, and either a control-plane sign-in or
/// a token.
pub struct Person {
    pub home: PathBuf,
    token: Option<String>,
    control_url: String,
    path: OsString,
}

impl Person {
    /// `javelin` with this person's environment, and the failing `git` first on PATH.
    pub fn command(&self) -> Command {
        let mut command = Command::new(JAVELIN);
        command
            .env("PATH", &self.path)
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

    pub fn run(&self, args: &[&str]) -> Output {
        self.command().arg("--json").args(args).output().unwrap()
    }

    /// Runs `args` in `clone`.
    pub fn at(&self, clone: &Path, args: &[&str]) -> Output {
        let mut args = args.to_vec();
        args.extend(["--repo", clone.to_str().unwrap()]);
        self.run(&args)
    }

    /// `javelin login`, approved with `javelin control approve` and the admin token.
    pub fn login(&self, site: &Site) {
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
        let approved = self
            .command()
            .args(["control", "approve", &code])
            .env("JAVELIN_ADMIN_TOKEN", &site.admin)
            .output()
            .unwrap();
        assert!(approved.status.success(), "{}", describe(&approved));
        assert!(login.wait().unwrap().success(), "login failed");
    }

    /// `javelin attach URL DIR`; the directory need not exist.
    pub fn attach(&self, site: &Site, name: &str) -> PathBuf {
        let clone = site.dir(name);
        let attached = ok(self.run(&["attach", &site.attach_url(), clone.to_str().unwrap()]));
        assert_eq!(attached["ok"], true, "{attached}");
        clone
    }

    /// Opens a Layer over the clone's World and returns its plain directory.
    pub fn layer(&self, clone: &Path, id: &str) -> PathBuf {
        let created = ok(self.at(clone, &["workspace", "create", id]));
        PathBuf::from(
            created["workspace"]["root"]
                .as_str()
                .expect("workspace root"),
        )
    }

    pub fn sync(&self, clone: &Path) -> Value {
        ok(self.at(clone, &["sync"]))
    }
}
