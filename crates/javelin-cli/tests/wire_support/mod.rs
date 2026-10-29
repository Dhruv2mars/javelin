//! A deployment for end-to-end CLI tests over the wire: the control plane serves its HTTP API,
//! a device is signed in with a refresh token, and the node serves a private repository behind
//! its gate on loopback. The node also trusts the public test key, so a test can act as other
//! principals with `JAVELIN_TOKEN`.

#![allow(dead_code)] // each test binary uses a different subset

use std::{
    fs,
    net::{SocketAddr, TcpListener},
    path::PathBuf,
    process::{Command, Output},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use javelin::{LayerId, ObjectId, OperationId, Repository};
use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_control::{
    ControlPlane, CreateRequest, Credentials, LocalNode, NodeLink, Poll, Publish, RelyingParty,
};
use javelin_identity::{Scope, test_keys};
use javelin_node::{FileWorldLog, auth::Gate, bind_loopback, serve};
use serde_json::Value;
use tempfile::TempDir;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

pub fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}

pub fn layer(s: &str) -> LayerId {
    LayerId::parse(s).unwrap()
}

/// A URL with nothing listening behind it: an unreachable control plane or node.
pub fn closed_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

pub struct Deployment {
    pub temp: TempDir,
    pub admin: String,
    pub repository: String,
    /// The signed-in device's principal, which holds Maintain.
    pub principal: String,
    /// The control plane's HTTP API.
    pub control_url: String,
    pub node_url: String,
    /// What `seed` returned: Layers it wrote on the node, each with its head.
    pub heads: Vec<(String, ObjectId)>,
}

impl Deployment {
    /// Bootstraps the control plane, signs a device in, creates private `acme/vault`, applies
    /// `roles` beside the device's Maintain with `required_approvals`, lets `seed` write to the
    /// node, then serves the control plane and the node on loopback. The node also trusts the
    /// public test key, so [`Deployment::token`] signs in as any principal in `roles`.
    pub fn start_with(
        roles: &[(&str, Role)],
        required_approvals: u32,
        seed: impl FnOnce(&mut FileWorldLog) -> Vec<(String, ObjectId)>,
    ) -> Self {
        Self::start_as(Visibility::Private, roles, required_approvals, seed)
    }

    /// `start_with` for a repository of `visibility`: a public one lets every signed-in
    /// caller in, without a role.
    pub fn start_as(
        visibility: Visibility,
        roles: &[(&str, Role)],
        required_approvals: u32,
        seed: impl FnOnce(&mut FileWorldLog) -> Vec<(String, ObjectId)>,
    ) -> Self {
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
        let mut applied = vec![(
            PrincipalId::parse(&session.principal).unwrap(),
            Role::Maintain,
        )];
        applied.extend(
            roles
                .iter()
                .map(|(name, role)| (PrincipalId::parse(name).unwrap(), *role)),
        );
        nodes
            .apply(&AppliedPermissions {
                repository: id.clone(),
                epoch: 2,
                visibility,
                roles: applied.into_iter().collect(),
                required_approvals,
            })
            .unwrap();
        drop(plane);

        let node_dir = nodes.repository_dir(&id);
        let mut log = FileWorldLog::open(&node_dir).unwrap();
        let heads = seed(&mut log);
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
            admin,
            repository,
            principal: session.principal,
            control_url,
            node_url,
            heads,
        }
    }

    pub fn home(&self) -> PathBuf {
        self.temp.path().join("home")
    }

    /// `URL/REPOSITORY`, as `javelin attach` takes it.
    pub fn attach_url(&self) -> String {
        format!("{}/{}", self.node_url, self.repository)
    }

    /// The node's repository, read directly from its directory.
    pub fn node_repo(&self) -> Repository {
        let dir = LocalNode::new(self.temp.path().join("control").join("nodes"))
            .repository_dir(&RepositoryId::parse(&self.repository).unwrap());
        Repository::open(dir).unwrap()
    }

    pub fn node_head(&self) -> u64 {
        self.node_repo().head().unwrap().number.get()
    }

    /// Runs the CLI with this device's home and the control plane at `control_url`.
    pub fn javelin(&self, control_url: &str, args: &[&str]) -> Output {
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

    /// A token the node accepts for `principal`, signed with the public test key.
    pub fn token(&self, principal: &str, scopes: &[Scope]) -> String {
        let claims = test_keys::claims(
            principal,
            &self.repository,
            scopes,
            now(),
            now() + 3600,
            principal,
        );
        test_keys::issuer().issue(&claims).unwrap()
    }

    /// Runs the CLI as the holder of `token`, with a home of its own and no control plane.
    pub fn javelin_as(&self, token: &str, args: &[&str]) -> Output {
        let home = self.temp.path().join(format!(
            "home-{}",
            &javelin::ObjectId::for_blob(token.as_bytes()).to_string()[..12]
        ));
        fs::create_dir_all(&home).unwrap();
        Command::new(env!("CARGO_BIN_EXE_javelin"))
            .current_dir(self.temp.path())
            .env("JAVELIN_HOME", home)
            .env("JAVELIN_CONTROL_URL", closed_url())
            .env("JAVELIN_TOKEN", token)
            .env_remove("JAVELIN_ADMIN_TOKEN")
            .arg("--json")
            .args(args)
            .output()
            .unwrap()
    }
}

pub fn stdout(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout is not JSON: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

pub fn error_code(output: &Output) -> String {
    let value: Value = serde_json::from_slice(&output.stderr).unwrap_or_else(|_| {
        panic!(
            "stderr is not JSON: {}\nstdout: {}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        )
    });
    value["error"]["code"].as_str().unwrap().to_string()
}
