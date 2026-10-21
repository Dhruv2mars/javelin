//! The World log's commit transaction on a Durable Object (U12, `docs/online-design.md`
//! 4.5). `DoLog` is the node's client for one repository's log object in
//! `tools/spike-do`: it reads the head, creates Layers and commits a composed version.
//!
//! The object orders and records; the caller composes. A commit names the head its version
//! was composed on, and the object refuses it as `HeadMoved` when another commit landed
//! first, so the caller re-prepares on the new head. The digest is the object ID of the
//! encoded `PublishLayer`: the same operation with another request is `operation_id_reused`.
//!
//! This is not yet a `WorldLog`. The trait's `repository()` returns the file-backed
//! `Repository`, which the object cannot host. U13 runs composition inside the Worker and
//! implements the trait there.

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::Duration,
};

use javelin::{LayerId, ObjectId, OperationId, PublishAttribution, VersionNumber};
use javelin_protocol::{
    Message, ProtocolError,
    codec::{PublishOutcome, encode},
};
use serde_json::{Value, json};

/// Longest wait for one connect, one write or one read.
const TIMEOUT: Duration = Duration::from_secs(10);

/// One `PublishLayer` request, as the node received it.
pub struct Publish<'a> {
    pub operation: &'a OperationId,
    pub layer: &'a LayerId,
    pub expected: ObjectId,
    pub base: VersionNumber,
    pub attribution: &'a PublishAttribution,
}

/// The version the caller composed. Its number is the head the commit names, plus one.
pub struct ComposedVersion {
    pub id: ObjectId,
    pub tree: ObjectId,
}

/// What the object decided for one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitReply {
    Published {
        number: VersionNumber,
        id: ObjectId,
        tree: ObjectId,
        replayed: bool,
    },
    Refused {
        code: String,
    },
    LayerStale {
        head: ObjectId,
    },
    /// Another commit advanced the World first. The caller re-prepares on `head`.
    HeadMoved {
        head: VersionNumber,
    },
}

impl CommitReply {
    /// The wire outcome, or `None` when the caller must re-prepare.
    pub fn into_outcome(self) -> Option<PublishOutcome> {
        Some(match self {
            Self::Published {
                number, id, tree, ..
            } => PublishOutcome::Published { number, id, tree },
            Self::Refused { code } => PublishOutcome::Refused { code },
            Self::LayerStale { head } => PublishOutcome::LayerStale { head },
            Self::HeadMoved { .. } => return None,
        })
    }
}

/// The client for one repository's log object on a loopback address.
pub struct DoLog {
    addr: SocketAddr,
    name: String,
}

impl DoLog {
    pub fn new(addr: SocketAddr, name: impl Into<String>) -> Self {
        Self {
            addr,
            name: name.into(),
        }
    }

    /// The head's number and ID. Genesis is version 0.
    pub fn head(&self) -> Result<(VersionNumber, ObjectId), ProtocolError> {
        let value = self.exchange("GET", "head", None)?;
        Ok((version(&value["n"])?, object(&value["id"])?))
    }

    /// Creates an active Layer whose checkpoint is `head`.
    pub fn create_layer(&self, layer: &LayerId, head: ObjectId) -> Result<(), ProtocolError> {
        let body = json!({ "layer": layer.as_str(), "head": head.to_string() });
        self.exchange("POST", "layers", Some(&body)).map(|_| ())
    }

    /// Commits `version` as the next World version, composed on `head`.
    pub fn commit(
        &self,
        publish: &Publish,
        head: VersionNumber,
        version: &ComposedVersion,
    ) -> Result<CommitReply, ProtocolError> {
        let body = json!({
            "operation": publish.operation.as_str(),
            "digest": digest(publish)?.to_string(),
            "layer": publish.layer.as_str(),
            "expectedLayerHead": publish.expected.to_string(),
            "expectedWorld": head.get(),
            "version": { "id": version.id.to_string(), "tree": version.tree.to_string() },
        });
        reply(&self.exchange("POST", "commit", Some(&body))?)
    }

    /// One HTTP/1.1 exchange with `Connection: close`, read to EOF. The object's replies are
    /// JSON with a length, so chunked framing is refused rather than decoded.
    fn exchange(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, ProtocolError> {
        let payload = body.map(Value::to_string).unwrap_or_default();
        let mut stream = TcpStream::connect_timeout(&self.addr, TIMEOUT)
            .map_err(|e| unavailable(&e.to_string()))?;
        stream
            .set_read_timeout(Some(TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(TIMEOUT)))
            .map_err(|e| unavailable(&e.to_string()))?;
        write!(
            stream,
            "{method} /r/{}/{path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            self.name,
            self.addr,
            payload.len(),
        )
        .map_err(|e| unavailable(&e.to_string()))?;
        let mut raw = Vec::new();
        stream
            .read_to_end(&mut raw)
            .map_err(|e| unavailable(&e.to_string()))?;
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| unavailable("reply has no header end"))?;
        let (head, body) = (&raw[..split], &raw[split + 4..]);
        let head = String::from_utf8_lossy(head).to_ascii_lowercase();
        if head.contains("transfer-encoding: chunked") {
            return Err(unavailable("chunked replies are not supported"));
        }
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok());
        let value: Value = serde_json::from_slice(body).map_err(|e| unavailable(&e.to_string()))?;
        match status {
            Some(200) => Ok(value),
            // The object reports its own failures as JSON with a 500 status.
            _ => Err(ProtocolError::new(
                "storage",
                value["detail"].as_str().unwrap_or("log object failed"),
            )),
        }
    }
}

