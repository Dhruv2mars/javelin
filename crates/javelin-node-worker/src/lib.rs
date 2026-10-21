//! The managed repository node on Cloudflare (U13, `docs/online-design.md` 4.10, D1, D23).
//!
//! The node runs in `tools/node-cloudflare`: a Worker and one Durable Object per repository.
//! The Rust core does not compile to wasm32 (the `javelin` root crate uses unix-only file
//! operations and bundled SQLite), so D23's fallback applies and the Worker is TypeScript. This
//! crate is the control plane's side of it: [`CloudflareNode`] implements the same
//! [`NodeLink`] pushes that `LocalNode` implements for a self-hosted node, over HTTPS with the
//! node's admin token, and [`CloudflareNode::snapshot`] reads the node's state back.
//!
//! The conformance suite in `tests/common` runs the same assertions against `LocalNode` (in
//! CI) and against a workerd `wrangler dev` node (`tools/node-cloudflare`, `bun run test`).

use std::{collections::BTreeMap, time::Duration};

use javelin::Repository;
use javelin_collaboration::{AppliedPermissions, RepositoryId, Role, Visibility};
use javelin_control::{ControlError, NodeLink};
use javelin_protocol::ProtocolError;
use serde_json::{Value, json};

/// Longest wait for one request, from connect to the last byte.
const TIMEOUT: Duration = Duration::from_secs(10);

/// What a node holds for one repository, in the shape both nodes can report: World 0's
/// `(id, tree)` and the applied permissions as written to `permissions.json`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub genesis: Option<(String, String)>,
    pub permissions: Option<Value>,
}

/// The control plane's `NodeLink` for a Cloudflare managed node at `base`.
pub struct CloudflareNode {
    base: String,
    token: String,
    agent: ureq::Agent,
}

impl CloudflareNode {
    /// `base` is the node's origin, such as `https://javelin-node.example.workers.dev`. Plain
    /// `http` is accepted only for loopback, so the admin token never crosses the network in clear.
    pub fn new(base: &str, token: &str) -> Result<Self, ControlError> {
        let base = base.trim_end_matches('/');
        let loopback = ["http://127.0.0.1:", "http://localhost:"]
            .iter()
            .any(|prefix| base.starts_with(prefix));
        if !base.starts_with("https://") && !loopback {
            return Err(ControlError::InvalidInput(
                "node URL must be https (http only on localhost)".into(),
            ));
        }
        if token.is_empty() {
            return Err(ControlError::Unauthorized("node admin token is empty"));
        }
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .build()
            .into();
        Ok(Self {
            base: base.into(),
            token: token.into(),
            agent,
        })
    }

    /// Reads the node's state for `repository`.
    pub fn snapshot(&self, repository: &RepositoryId) -> Result<Snapshot, ControlError> {
        let url = self.url(repository, "state");
        let answer = self
            .agent
            .get(&url)
            .header("authorization", &self.bearer())
            .call();
        let body = answer_json(answer)?;
        let genesis = match body.get("genesis") {
            Some(Value::Object(genesis)) => {
                Some((string(genesis.get("id"))?, string(genesis.get("tree"))?))
            }
            _ => None,
        };
        let permissions = match body.get("permissions") {
            Some(Value::Null) | None => None,
            Some(value) => Some(value.clone()),
        };
        Ok(Snapshot {
            genesis,
            permissions,
        })
    }

    fn url(&self, repository: &RepositoryId, action: &str) -> String {
        format!("{}/admin/r/{}/{action}", self.base, repository.as_str())
    }

    fn bearer(&self) -> String {
        format!("Bearer {}", self.token)
    }
}

/// Reads a node answer. A 2xx answer is its JSON body. A 409 `stale_permissions` is
/// `ControlError::Refused`, and any other refusal or transport fault is `ControlError::Node`.
fn answer_json(
    answer: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<Value, ControlError> {
    let mut answer =
        answer.map_err(|error| ControlError::Node(format!("node unreachable: {error}")))?;
    let status = answer.status().as_u16();
    let text = answer
        .body_mut()
        .read_to_string()
        .map_err(|error| ControlError::Node(format!("node answer unreadable: {error}")))?;
    let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if (200..300).contains(&status) {
        return Ok(value);
    }
    let error = value
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    if status == 409 && error == "stale_permissions" {
        return Err(ControlError::Refused(ProtocolError::new(
            "stale_permissions",
            "node refused a stale permissions push",
        )));
    }
    Err(ControlError::Node(format!(
        "node answered {status}: {error}"
    )))
}

impl NodeLink for CloudflareNode {
    fn provision(&self, repository: &RepositoryId) -> Result<(), ControlError> {
        let (id, tree) = genesis()?;
        let body = json!({ "genesis": { "id": id, "tree": tree } });
        self.put(&self.url(repository, "provision"), &body)
    }

    fn apply(&self, permissions: &AppliedPermissions) -> Result<(), ControlError> {
        let url = self.url(&permissions.repository, "permissions");
        self.put(&url, &permissions_json(permissions))
    }
}

/// World 0 of an empty repository, as `(id, tree)`. It is the same value a node computes for
/// `Repository::init`, so every node that provisions a repository starts from one genesis.
pub fn genesis() -> Result<(String, String), ControlError> {
    // A process-and-clock name keeps concurrent calls apart; the directory is removed after.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let dir = std::env::temp_dir().join(format!("javelin-genesis-{}-{nanos}", std::process::id()));
    let result = std::fs::create_dir_all(&dir)
        .map_err(|error| ControlError::Storage(error.to_string()))
        .and_then(|()| {
            let repository =
                Repository::init(&dir).map_err(|error| ControlError::Node(error.to_string()))?;
            let head = repository
                .head()
                .map_err(|error| ControlError::Node(error.to_string()))?;
            Ok((head.id.to_string(), head.tree.to_string()))
        });
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// The permissions as the node stores them. The layout matches `LocalNode`'s `permissions.json`.
pub fn permissions_json(permissions: &AppliedPermissions) -> Value {
    let roles: BTreeMap<&str, &str> = permissions
        .roles
        .iter()
        .map(|(principal, role)| (principal.as_str(), role_name(*role)))
        .collect();
    json!({
        "repository": permissions.repository.as_str(),
        "epoch": permissions.epoch,
        "public": permissions.visibility == Visibility::Public,
        "roles": roles,
        "required_approvals": permissions.required_approvals,
    })
}

impl CloudflareNode {
    fn put(&self, url: &str, body: &Value) -> Result<(), ControlError> {
        let answer = self
            .agent
            .put(url)
            .header("authorization", &self.bearer())
            .header("content-type", "application/json")
            .send(body.to_string());
        answer_json(answer).map(|_| ())
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Read => "read",
        Role::Review => "review",
        Role::Maintain => "maintain",
    }
}

fn string(value: Option<&Value>) -> Result<String, ControlError> {
    value
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ControlError::Node("node state is missing a genesis field".into()))
}
