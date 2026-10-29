//! A selectable node target, by URL.

use super::support::*;
use javelin_protocol::codec::ReviewDetail;

// ---------------------------------------------------------------------------------------------
// A selectable target: a node at a URL, such as the Cloudflare Worker on `wrangler dev` or a
// deployed Worker. `JAVELIN_ACCEPTANCE_NODE_URL` and `JAVELIN_ACCEPTANCE_NODE_ADMIN_TOKEN` name
// it, and `JAVELIN_ACCEPTANCE_TARGET` labels its report lines. Faults are injected by a proxy
// when the node is on loopback; a remote node cannot be put behind one, so those checks report
// NOT VERIFIED there.

struct Target {
    name: String,
    url: String,
    token: String,
}

impl Target {
    fn from_env() -> Self {
        let var = |name: &str| env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
        Self {
            name: env::var("JAVELIN_ACCEPTANCE_TARGET")
                .unwrap_or_else(|_| "cloudflare-workerd".into()),
            url: var("JAVELIN_ACCEPTANCE_NODE_URL"),
            token: var("JAVELIN_ACCEPTANCE_NODE_ADMIN_TOKEN"),
        }
    }

    /// What the control plane records as this repository's node: the target's own URL.
    fn label(&self) -> NodeTarget {
        NodeTarget::cloudflare(&self.url)
    }

    fn node(&self, url: &str) -> CloudflareNode {
        CloudflareNode::new(url, &self.token).unwrap()
    }

    /// The node's address when it is on loopback, so a proxy can stand in front of it.
    fn loopback(&self) -> Option<SocketAddr> {
        let rest = self.url.strip_prefix("http://")?;
        let authority = rest.split('/').next()?;
        let (host, port) = authority.rsplit_once(':')?;
        let host = match host {
            "localhost" | "127.0.0.1" => "127.0.0.1",
            _ => return None,
        };
        format!("{host}:{port}").parse().ok()
    }

    fn verified(&self, id: &str, detail: &str) {
        report(&self.name, id, "VERIFIED", detail);
    }

    fn not_verified(&self, id: &str, reason: &str) {
        report(&self.name, id, "NOT VERIFIED", reason);
    }
}

fn node_state(node: &CloudflareNode, created: &Created) -> (Option<(String, String)>, Value) {
    let snapshot = node.snapshot(&repository_id(created)).unwrap();
    (
        snapshot.genesis,
        snapshot.permissions.unwrap_or(Value::Null),
    )
}

