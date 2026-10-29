//! Outside contributors (`docs/build-contract.md`, Contributions and reviews): a signed-in
//! caller without a role in a public repository works on Layers of its own and opens Publish
//! Requests from them, which a member reviews and publishes.
//!
//! An outsider reads at the public frontier (`disclosure.rs`), and a Layer is work above
//! every version, so its Layers and requests live in a namespace of its own. The node
//! prefixes every Layer and request ID an outsider names with `outside-` and a hash of its
//! principal, and strips the prefix from every reply; its operation IDs get the prefix too.
//! The stored ID records who created the Layer or request: the outsider reaches only its
//! own, and an ID it picks never meets anyone else's, so no reply says whether another
//! Layer, request or operation exists. Only the outsider writes in its namespace: a member's
//! Layer write, direct Publish, new request or revision there is refused as
//! `layer_reserved`. An outsider creates a Layer over a version at or
//! below the frontier, and reads its own Layers' checkpoints over it
//! (`javelin::Reader::with_namespace`); everything above the frontier stays absent.
//!
//! An outsider may create, append to, read and take its Layers, read their checkpoints, and
//! open, revise, comment on, read and list its requests. Stack, Approve, both Publishes and
//! check reports need a role, and are refused with `forbidden` before anything stored is
//! looked up, as every Layer, request and check message is for an anonymous caller. A Layer
//! or request ID that names nothing in the namespace is `forbidden` too, as before: the
//! outsider reaches none but its own.

use javelin::{LayerId, ObjectId, OperationId, Reader};
use javelin_identity::{Need, Scope};
use javelin_protocol::{
    Message, ProtocolError,
    codec::{LayerOutcome, PublishOutcome, RequestId, ReviewOutcome, code},
    receiver::refusal,
};

use crate::{FileWorldLog, auth::Gate};

/// The start of every ID in an outsider's namespace.
const PREFIX: &str = "outside-";
const UNKNOWN_REQUEST: &str = "unknown_request";

/// How a Layer, Publish Request or check message may go on.
pub(crate) enum Admission {
    /// From a member, or not about Layers, requests or checks: served as it is.
    Any,
    /// From an outside contributor, served in its namespace.
    Outside(Namespace),
    /// A member's write in an outsider's namespace, answered with this refusal.
    Reserved(Message),
}

/// One outsider's namespace: `outside-`, 32 hex digits of its principal's hash, and a dot.
pub(crate) struct Namespace(String);

impl Namespace {
    fn of(principal: &str) -> Self {
        let hash = ObjectId::for_blob(format!("outside/{principal}").as_bytes()).to_string();
        Self(format!("{PREFIX}{}.", &hash[..32]))
    }

    /// The caller's reader: an outsider held at the frontier, owning this namespace's Layers.
    pub(crate) fn reader(
        &self,
        log: &FileWorldLog,
        gate: &Gate,
        token: Option<&str>,
        now: u64,
    ) -> Result<Reader, ProtocolError> {
        let grant = gate.check(
            token,
            now,
            Need {
                scope: Scope::Read,
                layer: None,
            },
        )?;
        let reader = crate::disclosure::hold(log.records(), log.repo(), now, gate.reader(&grant))?;
        Ok(reader.with_namespace(self.0.clone()))
    }

    fn layer(&self, layer: LayerId) -> Result<LayerId, ProtocolError> {
        LayerId::parse(&format!("{}{}", self.0, layer.as_str())).map_err(|_| too_long())
    }

    fn operation(&self, operation: OperationId) -> Result<OperationId, ProtocolError> {
        OperationId::parse(&format!("{}{}", self.0, operation.as_str())).map_err(|_| too_long())
    }

    fn request(&self, request: RequestId) -> Result<RequestId, ProtocolError> {
        RequestId::parse(&format!("{}{}", self.0, request.as_str())).map_err(|_| too_long())
    }

