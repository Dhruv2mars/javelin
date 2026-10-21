//! A selectable node target, by URL.

use super::support::*;

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
            "gate4.exact-revision-authorization",
            "the Worker serves provision, permissions and state only (U13); review is not served",
        ),
        (
            "gate4.immutable-prior-revisions",
            "the Worker serves provision, permissions and state only (U13); review is not served",
        ),
        (
            "gate4.fork-data-without-credentials",
            "the Worker does not fork; ControlPlane::fork_repository needs a node that implements \
             NodeLink::fork",
        ),
        (
            "gate4.restricted-traces",
            "the Worker stores no Worlds or traces yet (U13 scope)",
        ),
        (
            "gate4.distinct-outage-outcomes",
            "the Worker has no repository wire for Layer writes or Publish yet, so the queued \
             node-down and control-plane-down outcomes cannot be driven through it",
        ),
        (
            "outage.coordinator-transactions",
            "workerd and miniflare are not accepted for coordinator transactions \
             (docs/online-design.md section 6); only a deployed Durable Object proves them",
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

    let Some(proxy) = proxy else {
        target.not_verified(
            "gate4.lost-reply-one-outcome",
            "the target is not on loopback, so no proxy can drop a reply in front of it",
        );
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
