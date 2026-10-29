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
//! - `state`: `{"provisioned","genesis","permissions","head","frontier","fork_of"}`; the last
//!   three are `null` before provisioning, and `fork_of` is `null` for a repository never forked.
//! - `budget`: an empty body reads, a `budget.json` body (fields absent keep their default)
//!   replaces the owner's budget. `{"budget":{...},"usage":{"day",<budget>:{"used","limit"}}}`;
//!   `409 not_provisioned` before provisioning.
//!
//! A fork copies between two objects (`NodeLink::fork`). The Worker answers its one admin route,
//! `fork`, with three calls here; none of them is routed from outside:
//!
//! - `fork-finished` on the target, `{"source","at"}`: `{"forked":<fork>}` when it was forked
//!   from `source` at `at`, `{"forked":null}` when never forked; any other fork is `409`.
//! - `fork-export` on the source, `{"at","reader"}` (`reader` a role name, or `null` for an
//!   outsider): the fork bundle (`javelin_node::fork::export`), only what that reader may read.
//! - `fork-import` on the target, the bundle as the body: `<fork>`, written in the object's one
//!   transaction after the budget admits it.
//!
//! A fork is `{"origin":{"source","number","version"},"omitted":[trace,...]}`. A refusal is
//! `409 {"error","detail"}` with the protocol's code, `quota_exceeded` among them.
//!
//! A malformed body is `400 invalid_input`. An answer marked `rollback` must not commit: the
//! object throws inside its transaction, so a failed or refused import leaves the target as
//! it was.

use javelin::VersionNumber;
use javelin_collaboration::{RepositoryId, Role};
use javelin_identity::KeySet;
use javelin_node::{
    FileWorldLog,
    admin::{self, ApplyError},
    auth::{self, Gate, LiveGate},
    budget,
    fork::{self, Forked},
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
    /// The call's writes must be rolled back, not committed: it failed, or refused a fork
    /// part-way.
    pub rollback: bool,
}

impl Response {
    fn json(status: u16, value: Value) -> Self {
        Self {
            status,
            body: value.to_string().into_bytes(),
            json: true,
            cors: false,
            rollback: false,
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
            rollback: false,
        }
    }

    /// A protocol refusal as `409 {"error","detail"}`.
    fn refused(error: &ProtocolError) -> Self {
        Self::error(409, &error.code, &error.detail)
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
            "budget" => self.budget(body),
            "fork-finished" => self.fork_finished(body),
            "fork-export" => self.fork_export(body),
            "fork-import" => self.fork_import(body),
            _ => return Response::error(404, "not_found", action),
        };
        answer.unwrap_or_else(|error| Response {
            rollback: true,
            ..Response::error(500, "storage_error", error)
        })
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
                rollback: false,
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

    fn budget(&mut self, body: &[u8]) -> Result<Response, ProtocolError> {
        let replace = if body.is_empty() {
            None
        } else {
            match budget::parse(body) {
                Ok(budget) => Some(budget),
                Err(error) => return Ok(Response::error(400, "invalid_input", error)),
            }
        };
        if !self.storage.provisioned()? {
            return Ok(Response::error(
                409,
                "not_provisioned",
                "provision the repository first",
            ));
        }
        if let Some(budget) = &replace {
            admin::set_budget(&self.storage, budget)?;
        }
        let (log, _) = self.open()?;
        if let Some(budget) = replace {
            log.set_budget(budget);
        }
        let now = log.now();
        let usage = budget::usage(log, now)?;
        let meters: serde_json::Map<String, Value> = usage
            .meters
            .iter()
            .map(|meter| {
                (
                    meter.budget.clone(),
                    json!({ "used": meter.used, "limit": meter.limit }),
                )
            })
            .collect();
        let mut usage_json = Value::Object(meters);
        usage_json["day"] = json!(usage.day);
        Ok(Response::json(
            200,
            json!({ "budget": log.budget(), "usage": usage_json }),
        ))
    }