    fn bare_layer(&self, layer: LayerId) -> LayerId {
        layer
            .as_str()
            .strip_prefix(&self.0)
            .and_then(|bare| LayerId::parse(bare).ok())
            .unwrap_or(layer)
    }

    fn bare_request(&self, request: RequestId) -> RequestId {
        request
            .as_str()
            .strip_prefix(&self.0)
            .and_then(|bare| RequestId::parse(bare).ok())
            .unwrap_or(request)
    }

    /// `message` with every Layer and request ID it names moved into the namespace.
    pub(crate) fn enter(&self, message: Message) -> Result<Message, ProtocolError> {
        Ok(match message {
            Message::LayerCreate { layer, base } => Message::LayerCreate {
                layer: self.layer(layer)?,
                base,
            },
            Message::LayerAppend {
                operation,
                layer,
                parent,
                checkpoint,
            } => Message::LayerAppend {
                operation: self.operation(operation)?,
                layer: self.layer(layer)?,
                parent,
                checkpoint,
            },
            Message::LayerHead { layer } => Message::LayerHead {
                layer: self.layer(layer)?,
            },
            Message::LayerTake { operation, layer } => Message::LayerTake {
                operation: self.operation(operation)?,
                layer: self.layer(layer)?,
            },
            Message::OpenRequest {
                operation,
                request,
                layer,
                checkpoint,
                base,
            } => Message::OpenRequest {
                operation: self.operation(operation)?,
                request: self.request(request)?,
                layer: self.layer(layer)?,
                checkpoint,
                base,
            },
            Message::Revise {
                operation,
                request,
                expected_revision,
                checkpoint,
                base,
            } => Message::Revise {
                operation: self.operation(operation)?,
                request: self.request(request)?,
                expected_revision,
                checkpoint,
                base,
            },
            Message::Comment {
                operation,
                request,
                revision,
                body,
            } => Message::Comment {
                operation: self.operation(operation)?,
                request: self.request(request)?,
                revision,
                body,
            },
            Message::ReadRequest { request } => Message::ReadRequest {
                request: self.request(request)?,
            },
            Message::ReadReview {
                request,
                comments_from,
            } => Message::ReadReview {
                request: self.request(request)?,
                comments_from,
            },
            Message::ListRequests { after } => Message::ListRequests {
                after: after.map(|after| self.request(after)).transpose()?,
            },
            message => message,
        })
    }

    /// `reply` with the namespace taken off every ID it names, error details included. A
    /// Layer or request the namespace does not hold is refused with `forbidden`, as one
    /// that is not the outsider's: only its own are there.
    pub(crate) fn leave(&self, reply: Message) -> Message {
        match reply {
            Message::LayerState {
                layer,
                base,
                seq,
                head,
                state,
                lease,
            } => Message::LayerState {
                layer: self.bare_layer(layer),
                base,
                seq,
                head,
                state,
                lease,
            },
            Message::RequestState {
                request,
                state,
                revision,
            } => Message::RequestState {
                request: self.bare_request(request),
                state,
                revision,
            },
            Message::RequestList { requests, more } => Message::RequestList {
                requests: requests
                    .into_iter()
                    .map(|mut summary| {
                        summary.request = self.bare_request(summary.request);
                        summary.layer = self.bare_layer(summary.layer);
                        summary
                    })
                    .collect(),
                more,
            },
            Message::ReviewDetail(mut detail) => {
                detail.summary.request = self.bare_request(detail.summary.request);
                detail.summary.layer = self.bare_layer(detail.summary.layer);
                Message::ReviewDetail(detail)
            }
            Message::Error { code, .. }
                if code == refusal::UNKNOWN_LAYER || code == UNKNOWN_REQUEST =>
            {
                Message::Error {
                    code: code::FORBIDDEN.into(),
                    detail: "no Layer or Publish Request of the caller's has this ID".into(),
                }
            }
            Message::Error { code, detail } => Message::Error {
                code,
                detail: detail.replace(&self.0, ""),
            },
            reply => reply,
        }
    }
}