#[test]
#[ignore = "needs a node URL; tools/acceptance/run.ts starts one on workerd"]
fn target_repository_flows_and_retries() {
    let target = Target::from_env();
    let dir = TempDir::new().unwrap();
    let plane = ControlPlane::with_node(
        dir.path(),
        Box::new(target.node(&target.url)),
        target.label(),
    )
    .unwrap();
    let hosted = Hosted::sign_in(dir, plane);
    target.verified(
        "flow.browser-device-approval",
        "device sign-in approved through the approval page's HTTP endpoints with a passkey, for a \
         control plane whose node is this target",
    );
    let plane = hosted.plane();
    let bearer = &hosted.session.access_token;
    let genesis = javelin_node_worker::genesis().unwrap();

    let site = hosted.create("site", true, Publish::Requests);
    let vault = hosted.create("vault", false, Publish::Direct);
    let node = target.node(&target.url);
    for (created, public) in [(&site, true), (&vault, false)] {
        assert!(created.created);
        let (held, permissions) = node_state(&node, created);
        assert_eq!(held.as_ref(), Some(&genesis), "one World 0 for every node");
        assert_eq!(permissions["epoch"], 1);
        assert_eq!(permissions["public"], public);
        assert_eq!(permissions["roles"]["admin"], "maintain");
        assert_eq!(permissions["roles"].as_object().unwrap().len(), 1);
    }
    let again = plane
        .create_repository(
            bearer,
            &create_request("vault", false, Publish::Direct),
            now(),
        )
        .unwrap();
    assert!(!again.created);
    assert_eq!(again.repository, vault.repository);
    assert_eq!(node_state(&node, &vault).1["epoch"], 1);
    let mut collision = create_request("vault", true, Publish::Direct);
    collision.operation = "create-vault-again".into();
    assert_eq!(
        plane
            .create_repository(bearer, &collision, now())
            .unwrap_err()
            .code(),
        "conflict"
    );
    let site_token = hosted.token(&site.repository);
    assert_eq!(
        plane
            .create_repository(
                &site_token,
                &create_request("x", true, Publish::Direct),
                now()
            )
            .unwrap_err()
            .code(),
        "forbidden"
    );
    target.verified(
        "flow.public-private",
        "public+requests and private+direct repositories created through the control plane are \
         provisioned on the node with the shared World 0 and epoch-1 permissions (creator \
         maintain, visibility applied)",
    );
    target.verified(
        "flow.retries",
        "repeating create with its operation ID returns the same repository and leaves the \
         node at epoch 1; a name collision is conflict; a repository token cannot create",
    );

    // A wrong admin token: the node refuses, and the control plane does not report success.
    let impostor = CloudflareNode::new(&target.url, "not-the-admin-token").unwrap();
    let probe = RepositoryId::parse("repo_u14_probe").unwrap();
    assert!(matches!(
        impostor.provision(&probe),
        Err(ControlError::Unauthorized(_))
    ));
    assert!(impostor.snapshot(&repository_id(&site)).is_err());
    target.verified(
        "flow.permissions",
        "the node refuses provision and state reads without its admin token; the creator is the \
         only role a new repository's permissions hold",
    );
    for (id, reason) in [
        (
            "gate4.fork-data-without-credentials",
            "the Worker does not fork; ControlPlane::fork_repository needs a node that implements \
             NodeLink::fork",
        ),
        (
            "gate4.restricted-traces",
            "traces are restricted through the node's library API only; no wire message carries \
             them",
        ),
        (
            "gate4.distinct-outage-outcomes",
            "not driven through the Worker by this suite: the queued node-down and \
             control-plane-down outcomes need the suite's outage hooks on a remote node",
        ),
    ] {
        target.not_verified(id, reason);
    }
}

