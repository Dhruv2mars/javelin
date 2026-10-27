//! The node conformance suite: the same assertions for every node that implements
//! `NodeLink`, and for its protocol version 2 endpoint over HTTP. `local_node.rs` runs it
//! against the self-hosted node; `workerd.rs` runs it against the Cloudflare node on
//! `wrangler dev` or a deployed Worker.

#![allow(dead_code)] // each test binary uses a different subset

use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_control::{ControlError, NodeLink};
use javelin_identity::{Scope, test_keys};
use javelin_node_worker::{Snapshot, genesis, permissions_json};
use javelin_protocol::{
    Message,
    codec::{Kind, LayerOutcome, PublishOutcome, decode, encode},
};
use std::collections::BTreeMap;

/// The genesis every node must report after provisioning.
pub fn expected_genesis() -> Option<(String, String)> {
    Some(genesis().expect("genesis"))
}

pub fn permissions(
    repository: &RepositoryId,
    epoch: u64,
    visibility: Visibility,
    extra_reviewer: bool,
) -> AppliedPermissions {
    let mut roles = BTreeMap::from([(
        PrincipalId::parse("admin").expect("principal"),
        Role::Maintain,
    )]);
    if extra_reviewer {
        roles.insert(
            PrincipalId::parse("reviewer").expect("principal"),
            Role::Review,
        );
    }
    AppliedPermissions {
        repository: repository.clone(),
        epoch,
        visibility,
        roles,
        required_approvals: 0,
    }
}

fn refused_code(error: &ControlError) -> Option<&str> {
    match error {
        ControlError::Refused(protocol) => Some(protocol.code.as_str()),
        _ => None,
    }
}

/// Runs the suite on a repository the node has never seen. `snapshot` reads what the node
/// holds for it.
pub fn run(node: &dyn NodeLink, repository: &RepositoryId, snapshot: &dyn Fn() -> Snapshot) {
    assert_eq!(snapshot().genesis, None, "a fresh node holds no World 0");

    // Provision creates World 0 once; a repeat changes nothing.
    node.provision(repository).expect("provision");
    assert_eq!(snapshot().genesis, expected_genesis());
    node.provision(repository).expect("provision again");
    assert_eq!(snapshot().genesis, expected_genesis());
    assert_eq!(snapshot().permissions, None);

    // First permissions push; the same push again is accepted.
    let first = permissions(repository, 1, Visibility::Private, false);
    node.apply(&first).expect("apply epoch 1");
    assert_eq!(snapshot().permissions, Some(permissions_json(&first)));
    node.apply(&first).expect("apply epoch 1 again");

    // The same epoch with different content is refused, and nothing changes.
    let altered = permissions(repository, 1, Visibility::Public, false);
    let error = node.apply(&altered).expect_err("same epoch, other content");
    assert_eq!(refused_code(&error), Some("stale_permissions"), "{error}");
    assert_eq!(snapshot().permissions, Some(permissions_json(&first)));

    // An older epoch is refused.
    let older = permissions(repository, 0, Visibility::Private, false);
    let error = node.apply(&older).expect_err("older epoch");
    assert_eq!(refused_code(&error), Some("stale_permissions"), "{error}");

    // A newer epoch replaces the permissions.
    let second = permissions(repository, 2, Visibility::Public, true);
    node.apply(&second).expect("apply epoch 2");
    assert_eq!(snapshot().permissions, Some(permissions_json(&second)));

    // Provisioning again must not reset the applied permissions.
    node.provision(repository).expect("provision after apply");
    assert_eq!(snapshot().genesis, expected_genesis());
    assert_eq!(snapshot().permissions, Some(permissions_json(&second)));
}

/// One protocol version 2 exchange with a node over HTTP.
pub struct Wire {
    url: String,
    agent: ureq::Agent,
}

/// What a node answered: the HTTP status, a `Access-Control-Allow-Origin: *` header, and the
/// decoded message when the status is 200.
#[derive(Debug)]
pub struct Answer {
    pub status: u16,
    pub cors: bool,
    pub message: Option<Message>,
}

impl Wire {
    /// The node at `base` (`http(s)://host[:port]`), serving `repository`.
    pub fn new(base: &str, repository: &RepositoryId) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .build()
            .into();
        Self {
            url: format!(
                "{}/v2/r/{}",
                base.trim_end_matches('/'),
                repository.as_str()
            ),
            agent,
        }
    }

    /// POSTs `body` with the `Authorization` value `authorization`, from a browser `Origin`
    /// when `origin`.
    pub fn post_raw(&self, authorization: Option<&str>, origin: bool, body: &[u8]) -> Answer {
        let mut request = self
            .agent
            .post(&self.url)
            .header("content-type", "application/octet-stream");
        if let Some(value) = authorization {
            request = request.header("authorization", value);
        }
        if origin {
            request = request.header("origin", "https://page.example");
        }
        let mut reply = request.send(body).expect("node reachable");
        let status = reply.status().as_u16();
        let cors = reply
            .headers()
            .get("access-control-allow-origin")
            .is_some_and(|value| value == "*");
        let bytes = reply.body_mut().read_to_vec().expect("reply body");
        let message = (status == 200).then(|| decode(&bytes).expect("one message"));
        Answer {
            status,
            cors,
            message,
        }
    }

    /// Sends `message` as `caller` (`None` is anonymous).
    pub fn send(&self, caller: Option<&str>, message: &Message) -> Answer {
        let token = caller.map(|sub| format!("Bearer {}", token_for(sub, &self.repository())));
        self.post_raw(token.as_deref(), false, &encode(message).expect("encode"))
    }

    /// The reply message of a 200 answer.
    pub fn reply(&self, caller: Option<&str>, message: &Message) -> Message {
        let answer = self.send(caller, message);
        assert_eq!(answer.status, 200, "{message:?}: {answer:?}");
        answer.message.expect("a message")
    }

    fn repository(&self) -> String {
        self.url.rsplit('/').next().expect("repository").to_string()
    }

    /// `OPTIONS` or another method on the endpoint, without a body.
    pub fn status_of(&self, method: &str) -> u16 {
        let request = ureq::http::Request::builder()
            .method(method)
            .uri(&self.url)
            .body(())
            .expect("request");
        self.agent
            .run(request)
            .expect("node reachable")
            .status()
            .as_u16()
    }
}

