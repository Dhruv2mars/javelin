//! Credentials on the wire, end to end with the real binary: the control plane serves its HTTP
//! API, the node serves a private repository behind its gate, and the device is signed in with
//! a refresh token. The CLI attaches with a token it fetched and cached, replays the outbox
//! with the cached token while the control plane is down, stalls with
//! `control_plane_unavailable` once that token has expired, and refreshes it when the control
//! plane is back. Nothing is dropped. `check report --node` carries the reporter token.

use std::{
    fs,
    net::{SocketAddr, TcpListener},
    path::PathBuf,
    process::{Command, Output},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use javelin::{
    FileEntry, LayerId, ObjectId, OperationId, PublishAttribution, RepoPath, Repository,
    SparseChanges, VersionNumber, outbox::Outbox,
};
use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_control::{
    ControlPlane, CreateRequest, Credentials, LocalNode, NodeLink, Poll, Publish, RelyingParty,
};
use javelin_identity::{Scope, test_keys};
use javelin_node::{FileWorldLog, auth::Gate, bind_loopback, serve};
use javelin_protocol::{Message, codec::encode, receiver::WorldLog};
use serde_json::Value;
use tempfile::TempDir;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}

fn layer(s: &str) -> LayerId {
    LayerId::parse(s).unwrap()
}

/// A URL with nothing listening behind it: an unreachable control plane or node.
fn closed_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

struct Deployment {
    temp: TempDir,
    repository: String,
    /// The control plane's HTTP API.
    control_url: String,
    node_url: String,
    /// Layers on the node, each with one checkpoint over World 0: (name, head).
    heads: Vec<(String, ObjectId)>,
}

impl Deployment {
    /// Bootstraps the control plane, signs a device in, creates private `acme/vault`, writes
    /// two Layers on its node, then serves the control plane and the node on loopback. The
    /// node also trusts the public test key, for a CI reporter, and `ci-bot` reads.
    fn start() -> Self {
        let temp = TempDir::new().unwrap();
        let control = temp.path().join("control");
        let plane = ControlPlane::open(&control).unwrap();
        let admin = plane.bootstrap().unwrap();
        let start = plane.device_start(now()).unwrap();
        plane
            .device_approve(&admin, &start.user_code, now())
            .unwrap();
        let Poll::Approved(session) = plane.device_poll(&start.device_code, now()).unwrap() else {
            panic!("the approved code is still pending");
        };
        Credentials {
            principal: session.principal.clone(),
            device: session.device.clone(),
            refresh_token: session.refresh_token.clone(),
        }
        .save(&temp.path().join("home").join("credentials"))
        .unwrap();
        let created = plane
            .create_repository(
                &session.access_token,
                &CreateRequest {
                    owner: "acme".into(),
                    name: "vault".into(),
                    public: false,
                    publish: Publish::Direct,
                    operation: "create-vault".into(),
                },
                now(),
            )
            .unwrap();
        let repository = created.repository.clone();
        let id = RepositoryId::parse(&repository).unwrap();
        let nodes = LocalNode::new(control.join("nodes"));
        let mut keys = plane.node_keys().unwrap();
        keys.trust(test_keys::KID, test_keys::signing_key(1).verifying_key())
            .unwrap();
        nodes.trust_keys(&id, &keys).unwrap();
        nodes
            .apply(&AppliedPermissions {
                repository: id.clone(),
                epoch: 2,
                visibility: Visibility::Private,
                roles: [
                    (
                        PrincipalId::parse(&session.principal).unwrap(),
                        Role::Maintain,
                    ),
                    (PrincipalId::parse("ci-bot").unwrap(), Role::Read),
                ]
                .into_iter()
                .collect(),
                required_approvals: 0,
            })
            .unwrap();
        drop(plane);

        let node_dir = nodes.repository_dir(&id);
        let mut log = FileWorldLog::open(&node_dir).unwrap();
        let mut heads = Vec::new();
        for name in ["p", "q", "r"] {
            log.create_layer(&layer(name), VersionNumber::new(0).unwrap())
                .unwrap();
            let content = log.repository().put_blob(name.as_bytes()).unwrap();
            let changes = SparseChanges::new([(
                RepoPath::parse(&format!("{name}.txt")).unwrap(),
                Some(FileEntry::Regular {
                    content,
                    executable: false,
                }),
            )])
            .unwrap();
            let head = log
                .append(&op(&format!("write-{name}")), &layer(name), &changes)
                .unwrap();
            heads.push((name.to_string(), head.checkpoint().object()));
        }
        let gate = Gate::load(&node_dir, &repository).unwrap();
        let node = bind_loopback("127.0.0.1:0").unwrap();
        let node_url = format!("http://{}", node.local_addr().unwrap());
        let name = repository.clone();
        thread::spawn(move || {
            let mut log = log;
            let _ = serve(&node, &name, &mut log, &gate, None);
        });

        let api = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr: SocketAddr = api.local_addr().unwrap();
        let control_url = format!("http://{addr}");
        thread::spawn(move || {
            // The control plane is not Send, so its server thread opens its own.
            let plane = ControlPlane::open(&control).unwrap();
            let rp = RelyingParty::new(&format!("http://localhost:{}", addr.port())).unwrap();
            let _ = javelin_control::serve(&plane, &rp, &api);
        });
        Self {
            temp,
            repository,
            control_url,
            node_url,
            heads,
        }
    }

