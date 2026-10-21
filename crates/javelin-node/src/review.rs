//! The review partition: author-owned Publish Request revisions and exact approvals
//! (`docs/online-design.md` section 4.8, unit U9).
//!
//! Each request is an append-only, hash-chained list of accepted commands in `node.db`.
//! A request's state is never stored directly. It is rebuilt by replaying those commands
//! with the permissions epoch each was accepted under, so a later grant change does not
//! rewrite past decisions. Only `PublishRequest` touches the World. It publishes the
//! checkpoint of the revision it names, held in a Layer of its own, so the Layer's
//! current head does not change what is published.

use std::{str::FromStr, str::Split};

use javelin::{
    CheckpointId, Error, LayerId, LayerState, ObjectId, OperationId, PublishAttribution,
    Repository, VersionNumber,
};
use javelin_collaboration::{
    Action, AppliedPermissions, Command, CommentBody, PrincipalId, Refusal, RepositoryId,
    RequestId, Review, RevisionRef,
};
use javelin_identity::{Grant, Need, Scope};
use javelin_protocol::{
    Message, ProtocolError,
    codec::{PublishOutcome, RequestStatus, ReviewOutcome, code, decode, encode},
    receiver::refusal,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    auth::Gate,
    world_log::{FileWorldLog, outcome, refused},
};

const UNKNOWN_REQUEST: &str = "unknown_request";
const OPERATION_ID_REUSED: &str = "operation_id_reused";

/// A recorded command's canonical bytes, and the reply it was decided with.
type Recorded = (Vec<u8>, Vec<u8>);

/// Answers one review message: `OpenRequest`, `Revise`, `Approve`, `Comment`,
/// `PublishRequest` or `ReadRequest`. `token` is the caller's bearer token, if any, and
/// `now` is Unix time in seconds. A refused command comes back as the reply's `Refused`
/// code. Authorization failures and unknown requests are protocol errors.
pub fn handle(
    log: &mut FileWorldLog,
    gate: &Gate,
    token: Option<&str>,
    now: u64,
    message: &Message,
) -> Result<Message, ProtocolError> {
    let (repo, records) = log.parts();
    ensure_schema(records)?;
    match message {
        Message::OpenRequest {
            operation,
            request,
            layer,
            checkpoint,
            base,
        } => {
            let (grant, permissions) =
                authorize(gate, token, now, Scope::ReviewWrite, Some(layer.as_str()))?;
            let actor = principal(&grant)?;
            let id = review_id(request)?;
            if repo.layer_state(layer).is_err() {
                return Ok(review_refused(refusal::UNKNOWN_LAYER));
            }
            let revision = match revision_ref(repo, *checkpoint, *base)? {
                Ok(revision) => revision,
                Err(refused_code) => return Ok(review_refused(refused_code)),
            };
            let command = Command {
                operation: operation.clone(),
                actor,
                action: Action::Open { revision },
            };
            let review = restore(records, &permissions.repository, &id)?;
            let decision = decide(records, permissions, &review, &id, &command)?;
            settle_review(
                records,
                &id,
                &command,
                permissions.epoch,
                decision,
                Some(layer.as_str()),
            )
        }
        Message::Revise {
            operation,
            request,
            expected_revision,
            checkpoint,
            base,
        } => {
            let id = review_id(request)?;
            let (grant, permissions, review) =
                target(records, gate, token, now, Scope::ReviewWrite, &id)?;
            let actor = principal(&grant)?;
            let revision = match revision_ref(repo, *checkpoint, *base)? {
                Ok(revision) => revision,
                Err(refused_code) => return Ok(review_refused(refused_code)),
            };
            let command = Command {
                operation: operation.clone(),
                actor,
                action: Action::Revise {
                    expected: *expected_revision,
                    revision,
                },
            };
            let decision = decide(records, permissions, &review, &id, &command)?;
            settle_review(records, &id, &command, permissions.epoch, decision, None)
        }
        Message::Approve {
            operation,
            request,
            revision,
        } => {
            let id = review_id(request)?;
            let (grant, permissions, review) =
                target(records, gate, token, now, Scope::ReviewApprove, &id)?;
            let actor = principal(&grant)?;
            // Approval pins the revision's content as it is at this index.
            let Some(pinned) = pinned(&review, *revision) else {
                return Ok(review_refused(Refusal::UnknownRevision.code()));
            };
            let command = Command {
                operation: operation.clone(),
                actor,
                action: Action::Approve {
                    index: *revision,
                    revision: pinned,
                },
            };
            let decision = decide(records, permissions, &review, &id, &command)?;
            settle_review(records, &id, &command, permissions.epoch, decision, None)
        }
        Message::Comment {
            operation,
            request,
            revision,
            body,
        } => {
            let id = review_id(request)?;
            let (grant, permissions, review) =
                target(records, gate, token, now, Scope::ReviewWrite, &id)?;
            let actor = principal(&grant)?;
            let Ok(body) = CommentBody::parse(body) else {
                return Ok(review_refused(code::INVALID_INPUT));
            };
            let command = Command {
                operation: operation.clone(),
                actor,
                action: Action::Comment {
                    index: *revision,
                    body,
                },
            };
            let decision = decide(records, permissions, &review, &id, &command)?;
            settle_review(records, &id, &command, permissions.epoch, decision, None)
        }
        Message::PublishRequest {
            operation,
            request,
            revision,
            base,
        } => {
            let id = review_id(request)?;
            let (grant, permissions, review) =
                target(records, gate, token, now, Scope::Publish, &id)?;
            let actor = principal(&grant)?;
            let Some(pinned) = pinned(&review, *revision) else {
                return Ok(publish_refused(Refusal::UnknownRevision.code()));
            };
            if pinned.base() != *base {
                return Ok(publish_refused(code::INVALID_INPUT));
            }
            let command = Command {
                operation: operation.clone(),
                actor,
                action: Action::Publish {
                    index: *revision,
                    revision: pinned,
                },
            };
            match decide(records, permissions, &review, &id, &command)? {
                Decision::Replayed(reply) => Ok(reply),
                Decision::Refused(refused_code) => Ok(publish_refused(refused_code)),
                Decision::Accepted(_) => {
                    let reply = publish_checkpoint(repo, operation, &id, *revision, &pinned)?;
                    if matches!(reply, PublishOutcome::Published { .. }) {
                        let message = Message::PublishOutcome(reply.clone());
                        persist(records, &id, &command, permissions.epoch, &message, None)?;
                    }
                    Ok(Message::PublishOutcome(reply))
                }
            }
        }
        Message::ReadRequest { request } => {
            let id = review_id(request)?;
            let layer = request_layer(records, &id)?;
            let (_, permissions) = authorize(gate, token, now, Scope::Read, layer.as_deref())?;
            if layer.is_none() {
                return Err(unknown_request());
            }
            let review = restore(records, &permissions.repository, &id)?;
            Ok(Message::RequestState {
                request: request.clone(),
                state: status(review.state()),
                revision: review.current().map_or(0, |current| current.index),
            })
        }
        _ => Err(ProtocolError::new("malformed", "not a review message")),
    }
}