/// A ten-minute token from the public test issuer for `sub` on `repository`, with every scope.
pub fn token_for(sub: &str, repository: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let scopes = [
        Scope::Read,
        Scope::LayerWrite,
        Scope::Publish,
        Scope::ReviewWrite,
        Scope::ReviewApprove,
    ];
    let claims = test_keys::claims(
        sub,
        repository,
        &scopes,
        now,
        now + 600,
        &format!("{sub}-jti"),
    );
    test_keys::issuer().issue(&claims).expect("token")
}

fn code(message: &Message) -> Option<&str> {
    match message {
        Message::Error { code, .. } => Some(code),
        Message::LayerOutcome(LayerOutcome::Refused { code })
        | Message::PublishOutcome(PublishOutcome::Refused { code }) => Some(code),
        _ => None,
    }
}

/// The protocol version 2 half of the suite, after `run`: the node trusts the public test
/// key, and the repository is public at epoch 2, with `admin` holding Maintain and `reviewer`
/// Review. `wire` reaches the repository and `absent` a repository the node does not serve.
pub fn run_wire(node: &dyn NodeLink, repository: &RepositoryId, wire: &Wire, absent: &Wire) {
    node.trust(repository, &test_keys::keys())
        .expect("trust the test key");

    // Transport: the endpoint answers a CORS preflight, refuses other methods and other
    // repositories, and refuses a credential that is not a bearer token.
    assert_eq!(wire.status_of("OPTIONS"), 204);
    assert_eq!(wire.status_of("GET"), 405);
    let hello = encode(&Message::Resolve { version: None }).expect("encode");
    assert_eq!(absent.post_raw(None, false, &hello).status, 404);
    assert_eq!(
        wire.post_raw(Some("Basic YWRtaW4="), false, &hello).status,
        400
    );

    // Anyone reads a public repository's head, from a browser too.
    let (id, tree) = expected_genesis().expect("genesis");
    let read = wire.post_raw(None, true, &hello);
    assert!(read.cors, "a read reply is readable from any origin");
    match read.message.expect("a message") {
        Message::Resolved {
            number,
            id: at,
            tree: root,
        } => {
            assert_eq!(number.get(), 0);
            assert_eq!((at.to_string(), root.to_string()), (id, tree));
        }
        other => panic!("resolve: {other:?}"),
    }

    // An upload is stored once, and only its uploader can confirm it before it is published.
    let blob = b"conformance blob\n".to_vec();
    let blob_id = javelin::ObjectId::for_blob(&blob);
    let want = Message::Want(vec![(Kind::Blob, blob_id)]);
    assert_eq!(
        wire.reply(Some("admin"), &want),
        Message::Missing(vec![(Kind::Blob, blob_id)])
    );
    assert_eq!(
        wire.reply(
            Some("admin"),
            &Message::Objects(vec![(Kind::Blob, blob_id, blob.clone())])
        ),
        Message::Stored { count: 1 }
    );
    assert_eq!(wire.reply(Some("admin"), &want), Message::Missing(vec![]));
    let get = Message::Get(vec![(Kind::Blob, blob_id)]);
    assert_eq!(
        wire.reply(Some("admin"), &get),
        Message::Objects(vec![(Kind::Blob, blob_id, blob)])
    );
    assert_eq!(code(&wire.reply(None, &get)), Some("missing_object"));
    assert_eq!(
        code(&wire.reply(Some("reviewer"), &get)),
        Some("missing_object")
    );

    // Layer messages name Layers this repository does not hold.
    let layer = javelin::LayerId::parse("absent").expect("layer");
    assert_eq!(
        code(&wire.reply(
            Some("admin"),
            &Message::LayerHead {
                layer: layer.clone()
            }
        )),
        Some("unknown_layer")
    );
    let publish = Message::PublishLayer {
        operation: javelin::OperationId::parse("publish-absent").expect("operation"),
        layer,
        expected: blob_id,
        base: javelin::VersionNumber::new(0).expect("version"),
        attribution: javelin::PublishAttribution::Human,
    };
    assert_eq!(
        code(&wire.reply(Some("admin"), &publish)),
        Some("unknown_layer")
    );

    // Refusals: an anonymous write, a token the node does not trust, a reviewer's Publish
    // and any write from a browser.
    assert_eq!(code(&wire.reply(None, &publish)), Some("unauthorized"));
    let forged = format!("Bearer {}x", token_for("admin", repository.as_str()));
    let answer = wire.post_raw(Some(&forged), false, &encode(&publish).expect("encode"));
    assert_eq!(answer.message.as_ref().and_then(code), Some("unauthorized"));
    assert_eq!(
        code(&wire.reply(Some("reviewer"), &publish)),
        Some("forbidden")
    );
    let token = format!("Bearer {}", token_for("admin", repository.as_str()));
    let answer = wire.post_raw(Some(&token), true, &encode(&publish).expect("encode"));
    assert!(!answer.cors);
    assert_eq!(answer.message.as_ref().and_then(code), Some("forbidden"));
}