    fn home(&self) -> PathBuf {
        self.temp.path().join("home")
    }

    fn project(&self) -> PathBuf {
        let project = self.temp.path().join("project");
        fs::create_dir_all(&project).unwrap();
        project
    }

    fn node_head(&self) -> u64 {
        let dir = LocalNode::new(self.temp.path().join("control").join("nodes"))
            .repository_dir(&RepositoryId::parse(&self.repository).unwrap());
        Repository::open(dir).unwrap().head().unwrap().number.get()
    }

    /// Runs the CLI with this device's home and the control plane at `control_url`.
    fn javelin(&self, control_url: &str, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_javelin"))
            .current_dir(self.temp.path())
            .env("JAVELIN_HOME", self.home())
            .env("JAVELIN_CONTROL_URL", control_url)
            .env_remove("JAVELIN_TOKEN")
            .env_remove("JAVELIN_ADMIN_TOKEN")
            .arg("--json")
            .args(args)
            .output()
            .unwrap()
    }

    fn queue_publish(&self, name: &str) {
        let (_, head) = self.heads.iter().find(|(n, _)| n == name).unwrap();
        let payload = encode(&Message::PublishLayer {
            operation: op(&format!("publish-{name}")),
            layer: layer(name),
            expected: *head,
            base: VersionNumber::new(0).unwrap(),
            attribution: PublishAttribution::Human,
        })
        .unwrap();
        Outbox::open(self.project(), &self.repository)
            .unwrap()
            .enqueue(
                javelin::outbox::Kind::Publish,
                name,
                &op(&format!("publish-{name}")),
                &payload,
            )
            .unwrap();
    }

    fn replay(&self, control_url: &str) -> (Option<i32>, Value) {
        let project = self.project();
        let output = self.javelin(
            control_url,
            &[
                "outbox",
                "replay",
                "--remote",
                &self.repository,
                "--repo",
                project.to_str().unwrap(),
            ],
        );
        (output.status.code(), stdout(&output))
    }

    fn cache(&self) -> Value {
        serde_json::from_slice(&fs::read(self.home().join("tokens.json")).unwrap()).unwrap()
    }

    /// Marks the cached token for the repository as expired.
    fn expire_cache(&self) {
        let mut cache = self.cache();
        cache[&self.repository]["expires_at"] = 0.into();
        fs::write(
            self.home().join("tokens.json"),
            serde_json::to_vec(&cache).unwrap(),
        )
        .unwrap();
    }
}