/// The outcome of applying one command against the stored state.
enum Decision {
    /// The same operation was already accepted; this is its recorded reply.
    Replayed(Message),
    Refused(&'static str),
    /// Accepted: the request's state after the command.
    Accepted(Review),
}

/// Looks up the operation first. A recorded operation replays its reply when the input
/// is identical, and is refused as reused otherwise. Only then does the state decide.
fn decide(
    conn: &Connection,
    permissions: &AppliedPermissions,
    review: &Review,
    id: &RequestId,
    command: &Command,
) -> Result<Decision, ProtocolError> {
    if let Some((recorded, reply)) = recorded(conn, &command.operation)? {
        return Ok(if recorded == encode_command(id, command) {
            Decision::Replayed(decode(&reply)?)
        } else {
            Decision::Refused(OPERATION_ID_REUSED)
        });
    }
    Ok(match review.apply(command, permissions) {
        Ok(applied) => Decision::Accepted(applied.review),
        Err(refusal) => Decision::Refused(refusal.code()),
    })
}

/// Records an accepted command and returns its reply. A refused command records nothing,
/// so its operation ID stays free for a corrected retry.
fn settle_review(
    conn: &mut Connection,
    id: &RequestId,
    command: &Command,
    epoch: u64,
    decision: Decision,
    layer: Option<&str>,
) -> Result<Message, ProtocolError> {
    match decision {
        Decision::Replayed(reply) => Ok(reply),
        Decision::Refused(refused_code) => Ok(review_refused(refused_code)),
        Decision::Accepted(next) => {
            let reply = Message::ReviewOutcome(ReviewOutcome::Accepted {
                revision: next.current().map_or(0, |current| current.index),
            });
            persist(conn, id, command, epoch, &reply, layer)?;
            Ok(reply)
        }
    }
}

/// Publishes the checkpoint of revision `index`, into a Layer named for that revision.
/// A retry after a lost reply, or after a crash between the publish and its record,
/// finds the receipt by operation and answers with the same version.
fn publish_checkpoint(
    repo: &mut Repository,
    operation: &OperationId,
    id: &RequestId,
    index: u32,
    revision: &RevisionRef,
) -> Result<PublishOutcome, ProtocolError> {
    let layer = LayerId::parse(&format!(
        "review-{}",
        ObjectId::for_blob(format!("{id}/{index}").as_bytes())
    ))?;
    if let Some(receipt) = repo.publication(operation)? {
        if receipt.expected.layer() != &layer
            || receipt.expected.checkpoint().object() != revision.checkpoint()
        {
            return Ok(refused(OPERATION_ID_REUSED));
        }
        return outcome(receipt.outcome);
    }
    let head = match repo.layer_state(&layer) {
        Ok(LayerState::Active { head }) => head,
        Ok(LayerState::Published { .. }) => return Ok(refused(code::LAYER_PUBLISHED)),
        Ok(LayerState::Discarded { .. }) => return Ok(refused(refusal::LAYER_DISCARDED)),
        Err(Error::InvalidInput(_)) => {
            let checkpoint = CheckpointId::parse(&revision.checkpoint().to_string())?;
            repo.clone_layer(&layer, checkpoint)?
        }
        Err(error) => return Err(error.into()),
    };
    match repo.publish_layer(operation, &head, &PublishAttribution::Human, |_, _| Ok(())) {
        Ok(receipt) => outcome(receipt.outcome),
        Err(error) => match error.code() {
            "operation_id_reused"
            | "missing_agent_trace"
            | "layer_published"
            | "missing_object"
            | "invalid_input" => Ok(refused(error.code())),
            _ => Err(error.into()),
        },
    }
}

/// Reads a revision's checkpoint and its tree root. Refusals come back as a code, so the
/// caller can reply without recording anything.
fn revision_ref(
    repo: &mut Repository,
    checkpoint: ObjectId,
    base: VersionNumber,
) -> Result<Result<RevisionRef, &'static str>, ProtocolError> {
    let id = CheckpointId::parse(&checkpoint.to_string())?;
    let Ok(stored) = repo.read_checkpoint(id) else {
        return Ok(Err("missing_object"));
    };
    if stored.base().version() != base {
        return Ok(Err(code::INVALID_INPUT));
    }
    let tree = repo.put_snapshot(&repo.checkpoint_manifest(id)?)?;
    Ok(Ok(RevisionRef::new(base, checkpoint, tree, None)))
}