#[test]
#[ignore = "needs a node URL; tools/acceptance/run.ts starts one on workerd"]
fn target_node_outage_and_lost_replies() {
    let target = Target::from_env();
    let dir = TempDir::new().unwrap();
    let control = dir.path().to_path_buf();
    // Every control plane here names the target's URL; the proxy, when there is one, stands in
    // for it. Without one, an unreachable address plays the node that is down.
    let proxy = target.loopback().map(Proxy::start);
    let via = proxy.as_ref().map_or_else(
        || target.url.clone(),
        |proxy| format!("http://{}", proxy.addr),
    );
    let plane = |url: &str| {
        ControlPlane::with_node(&control, Box::new(target.node(url)), target.label()).unwrap()
    };

    // The node is down: create fails with node_unavailable and nothing is reported done.
    let down = match &proxy {
        Some(proxy) => {
            proxy.set(Mode::Reset);
            plane(&via)
        }
        None => plane("http://127.0.0.1:1"),
    };
    let hosted = Hosted::sign_in(dir, down);
    let bearer = hosted.session.access_token.clone();
    let request = create_request("app", false, Publish::Direct);
    let refused = hosted
        .plane()
        .create_repository(&bearer, &request, now())
        .unwrap_err();
    assert_eq!(refused.code(), NODE_UNAVAILABLE);
    let mut hosted = hosted;
    hosted.plane = None;

    // A retry may not finish the create on another node.
    let elsewhere = ControlPlane::with_node(
        &control,
        Box::new(target.node(&target.url)),
        NodeTarget::cloudflare("http://127.0.0.1:1"),
    )
    .unwrap();
    assert_eq!(
        elsewhere
            .create_repository(&bearer, &request, now())
            .unwrap_err()
            .code(),
        "operation_id_reused"
    );
    drop(elsewhere);

    // The node is back: the same operation completes once.
    if let Some(proxy) = &proxy {
        proxy.set(Mode::Pass);
    }
    let live = plane(&via);
    let created = live.create_repository(&bearer, &request, now()).unwrap();
    assert_eq!(created.epoch, 1);
    let node = target.node(&target.url);
    assert_eq!(node_state(&node, &created).1["epoch"], 1);
    target.verified(
        "outage.node-down-create",
        "repo create with the node down fails with node_unavailable; a retry naming another \
         node is operation_id_reused; the retry with the same operation ID on the same node \
         completes at epoch 1",
    );

    // A remote target has no proxy in front of it; `target_review_loop_over_the_wire` retries
    // a Publish whose reply was discarded there instead.
    let Some(proxy) = proxy else {
        return;
    };
    let plane = live;

    // The provision reply is lost after the node stored World 0.
    let request = create_request("lost-provision", true, Publish::Requests);
    proxy.set(Mode::DropReplyOnce);
    assert_eq!(
        plane
            .create_repository(&bearer, &request, now())
            .unwrap_err()
            .code(),
        NODE_UNAVAILABLE
    );
    let first = plane.create_repository(&bearer, &request, now()).unwrap();
    // The permissions reply is lost after the node applied epoch 1.
    let request = create_request("lost-permissions", false, Publish::Direct);
    proxy.drop_reply_after(1);
    assert_eq!(
        plane
            .create_repository(&bearer, &request, now())
            .unwrap_err()
            .code(),
        NODE_UNAVAILABLE
    );
    let second = plane.create_repository(&bearer, &request, now()).unwrap();
    let genesis = javelin_node_worker::genesis().unwrap();
    for created in [&first, &second] {
        let (held, permissions) = node_state(&node, created);
        assert_eq!(held, Some(genesis.clone()));
        assert_eq!(permissions["epoch"], 1);
        let repeat = plane
            .create_repository(
                &bearer,
                &create_request(&created.name, created.public, created.publish),
                now(),
            )
            .unwrap();
        assert!(!repeat.created);
        assert_eq!(repeat.repository, created.repository);
    }
    assert!(proxy.forwarded() >= 6);
    target.verified(
        "gate4.lost-reply-one-outcome",
        "the node's reply to provision, then to the permissions push, is dropped after the node \
         acted; each retry of the same create completes with one World 0 and permissions at \
         epoch 1, and a repeat returns the same repository",
    );
}

/// A maintainer's token from the public test key: every scope.
const MAINTAIN: &[Scope] = &[
    Scope::Read,
    Scope::LayerWrite,
    Scope::Publish,
    Scope::ReviewWrite,
    Scope::ReviewApprove,
];

/// Protocol version 2 at the target's `/v2/r/{repository}`, over its own transport (https for
/// a deployed Worker).
struct Remote {
    url: String,
    agent: ureq::Agent,
}

impl Remote {
    fn new(base: &str, repository: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .build()
            .into();
        Self {
            url: format!("{}/v2/r/{repository}", base.trim_end_matches('/')),
            agent,
        }
    }

    /// Sends `message` as the bearer of `token` and returns the node's reply.
    fn call(&self, token: &str, message: &Message) -> Message {
        let mut reply = self
            .agent
            .post(&self.url)
            .header("content-type", "application/octet-stream")
            .header("authorization", &format!("Bearer {token}"))
            .send(&encode(message).unwrap()[..])
            .expect("node reachable");
        assert_eq!(reply.status().as_u16(), 200, "{message:?}");
        decode(&reply.body_mut().read_to_vec().unwrap()).unwrap()
    }