fn stdout(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout is not JSON: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn error_code(output: &Output) -> String {
    let value: Value = serde_json::from_slice(&output.stderr).unwrap_or_else(|_| {
        panic!(
            "stderr is not JSON: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    value["error"]["code"].as_str().unwrap().to_string()
}

#[test]
fn the_outbox_waits_for_a_credential_and_refreshes_it_through_the_control_plane() {
    let deployment = Deployment::start();
    let down = closed_url();
    let attach_url = format!("{}/{}", deployment.node_url, deployment.repository);
    let project = deployment.project();

    // A private repository: attach fetches a token from the control plane and caches it.
    let attached = deployment.javelin(
        &deployment.control_url,
        &["attach", &attach_url, project.to_str().unwrap()],
    );
    assert_eq!(attached.status.code(), Some(0), "{attached:?}");
    let cached = deployment.cache();
    let token = cached[&deployment.repository]["access_token"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        cached[&deployment.repository]["expires_at"]
            .as_u64()
            .unwrap()
            > now() + 3000
    );
    assert!(
        !fs::read_to_string(deployment.home().join("tokens.json"))
            .unwrap()
            .contains("jav_refresh_"),
        "the refresh token stays in the credentials file"
    );

    // Control plane down, token valid: the node accepts the cached token.
    deployment.queue_publish("p");
    let (code, value) = deployment.replay(&down);
    assert_eq!(code, Some(0), "{value}");
    assert_eq!(value["delivered"][0]["outcome"]["published"], 1);
    assert_eq!(deployment.node_head(), 1);

    // Control plane down, token expired: the record waits, and nothing is sent without one.
    deployment.expire_cache();
    deployment.queue_publish("q");
    let (code, value) = deployment.replay(&down);
    assert_eq!(code, Some(1), "{value}");
    assert_eq!(value["code"], "control_plane_unavailable");
    assert_eq!(value["pending"], 1);
    assert_eq!(value["delivered"].as_array().unwrap().len(), 0);
    assert_eq!(deployment.node_head(), 1);

    // The control plane is back: the token is refreshed and the record replays once.
    let (code, value) = deployment.replay(&deployment.control_url);
    assert_eq!(code, Some(0), "{value}");
    assert_eq!(value["delivered"][0]["outcome"]["published"], 2);
    assert_eq!(value["pending"], 0);
    assert_eq!(deployment.node_head(), 2);
    let renewed = deployment.cache()[&deployment.repository]["access_token"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(renewed, token, "a fresh token was cached");

    // Signed out, nothing cached: the node refuses the anonymous write and the record waits.
    fs::remove_file(deployment.home().join("credentials")).unwrap();
    fs::remove_file(deployment.home().join("tokens.json")).unwrap();
    deployment.queue_publish("r");
    let (code, value) = deployment.replay(&deployment.control_url);
    assert_eq!(code, Some(1), "{value}");
    assert_eq!(value["code"], "unauthorized");
    assert_eq!(value["pending"], 1);
    assert_eq!(deployment.node_head(), 2);
}

#[test]
fn a_stale_cached_token_is_renewed_when_the_node_refuses_it() {
    let deployment = Deployment::start();
    let project = deployment.project();
    let attach_url = format!("{}/{}", deployment.node_url, deployment.repository);
    let attached = deployment.javelin(
        &deployment.control_url,
        &["attach", &attach_url, project.to_str().unwrap()],
    );
    assert_eq!(attached.status.code(), Some(0), "{attached:?}");
    // The cache claims a long life for a token the node does not accept.
    let mut cache = deployment.cache();
    cache[&deployment.repository]["access_token"] = "not.a.token".into();
    fs::write(
        deployment.home().join("tokens.json"),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();
    deployment.queue_publish("p");
    let (code, value) = deployment.replay(&deployment.control_url);
    assert_eq!(code, Some(0), "{value}");
    assert_eq!(deployment.node_head(), 1);
}

#[test]
fn the_control_plane_over_http_creates_and_refuses_when_unreachable() {
    let deployment = Deployment::start();
    let down = closed_url();
    let create = |url: &str, operation: &str| {
        deployment.javelin(
            url,
            &[
                "repo",
                "create",
                "acme/site",
                "--operation",
                operation,
                "--visibility",
                "public",
            ],
        )
    };
    let refused = create(&down, "create-site");
    assert!(!refused.status.success());
    assert_eq!(error_code(&refused), "control_plane_unavailable");

    let created = create(&deployment.control_url, "create-site");
    assert_eq!(created.status.code(), Some(0), "{created:?}");
    let first = stdout(&created);
    assert_eq!(first["visibility"], "public");
    assert_eq!(first["created"], true);
    let again = stdout(&create(&deployment.control_url, "create-site"));
    assert_eq!(again["repository"], first["repository"]);
    assert_eq!(again["created"], false);

    let taken = create(&deployment.control_url, "create-site-2");
    assert_eq!(error_code(&taken), "conflict");

    // Sign-in needs the control plane too.
    let login = deployment.javelin(&down, &["login"]);
    assert_eq!(error_code(&login), "control_plane_unavailable");
}

#[test]
fn check_report_goes_to_a_serving_node_with_the_reporter_token() {
    let deployment = Deployment::start();
    let report = |token: Option<&str>, operation: &str| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_javelin"));
        command
            .env("JAVELIN_HOME", deployment.temp.path().join("ci-home"))
            .env_remove("JAVELIN_TOKEN")
            .args([
                "--json",
                "check",
                "report",
                "--node",
                &deployment.node_url,
                "--repository",
                &deployment.repository,
                "--operation",
                operation,
                "--tree",
                &empty_tree(),
                "--name",
                "ci/unit",
                "--conclusion",
                "success",
                "--completed",
                "1700000000",
            ]);
        if let Some(token) = token {
            command.env("JAVELIN_TOKEN", token);
        }
        command.output().unwrap()
    };
    let t = now();
    let mut claims = test_keys::claims(
        "ci-bot",
        &deployment.repository,
        &[Scope::CheckReport],
        t,
        t + 600,
        "ci-1",
    );
    claims.checks = vec!["ci/*".into()];
    let reporter = test_keys::issuer().issue(&claims).unwrap();

    let anonymous = report(None, "report-anon");
    assert_eq!(anonymous.status.code(), Some(1));
    assert_eq!(stdout(&anonymous)["code"], "unauthorized");

    let accepted = report(Some(&reporter), "report-1");
    assert_eq!(accepted.status.code(), Some(0), "{accepted:?}");
    let first = stdout(&accepted);
    assert_eq!(first["outcome"], "accepted");
    let again = stdout(&report(Some(&reporter), "report-1"));
    assert_eq!(
        again["seq"], first["seq"],
        "a retry replays the stored result"
    );
}

/// The tree of an empty World, which every repository stores.
fn empty_tree() -> String {
    let dir = TempDir::new().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    repo.version(VersionNumber::new(0).unwrap())
        .unwrap()
        .tree
        .to_string()
}