/// Checks the gate for `scope` on `layer`. The permissions returned are the ones currently
/// applied, which is what the state machine checks against.
fn authorize<'g>(
    gate: &'g Gate,
    token: Option<&str>,
    now: u64,
    scope: Scope,
    layer: Option<&str>,
) -> Result<(Grant, &'g AppliedPermissions), ProtocolError> {
    let grant = gate.check(token, now, Need { scope, layer })?;
    let permissions = gate
        .permissions()
        .ok_or_else(|| ProtocolError::new("forbidden", "no permissions are applied yet"))?;
    Ok((grant, permissions))
}

/// Authorizes a caller for an existing request and rebuilds its state. The request's Layer
/// scopes the token, so a Layer-limited token cannot touch another Layer's requests.
fn target<'g>(
    conn: &Connection,
    gate: &'g Gate,
    token: Option<&str>,
    now: u64,
    scope: Scope,
    id: &RequestId,
) -> Result<(Grant, &'g AppliedPermissions, Review), ProtocolError> {
    let layer = request_layer(conn, id)?;
    let (grant, permissions) = authorize(gate, token, now, scope, layer.as_deref())?;
    if layer.is_none() {
        return Err(unknown_request());
    }
    let review = restore(conn, &permissions.repository, id)?;
    Ok((grant, permissions, review))
}