    /// Creates Layer `name` at World `base` and returns its head.
    fn create_layer(&self, token: &str, name: &str, base: u64) -> ObjectId {
        let create = Message::LayerCreate {
            layer: layer(name),
            base: v(base),
        };
        match self.call(token, &create) {
            Message::LayerOutcome(LayerOutcome::Accepted { checkpoint, .. }) => checkpoint,
            other => panic!("create {name}: {other:?}"),
        }
    }

    /// Uploads `bytes` as `path` and appends it to Layer `name` after `parent`, as a client's
    /// capture does. Returns the Layer's new head.
    fn write(
        &self,
        token: &str,
        name: &str,
        operation: &str,
        parent: ObjectId,
        path: &str,
        bytes: &[u8],
    ) -> ObjectId {
        let blob = ObjectId::for_blob(bytes);
        self.call(
            token,
            &Message::Objects(vec![(Kind::Blob, blob, bytes.to_vec())]),
        );
        let changes = file(path, blob).canonical_bytes();
        let checkpoint = Kind::Checkpoint.id(&changes);
        self.call(
            token,
            &Message::Objects(vec![(Kind::Checkpoint, checkpoint, changes)]),
        );
        let append = Message::LayerAppend {
            operation: op(operation),
            layer: layer(name),
            parent: Some(parent),
            checkpoint,
        };
        match self.call(token, &append) {
            Message::LayerOutcome(LayerOutcome::Accepted { checkpoint, .. }) => checkpoint,
            other => panic!("append {name}: {other:?}"),
        }
    }

    fn head(&self, token: &str) -> u64 {
        match self.call(token, &Message::Resolve { version: None }) {
            Message::Resolved { number, .. } => number.get(),
            other => panic!("resolve: {other:?}"),
        }
    }

    fn review(&self, token: &str, id: &str) -> ReviewDetail {
        let read = Message::ReadReview {
            request: request(id),
            comments_from: 0,
        };
        match self.call(token, &read) {
            Message::ReviewDetail(detail) => detail,
            other => panic!("review: {other:?}"),
        }
    }
}