/// Digest of the request: the object ID of its encoded `PublishLayer`. The head it was
/// composed on is not part of it, so a retry on a moved head keeps the same digest.
fn digest(publish: &Publish) -> Result<ObjectId, ProtocolError> {
    let bytes = encode(&Message::PublishLayer {
        operation: publish.operation.clone(),
        layer: publish.layer.clone(),
        expected: publish.expected,
        base: publish.base,
        attribution: publish.attribution.clone(),
    })?;
    Ok(ObjectId::for_blob(&bytes))
}

fn reply(value: &Value) -> Result<CommitReply, ProtocolError> {
    Ok(match value["outcome"].as_str() {
        Some("published") => CommitReply::Published {
            number: version(&value["n"])?,
            id: object(&value["id"])?,
            tree: object(&value["tree"])?,
            replayed: value["replayed"].as_bool().unwrap_or(false),
        },
        Some("refused") => CommitReply::Refused {
            code: text(&value["code"])?.into(),
        },
        Some("layer_stale") => CommitReply::LayerStale {
            head: object(&value["head"])?,
        },
        Some("head_moved") => CommitReply::HeadMoved {
            head: version(&value["head"]["n"])?,
        },
        _ => return Err(unavailable("unrecognised commit reply")),
    })
}

fn version(value: &Value) -> Result<VersionNumber, ProtocolError> {
    let number = value
        .as_u64()
        .ok_or_else(|| unavailable("version number is not an integer"))?;
    VersionNumber::new(number).map_err(|e| unavailable(&e.to_string()))
}

fn object(value: &Value) -> Result<ObjectId, ProtocolError> {
    ObjectId::parse(text(value)?).map_err(|e| unavailable(&e.to_string()))
}

fn text(value: &Value) -> Result<&str, ProtocolError> {
    value
        .as_str()
        .ok_or_else(|| unavailable("expected a string in the object's reply"))
}

fn unavailable(detail: &str) -> ProtocolError {
    ProtocolError::new("node_unavailable", detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> ObjectId {
        ObjectId::for_blob(s.as_bytes())
    }

    #[test]
    fn replies_map_to_the_wire_outcomes() {
        let published = reply(&json!({
            "outcome": "published", "replayed": true, "n": 3,
            "id": id("v").to_string(), "tree": id("t").to_string(),
        }))
        .unwrap();
        assert_eq!(
            published.into_outcome(),
            Some(PublishOutcome::Published {
                number: VersionNumber::new(3).unwrap(),
                id: id("v"),
                tree: id("t"),
            })
        );
        assert_eq!(
            reply(&json!({ "outcome": "refused", "code": "operation_id_reused" }))
                .unwrap()
                .into_outcome(),
            Some(PublishOutcome::Refused {
                code: "operation_id_reused".into()
            })
        );
        assert_eq!(
            reply(&json!({ "outcome": "layer_stale", "head": id("c").to_string() }))
                .unwrap()
                .into_outcome(),
            Some(PublishOutcome::LayerStale { head: id("c") })
        );
    }

    #[test]
    fn a_moved_head_is_not_an_outcome() {
        let moved = reply(&json!({
            "outcome": "head_moved",
            "head": { "n": 7, "id": id("h").to_string(), "tree": id("t").to_string() },
        }))
        .unwrap();
        assert_eq!(
            moved,
            CommitReply::HeadMoved {
                head: VersionNumber::new(7).unwrap()
            }
        );
        assert_eq!(moved.into_outcome(), None);
    }

    #[test]
    fn unknown_replies_are_refused_as_unavailable() {
        let error = reply(&json!({ "outcome": "bogus" })).unwrap_err();
        assert_eq!(error.code, "node_unavailable");
    }

    #[test]
    fn the_digest_names_the_request_not_the_head() {
        let operation = OperationId::parse("publish-1").unwrap();
        let layer = LayerId::parse("work").unwrap();
        let attribution = PublishAttribution::Human;
        let publish = Publish {
            operation: &operation,
            layer: &layer,
            expected: id("c"),
            base: VersionNumber::new(0).unwrap(),
            attribution: &attribution,
        };
        assert_eq!(digest(&publish).unwrap(), digest(&publish).unwrap());

        let other = PublishAttribution::Agent {
            context: None,
            allow_missing_trace: false,
        };
        let changed = Publish {
            attribution: &other,
            ..publish
        };
        assert_ne!(digest(&publish).unwrap(), digest(&changed).unwrap());
    }
}
