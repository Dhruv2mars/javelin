//! One repository's node behind HTTP, independent of the runtime: the admin routes the
//! control plane calls with the node's admin token, and protocol version 2. The Worker
//! (`tools/node-cloudflare/src/worker.js`) checks the admin token, routes each repository to
//! its Durable Object and passes requests here; the tests drive the same `Node` natively.
//!
//! Admin routes answer JSON, in the shapes `javelin-node-worker`'s `CloudflareNode` reads:
//!
//! - `provision` `{"genesis":{"id","tree"}}`: creates World 0 once. `{"created":bool}`; a
//!   genesis other than World 0 is `409 genesis_mismatch`.
//! - `permissions` (the `permissions.json` layout): applies them with the gate's epoch rules.
//!   `{"applied":bool,"epoch":n}`; `409 stale_permissions` with the stored epoch as `seen`;
//!   `409 not_provisioned` before provisioning.
//! - `keys` (a `KeySet` as JSON): replaces the keys the node verifies tokens with.
//! - `state`: `{"provisioned","genesis","permissions"}`.
//!
//! A malformed body is `400 invalid_input`.

use javelin_collaboration::RepositoryId;
use javelin_identity::KeySet;
use javelin_node::{
    FileWorldLog,
    admin::{self, ApplyError},
    auth::{self, Gate, LiveGate},
    storage::{Coordination, Storage},
    wire,
};
use javelin_protocol::{ProtocolError, limits::MAX_FRAME};
use serde_json::{Value, json};

/// One HTTP answer.
#[derive(Debug, PartialEq)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    /// The body is JSON; otherwise it is one protocol message.
    pub json: bool,
    /// Any browser origin may read the reply.
    pub cors: bool,
}

impl Response {
    fn json(status: u16, value: Value) -> Self {
        Self {
            status,
            body: value.to_string().into_bytes(),
            json: true,
            cors: false,
        }
    }

    fn error(status: u16, code: &str, detail: impl std::fmt::Display) -> Self {
        Self::json(
            status,
            json!({ "error": code, "detail": detail.to_string() }),
        )
    }

    fn status(status: u16) -> Self {
        Self {
            status,
            body: Vec::new(),
            json: false,
            cors: false,
        }
    }
}

/// The node for repository `name` on `storage`, keeping time by `clock` (Unix seconds),
/// which decides token expiry and the public frontier. The World log is opened on first use
/// and kept, so its caches outlive one request. The gate follows the stored permissions and
/// keys (`LiveGate`), as on the self-hosted node, so a push applies from the next request.
pub struct Node<S> {
    name: RepositoryId,
    storage: S,
    clock: fn() -> u64,
    log: Option<FileWorldLog>,
    gate: LiveGate,
}

impl<S: Storage + Coordination> Node<S> {
    pub fn new(name: RepositoryId, storage: S, clock: fn() -> u64) -> Self {
        Self {
            gate: LiveGate::new(name.as_str()),
            name,
            storage,
            clock,
            log: None,
        }
    }

    /// One admin route. The caller has checked the admin token.
    pub fn admin(&mut self, action: &str, body: &[u8]) -> Response {
        let answer = match action {
            "provision" => self.provision(body),
            "permissions" => self.permissions(body),
            "keys" => self.keys(body),
            "state" => self.state(),
            _ => return Response::error(404, "not_found", action),
        };
        answer.unwrap_or_else(|error| Response::error(500, "storage_error", error))
    }

    /// One protocol version 2 request: `authorization` is the request's `Authorization`
    /// header and `origin` whether it had an `Origin` header. A repository this node never
    /// provisioned is 404, as on a node that does not serve it.
    pub fn v2(&mut self, authorization: Option<&str>, origin: bool, body: &[u8]) -> Response {
        if body.len() > MAX_FRAME {
            return Response::status(413);
        }
        let token = match authorization {
            None => None,
            Some(value) => match wire::bearer(value) {
                Some(token) => Some(token),
                None => return Response::status(400),
            },
        };
        match self.storage.provisioned() {
            Ok(true) => {}
            Ok(false) => return Response::status(404),
            Err(error) => return Response::error(500, "storage_error", error),
        }
        let answer = self.open().and_then(|(log, gate)| {
            let now = log.now();
            wire::handle(log, gate, token.as_deref(), origin, now, body)
        });
        match answer {
            Ok(answer) => Response {
                status: 200,
                body: answer.body,
                json: false,
                cors: answer.cors,
            },
            Err(error) => Response::error(500, "storage_error", error),
        }
    }