/// Gate 4's request loop, a lost reply, and the coordinator, driven over the wire through the
/// target as a client would: Layers created and written with `LayerCreate`, uploads and
/// `LayerAppend`, then a Publish Request reviewed, revised and published. The control plane
/// creates the repository; reviewers get roles by a permissions push with the node's admin
/// token, and every caller's token comes from the public test key the node is told to trust.
#[test]
#[ignore = "needs a node URL; tools/acceptance/run.ts starts one on workerd"]
fn target_review_loop_over_the_wire() {
    let target = Target::from_env();
    let dir = TempDir::new().unwrap();
    let plane = ControlPlane::with_node(
        dir.path(),
        Box::new(target.node(&target.url)),
        target.label(),
    )
    .unwrap();
    let hosted = Hosted::sign_in(dir, plane);
    let site = hosted.create("loop", false, Publish::Requests);
    let id = site.repository.as_str();
    let node = target.node(&target.url);
    node.trust(&repository_id(&site), &test_keys::keys())
        .unwrap();
    let reviewers = [("alice", Role::Review), ("rita", Role::Review)];
    node.apply(&permissions(&site, 2, &reviewers, 1)).unwrap();
    let wire = Remote::new(&target.url, id);
    let alice = member(id, "alice", REVIEWER);
    let rita = member(id, "rita", REVIEWER);
    let maintainer = member(id, "admin", MAINTAIN);

    let start = wire.create_layer(&alice, "work", 0);
    let one = wire.write(&alice, "work", "write-1", start, "README.md", b"one");
    let call = |token: &str, message: Message| wire.call(token, &message);
    assert_eq!(
        accepted(call(&alice, open_request("open", "pr-1", "work", one, 0))),
        1
    );
    assert_eq!(accepted(call(&rita, approve("approve-1", "pr-1", 1))), 1);
    let first = wire.review(&maintainer, "pr-1").revisions;
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].approvals, ["rita"]);

    let two = wire.write(&alice, "work", "write-2", one, "README.md", b"two");
    assert_eq!(
        accepted(call(&alice, revise("revise-1", "pr-1", 1, two))),
        2
    );
    let second = wire.review(&maintainer, "pr-1").revisions;
    assert_eq!(
        second[..1],
        first[..],
        "revision 1 and its approval are unchanged"
    );
    assert!(second[1].approvals.is_empty());

    // Exact authorization.
    for (token, message, code, why) in [
        (
            &maintainer,
            publish_request("publish-a", "pr-1", 2, 0),
            "not_enough_approvals",
            "an approval of revision 1 does not count for revision 2",
        ),
        (
            &maintainer,
            publish_request("publish-b", "pr-1", 1, 0),
            "stale_revision",
            "a superseded revision cannot be published",
        ),
        (
            &alice,
            approve("approve-self", "pr-1", 2),
            "self_approval",
            "the author cannot approve",
        ),
        (
            &rita,
            revise("revise-rita", "pr-1", 2, two),
            "not_author",
            "only the author revises",
        ),
        (
            &rita,
            approve("approve-old", "pr-1", 1),
            "stale_revision",
            "an approval names the exact current revision",
        ),
    ] {
        assert_eq!(refused(call(token, message)), code, "{why}");
    }
    // Immutable prior revisions: revision 2 cannot be submitted again.
    let three = wire.write(&alice, "work", "write-3", two, "README.md", b"three");
    assert_eq!(
        refused(call(&alice, revise("revise-again", "pr-1", 1, three))),
        "stale_revision"
    );

    // An approver whose role is removed at a newer epoch no longer counts.
    assert_eq!(accepted(call(&rita, approve("approve-2", "pr-1", 2))), 2);
    node.apply(&permissions(&site, 3, &reviewers[..1], 1))
        .unwrap();
    assert_eq!(
        refused(call(
            &maintainer,
            publish_request("publish-c", "pr-1", 2, 0)
        )),
        "not_enough_approvals"
    );
    node.apply(&permissions(&site, 4, &reviewers, 1)).unwrap();

    // The Layer has moved on to "three"; revision 2 still publishes its own tree, the one a
    // store holding only "two" at README.md composes.
    let scratch = TempDir::new().unwrap();
    Repository::init(scratch.path()).unwrap();
    let mut local = FileWorldLog::open(scratch.path()).unwrap();
    local.create_layer(&layer("work"), v(0)).unwrap();
    let local_two = write(&mut local, "work", "write-2", "README.md", b"two");
    let approved_tree = tree_of(&mut local, local_two);

    // The reply to the Publish is lost after the node decided: the client never reads it.
    // To the node, the retry is the same operation again.
    let publish = publish_request("publish-d", "pr-1", 2, 0);
    let _lost = call(&maintainer, publish.clone());
    let retry = call(&maintainer, publish.clone());
    let (number, tree) = published(&retry);
    assert_eq!(number, v(1));
    assert_eq!(tree, approved_tree);
    assert_eq!(call(&maintainer, publish), retry);
    assert_eq!(
        wire.head(&maintainer),
        1,
        "one World version for the operation"
    );
    assert_eq!(
        refused(call(
            &maintainer,
            publish_request("publish-d", "pr-1", 1, 0)
        )),
        "operation_id_reused"
    );
    assert_eq!(
        call(
            &alice,
            Message::ReadRequest {
                request: request("pr-1")
            }
        ),
        Message::RequestState {
            request: request("pr-1"),
            state: RequestStatus::Published,
            revision: 2,
        }
    );
    let last = wire.review(&maintainer, "pr-1").revisions;
    assert_eq!(last[..1], first[..]);
    assert_eq!(
        (last[1].base, last[1].checkpoint),
        (second[1].base, second[1].checkpoint)
    );
    target.verified(
        "gate4.exact-revision-authorization",
        "over the wire through the target, with Layers made by LayerCreate and LayerAppend: an \
         approval of revision 1 does not count for revision 2 (not_enough_approvals); a \
         superseded revision is stale_revision; self-approval and non-author revise are refused; \
         an approver removed at a newer epoch blocks Publish; Publish publishes the approved \
         revision's tree although the Layer moved on",
    );
    target.verified(
        "gate4.immutable-prior-revisions",
        "re-submitting a revision is stale_revision; ReadReview shows revision 1's base, \
         checkpoint and approvals unchanged after the revise, the approvals and the Publish \
         (a remote node's stored rows cannot be read directly)",
    );

    if target.loopback().is_some() {
        // `target_node_outage_and_lost_replies` drops the reply at a proxy, and workerd is not
        // accepted for the coordinator.
        target.not_verified(
            "outage.coordinator-transactions",
            "workerd and miniflare are not accepted for coordinator transactions \
             (docs/online-design.md section 6); only a deployed Durable Object proves them",
        );
        return;
    }
    target.verified(
        "gate4.lost-reply-one-outcome",
        "a PublishRequest whose reply the client discarded after the node answered, retried \
         twice: the identical outcome, the World advanced once, and the operation ID bound to \
         its request (operation_id_reused). No proxy stands in front of a remote node, so the \
         reply is lost at the client, which is the same retry to the node",
    );

    // The coordinator: concurrent publishers on one repository, exactly one per version.
    // Direct Publish needs a repository that requires no approvals.
    node.apply(&permissions(&site, 5, &reviewers, 0)).unwrap();
    const PUBLISHERS: u64 = 50;
    let maintainer = maintainer.as_str();
    let wire = &wire;
    // The Layers are made first, so a failure there ends the test before anyone waits.
    let publishes: Vec<Message> = thread::scope(|scope| {
        let threads: Vec<_> = (0..PUBLISHERS)
            .map(|i| {
                scope.spawn(move || {
                    let name = format!("c{i}");
                    let start = wire.create_layer(maintainer, &name, 1);
                    let head = wire.write(
                        maintainer,
                        &name,
                        &format!("write-{name}"),
                        start,
                        &format!("c/{i}.txt"),
                        name.as_bytes(),
                    );
                    Message::PublishLayer {
                        operation: op(&format!("publish-{name}")),
                        layer: layer(&name),
                        expected: head,
                        base: v(1),
                        attribution: PublishAttribution::Human,
                    }
                })
            })
            .collect();
        threads.into_iter().map(|t| t.join().unwrap()).collect()
    });
    let ready = std::sync::Barrier::new(publishes.len());
    let outcomes: Vec<(u64, &Message)> = thread::scope(|scope| {
        let ready = &ready;
        let threads: Vec<_> = publishes
            .iter()
            .map(|publish| {
                scope.spawn(move || {
                    ready.wait();
                    let outcome = wire.call(maintainer, publish);
                    assert_eq!(wire.call(maintainer, publish), outcome, "the replay");
                    (published(&outcome).0.get(), publish)
                })
            })
            .collect();
        threads.into_iter().map(|t| t.join().unwrap()).collect()
    });
    let mut numbers: Vec<u64> = outcomes.iter().map(|(number, _)| *number).collect();
    numbers.sort_unstable();
    assert_eq!(numbers, (2..=PUBLISHERS + 1).collect::<Vec<_>>());
    assert_eq!(wire.head(maintainer), PUBLISHERS + 1);
    for (number, publish) in &outcomes {
        assert_eq!(published(&wire.call(maintainer, publish)).0.get(), *number);
    }
    target.verified(
        "outage.coordinator-transactions",
        "on the deployed Durable Object: 50 PublishLayer calls released together on one \
         repository, each over World 1 with its own path, publish as Worlds 2 to 51, exactly one \
         per version, the head is 51, and every replay returns its version. A kill inside a \
         commit cannot be injected into a deployed object; that rests on transactionSync \
         (tools/node-cloudflare/README.md)",
    );
}