/// Rebuilds a request's state. Each stored command is replayed under the epoch it was
/// accepted with, without re-checking roles. The hash chain must verify.
fn restore(
    conn: &Connection,
    repository: &RepositoryId,
    id: &RequestId,
) -> Result<Review, ProtocolError> {
    let mut review = Review::new(repository.clone(), id.clone());
    let mut prev = String::new();
    let mut statement = conn
        .prepare(
            "SELECT epoch, command, reply, chain FROM review_rows WHERE request = ?1 ORDER BY seq",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map([id.as_str()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(storage)?;
    for row in rows {
        let (epoch, command, reply, chain) = row.map_err(storage)?;
        let epoch = u64::try_from(epoch).map_err(|_| storage_detail("negative epoch"))?;
        if chain != link(&prev, epoch, &command, &reply) {
            return Err(storage_detail("review chain does not verify"));
        }
        let (stored, command) = decode_command(&command)?;
        if stored != *id {
            return Err(storage_detail("review row belongs to another request"));
        }
        review = review
            .replay(&command, epoch)
            .map_err(|refusal| storage_detail(refusal.code()))?;
        prev = chain;
    }
    Ok(review)
}

/// Appends one accepted command and its reply. A new request also records its Layer.
fn persist(
    conn: &mut Connection,
    id: &RequestId,
    command: &Command,
    epoch: u64,
    reply: &Message,
    layer: Option<&str>,
) -> Result<(), ProtocolError> {
    let key = encode_command(id, command);
    let reply = encode(reply)?;
    let tx = conn.transaction().map_err(storage)?;
    if let Some(layer) = layer {
        tx.execute(
            "INSERT INTO review_requests (request, layer) VALUES (?1, ?2)",
            params![id.as_str(), layer],
        )
        .map_err(storage)?;
    }
    let (seq, prev) = tx
        .query_row(
            "SELECT seq, chain FROM review_rows WHERE request = ?1 ORDER BY seq DESC LIMIT 1",
            [id.as_str()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(storage)?
        .unwrap_or((-1, String::new()));
    let chain = link(&prev, epoch, &key, &reply);
    tx.execute(
        "INSERT INTO review_rows (request, seq, operation, epoch, command, reply, chain)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id.as_str(),
            seq + 1,
            command.operation.as_str(),
            epoch as i64,
            key,
            reply,
            chain,
        ],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)
}

/// The hash that links a row to the rows before it.
fn link(prev: &str, epoch: u64, command: &[u8], reply: &[u8]) -> String {
    let mut input = prev.as_bytes().to_vec();
    input.extend_from_slice(&epoch.to_be_bytes());
    input.extend_from_slice(command);
    input.extend_from_slice(reply);
    ObjectId::for_blob(&input).to_string()
}

fn recorded(conn: &Connection, operation: &OperationId) -> Result<Option<Recorded>, ProtocolError> {
    conn.query_row(
        "SELECT command, reply FROM review_rows WHERE operation = ?1",
        [operation.as_str()],
        |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
    )
    .optional()
    .map_err(storage)
}

fn request_layer(conn: &Connection, id: &RequestId) -> Result<Option<String>, ProtocolError> {
    conn.query_row(
        "SELECT layer FROM review_requests WHERE request = ?1",
        [id.as_str()],
        |row| row.get(0),
    )
    .optional()
    .map_err(storage)
}

fn ensure_schema(conn: &Connection) -> Result<(), ProtocolError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS review_requests (
            request TEXT PRIMARY KEY,
            layer TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS review_rows (
            request TEXT NOT NULL,
            seq INTEGER NOT NULL,
            operation TEXT NOT NULL UNIQUE,
            epoch INTEGER NOT NULL,
            command BLOB NOT NULL,
            reply BLOB NOT NULL,
            chain TEXT NOT NULL,
            PRIMARY KEY (request, seq)
        );",
    )
    .map_err(storage)
}

/// The canonical bytes of a command: one space-separated line, so a stored command
/// compares equal to a retry only when every field does.
fn encode_command(id: &RequestId, command: &Command) -> Vec<u8> {
    let action = match &command.action {
        Action::Open { revision } => format!("open {}", revision_fields(revision)),
        Action::Revise { expected, revision } => {
            format!("revise {expected} {}", revision_fields(revision))
        }
        Action::Approve { index, revision } => {
            format!("approve {index} {}", revision_fields(revision))
        }
        Action::Comment { index, body } => format!(
            "comment {} {}",
            index.map_or("-".to_string(), |n| n.to_string()),
            hex(body.as_str().as_bytes())
        ),
        Action::Publish { index, revision } => {
            format!("publish {index} {}", revision_fields(revision))
        }
        Action::Close => "close".to_string(),
    };
    format!("{id} {} {} {action}", command.operation, command.actor).into_bytes()
}

fn revision_fields(revision: &RevisionRef) -> String {
    format!(
        "{} {} {} {}",
        revision.base().get(),
        revision.checkpoint(),
        revision.tree(),
        revision
            .context()
            .map_or("-".to_string(), |context| context.to_string())
    )
}

fn decode_command(bytes: &[u8]) -> Result<(RequestId, Command), ProtocolError> {
    let text = std::str::from_utf8(bytes).map_err(|_| malformed_record())?;
    let mut tokens = text.split(' ');
    let id = RequestId::parse(next(&mut tokens)?).map_err(|_| malformed_record())?;
    let operation = OperationId::parse(next(&mut tokens)?).map_err(|_| malformed_record())?;
    let actor = PrincipalId::parse(next(&mut tokens)?).map_err(|_| malformed_record())?;
    let action = match next(&mut tokens)? {
        "open" => Action::Open {
            revision: revision(&mut tokens)?,
        },
        "revise" => {
            let expected = number(&mut tokens)?;
            Action::Revise {
                expected,
                revision: revision(&mut tokens)?,
            }
        }
        "approve" => {
            let index = number(&mut tokens)?;
            Action::Approve {
                index,
                revision: revision(&mut tokens)?,
            }
        }
        "publish" => {
            let index = number(&mut tokens)?;
            Action::Publish {
                index,
                revision: revision(&mut tokens)?,
            }
        }
        "comment" => {
            let index = match next(&mut tokens)? {
                "-" => None,
                text => Some(text.parse().map_err(|_| malformed_record())?),
            };
            let bytes = unhex(next(&mut tokens)?).ok_or_else(malformed_record)?;
            let body = String::from_utf8(bytes).map_err(|_| malformed_record())?;
            Action::Comment {
                index,
                body: CommentBody::parse(&body).map_err(|_| malformed_record())?,
            }
        }
        "close" => Action::Close,
        _ => return Err(malformed_record()),
    };
    Ok((
        id,
        Command {
            operation,
            actor,
            action,
        },
    ))
}

fn revision(tokens: &mut Split<'_, char>) -> Result<RevisionRef, ProtocolError> {
    let base = VersionNumber::new(number::<u64>(tokens)?).map_err(|_| malformed_record())?;
    let checkpoint = object(next(tokens)?)?;
    let tree = object(next(tokens)?)?;
    let context = match next(tokens)? {
        "-" => None,
        text => Some(object(text)?),
    };
    Ok(RevisionRef::new(base, checkpoint, tree, context))
}

fn next<'a>(tokens: &mut Split<'a, char>) -> Result<&'a str, ProtocolError> {
    tokens.next().ok_or_else(malformed_record)
}

fn number<T: FromStr>(tokens: &mut Split<'_, char>) -> Result<T, ProtocolError> {
    next(tokens)?.parse().map_err(|_| malformed_record())
}

fn object(text: &str) -> Result<ObjectId, ProtocolError> {
    ObjectId::parse(text).map_err(|_| malformed_record())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    (0..text.len())
        .step_by(2)
        .map(|i| {
            text.get(i..i + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
        })
        .collect()
}

fn review_id(id: &javelin_protocol::codec::RequestId) -> Result<RequestId, ProtocolError> {
    RequestId::parse(id.as_str())
        .map_err(|_| ProtocolError::new("malformed", "invalid Publish Request ID"))
}

fn principal(grant: &Grant) -> Result<PrincipalId, ProtocolError> {
    let name = grant
        .principal
        .as_deref()
        .ok_or_else(|| ProtocolError::new("unauthorized", "a token is required"))?;
    PrincipalId::parse(name)
        .map_err(|_| ProtocolError::new("unauthorized", "token subject is not an opaque ID"))
}

/// The revision at `index`, as it was submitted. Revisions are immutable, so the index
/// pins the content.
fn pinned(review: &Review, index: u32) -> Option<RevisionRef> {
    review
        .revisions()
        .iter()
        .find(|revision| revision.index == index)
        .map(|revision| revision.revision)
}

fn status(state: javelin_collaboration::RequestState) -> RequestStatus {
    match state {
        javelin_collaboration::RequestState::Open => RequestStatus::Open,
        javelin_collaboration::RequestState::Published => RequestStatus::Published,
        javelin_collaboration::RequestState::Closed => RequestStatus::Closed,
    }
}

fn review_refused(refused_code: &str) -> Message {
    Message::ReviewOutcome(ReviewOutcome::Refused {
        code: refused_code.into(),
    })
}

fn publish_refused(refused_code: &str) -> Message {
    Message::PublishOutcome(refused(refused_code))
}

fn unknown_request() -> ProtocolError {
    ProtocolError::new(UNKNOWN_REQUEST, "no such Publish Request")
}

fn malformed_record() -> ProtocolError {
    storage_detail("malformed review record")
}

fn storage(error: rusqlite::Error) -> ProtocolError {
    storage_detail(&error.to_string())
}

fn storage_detail(detail: &str) -> ProtocolError {
    ProtocolError::new("storage", detail)
}