    fn open(&mut self) -> Result<(&mut FileWorldLog, &Gate), ProtocolError> {
        if self.log.is_none() {
            let clock = Box::new(self.clock);
            self.log = Some(FileWorldLog::on(&self.storage, &self.storage, clock)?);
        }
        let gate = self.gate.current(&self.storage)?;
        match &mut self.log {
            Some(log) => Ok((log, gate)),
            None => unreachable!("opened above"),
        }
    }

    fn provision(&mut self, body: &[u8]) -> Result<Response, ProtocolError> {
        let Some((id, tree)) = genesis_of(body) else {
            return Ok(Response::error(
                400,
                "invalid_input",
                "genesis needs 64-hex id and tree",
            ));
        };
        let (zero, zero_tree) = admin::genesis();
        if id != zero.to_string() || tree != zero_tree.to_string() {
            return Ok(Response::error(
                409,
                "genesis_mismatch",
                "the genesis is not World 0 of an empty repository",
            ));
        }
        let created = admin::provision(&self.storage)?;
        Ok(Response::json(200, json!({ "created": created })))
    }

    fn permissions(&mut self, body: &[u8]) -> Result<Response, ProtocolError> {
        let permissions = match auth::parse_permissions(body) {
            Ok(permissions) if permissions.repository == self.name => permissions,
            _ => {
                return Ok(Response::error(
                    400,
                    "invalid_input",
                    "malformed permissions",
                ));
            }
        };
        if !self.storage.provisioned()? {
            return Ok(Response::error(
                409,
                "not_provisioned",
                "provision the repository first",
            ));
        }
        let seen = auth::stored_permissions(&self.storage)?.map(|stored| stored.epoch);
        match admin::apply(&self.storage, &permissions) {
            Ok(applied) => Ok(Response::json(
                200,
                json!({ "applied": applied, "epoch": permissions.epoch }),
            )),
            Err(ApplyError::Refused(refusal)) => Ok(Response::json(
                409,
                json!({ "error": refusal.code, "seen": seen }),
            )),
            Err(ApplyError::Storage(error)) => Err(error.into()),
        }
    }

    fn keys(&mut self, body: &[u8]) -> Result<Response, ProtocolError> {
        let Ok(keys) = KeySet::from_json(body) else {
            return Ok(Response::error(400, "invalid_input", "malformed keys"));
        };
        admin::trust(&self.storage, &keys)?;
        Ok(Response::json(200, json!({ "trusted": true })))
    }

    fn state(&mut self) -> Result<Response, ProtocolError> {
        let provisioned = self.storage.provisioned()?;
        let genesis = if provisioned {
            let (log, _) = self.open()?;
            let zero = log.repo().version(javelin::VersionNumber::new(0)?)?;
            json!({ "id": zero.id.to_string(), "tree": zero.tree.to_string() })
        } else {
            Value::Null
        };
        let permissions = match self.storage.read(auth::PERMISSIONS_FILE)? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| ProtocolError::new("storage", error.to_string()))?,
            None => Value::Null,
        };
        Ok(Response::json(
            200,
            json!({ "provisioned": provisioned, "genesis": genesis, "permissions": permissions }),
        ))
    }
}

/// The `(id, tree)` of a provision body, each 64 lowercase hex digits.
fn genesis_of(body: &[u8]) -> Option<(String, String)> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let hex = |field: &str| {
        let text = value.get("genesis")?.get(field)?.as_str()?;
        (text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .then(|| text.to_string())
    };
    Some((hex("id")?, hex("tree")?))
}