    fn state(&mut self) -> Result<Response, ProtocolError> {
        let provisioned = self.storage.provisioned()?;
        let (mut genesis, mut head, mut frontier, mut fork_of) =
            (Value::Null, Value::Null, Value::Null, Value::Null);
        if provisioned {
            let (log, _) = self.open()?;
            let zero = log.repo().version(javelin::VersionNumber::new(0)?)?;
            genesis = json!({ "id": zero.id.to_string(), "tree": zero.tree.to_string() });
            head = json!(log.head()?.number.get());
            frontier = json!(log.frontier()?.get());
            if let Some(origin) = fork::origin(log)? {
                fork_of = forked_json(&Forked {
                    omitted: fork::omitted_traces(log)?,
                    origin,
                });
            }
        }
        let permissions = match self.storage.read(auth::PERMISSIONS_FILE)? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| ProtocolError::new("storage", error.to_string()))?,
            None => Value::Null,
        };
        Ok(Response::json(
            200,
            json!({
                "provisioned": provisioned,
                "genesis": genesis,
                "permissions": permissions,
                "head": head,
                "frontier": frontier,
                "fork_of": fork_of,
            }),
        ))
    }
}

impl<S: Storage + Coordination> Node<S> {
    /// Whether this object, the target, already holds the fork, so a retry never reads the
    /// source again. Writes nothing.
    fn fork_finished(&mut self, body: &[u8]) -> Result<Response, ProtocolError> {
        let Some((source, at)) = fork_request(body) else {
            return Ok(Response::error(400, "invalid_input", "needs source and at"));
        };
        if !self.storage.provisioned()? {
            return Ok(not_provisioned());
        }
        let (log, _) = self.open()?;
        Ok(match fork::finished(log, &source, at) {
            Ok(forked) => {
                Response::json(200, json!({ "forked": forked.as_ref().map(forked_json) }))
            }
            Err(refusal) if refusal.code == "storage" => return Err(refusal),
            Err(refusal) => Response::refused(&refusal),
        })
    }

    /// The fork bundle of this object, the source, for `reader` at `at`. Writes nothing.
    fn fork_export(&mut self, body: &[u8]) -> Result<Response, ProtocolError> {
        let request: Option<(VersionNumber, Option<Role>)> = (|| {
            let value: Value = serde_json::from_slice(body).ok()?;
            let at = VersionNumber::new(value.get("at")?.as_u64()?).ok()?;
            let reader = match value.get("reader")? {
                Value::Null => None,
                name => Some(Role::parse(name.as_str()?)?),
            };
            Some((at, reader))
        })();
        let Some((at, reader)) = request else {
            return Ok(Response::error(400, "invalid_input", "needs at and reader"));
        };
        if !self.storage.provisioned()? {
            return Ok(not_provisioned());
        }
        let name = self.name.as_str().to_string();
        let (log, _) = self.open()?;
        Ok(match fork::export(log, &name, at, reader) {
            Ok(bundle) => Response {
                body: bundle,
                ..Response::status(200)
            },
            Err(refusal) if refusal.code == "storage" => return Err(refusal),
            Err(refusal) => Response::refused(&refusal),
        })
    }

    /// Writes the fork a source exported into this object, the target. A refusal after the
    /// first write is rolled back with the rest.
    fn fork_import(&mut self, body: &[u8]) -> Result<Response, ProtocolError> {
        let bundle = match fork::Bundle::decode(body) {
            Ok(bundle) => bundle,
            Err(error) => return Ok(Response::error(400, "invalid_input", error)),
        };
        if bundle.plan().origin().source == self.name.as_str() {
            return Ok(Response::error(
                400,
                "invalid_input",
                "a repository cannot be forked into itself",
            ));
        }
        if !self.storage.provisioned()? {
            return Ok(not_provisioned());
        }
        let (log, _) = self.open()?;
        Ok(match fork::apply(bundle.plan(), &bundle, log) {
            Ok(forked) => Response::json(200, forked_json(&forked)),
            Err(refusal) => Response {
                rollback: true,
                ..if refusal.code == "storage" {
                    Response::error(500, "storage_error", refusal)
                } else {
                    Response::refused(&refusal)
                }
            },
        })
    }
}

fn not_provisioned() -> Response {
    Response::error(409, "not_provisioned", "provision the repository first")
}

/// The `{"source","at"}` of a `fork-finished` body.
fn fork_request(body: &[u8]) -> Option<(String, VersionNumber)> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let source = value.get("source")?.as_str()?.to_string();
    let at = VersionNumber::new(value.get("at")?.as_u64()?).ok()?;
    Some((source, at))
}

/// A fork as the admin routes answer it.
pub fn forked_json(forked: &Forked) -> Value {
    json!({
        "origin": {
            "source": forked.origin.source,
            "number": forked.origin.number.get(),
            "version": forked.origin.version.to_string(),
        },
        "omitted": forked.omitted.iter().map(ToString::to_string).collect::<Vec<_>>(),
    })
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