/// Decides how a message about Layers, Publish Requests or check results goes on, before
/// anything stored is looked up. A Layer is work above every version: its state, base and
/// checkpoints, a request's state, or a check's subject could each name a version above the
/// public frontier. A public repository grants every signed-in caller Read, which carries
/// the write scopes, so the role decides, whatever the token carries. The token is checked
/// first, with no Layer named, so that refusal does not depend on stored state either.
pub(crate) fn admit(
    gate: &Gate,
    token: Option<&str>,
    now: u64,
    message: &Message,
) -> Result<Admission, ProtocolError> {
    let (scope, outsiders) = match message {
        Message::LayerHead { .. }
        | Message::ReadRequest { .. }
        | Message::ListRequests { .. }
        | Message::ReadReview { .. } => (Scope::Read, true),
        Message::Get(items) if crate::stack::all_checkpoints(items) => (Scope::Read, true),
        Message::LayerAppend { .. } | Message::LayerTake { .. } | Message::LayerCreate { .. } => {
            (Scope::LayerWrite, true)
        }
        Message::OpenRequest { .. } | Message::Revise { .. } | Message::Comment { .. } => {
            (Scope::ReviewWrite, true)
        }
        Message::LayerStack { .. } => (Scope::LayerWrite, false),
        Message::Approve { .. } => (Scope::ReviewApprove, false),
        Message::PublishRequest { .. } => (Scope::Publish, false),
        // A member's direct Publish is answered by `refuse_direct_publish`.
        Message::PublishLayer { .. } => (Scope::LayerWrite, false),
        Message::CheckReport(_) => (Scope::CheckReport, false),
        _ => return Ok(Admission::Any),
    };
    let grant = gate.check(token, now, Need { scope, layer: None })?;
    if gate.reader(&grant).is_member() {
        return Ok(reserved(message).map_or(Admission::Any, Admission::Reserved));
    }
    match grant.principal {
        Some(principal) if outsiders => Ok(Admission::Outside(Namespace::of(&principal))),
        _ => Err(ProtocolError::new(
            code::FORBIDDEN,
            "this needs a role in the repository; without one, a signed-in caller works on \
             its own Layers and Publish Requests",
        )),
    }
}

/// The refusal of a member's write in an outsider's namespace: Layer writes, a direct
/// Publish, a request opened there or from an outsider's Layer, and a revision of one, so
/// nothing there is ever based above the outsider's frontier.
fn reserved(message: &Message) -> Option<Message> {
    let outside = |id: &str| id.starts_with(PREFIX);
    let refused = || refusal::LAYER_RESERVED.to_string();
    match message {
        Message::LayerCreate { layer, .. }
        | Message::LayerAppend { layer, .. }
        | Message::LayerTake { layer, .. }
        | Message::LayerStack { target: layer, .. }
            if outside(layer.as_str()) =>
        {
            Some(Message::LayerOutcome(LayerOutcome::Refused {
                code: refused(),
            }))
        }
        Message::PublishLayer { layer, .. } if outside(layer.as_str()) => {
            Some(Message::PublishOutcome(PublishOutcome::Refused {
                code: refused(),
            }))
        }
        Message::OpenRequest { request, layer, .. }
            if outside(request.as_str()) || outside(layer.as_str()) =>
        {
            Some(Message::ReviewOutcome(ReviewOutcome::Refused {
                code: refused(),
            }))
        }
        Message::Revise { request, .. } if outside(request.as_str()) => {
            Some(Message::ReviewOutcome(ReviewOutcome::Refused {
                code: refused(),
            }))
        }
        _ => None,
    }
}

fn too_long() -> ProtocolError {
    ProtocolError::new(
        code::INVALID_INPUT,
        "the ID is too long for an outside contributor's namespace",
    )
}
