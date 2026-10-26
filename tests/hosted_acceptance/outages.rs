//! The outage matrix: distinct control-plane and node outcomes.

use super::support::*;

// ---------------------------------------------------------------------------------------------
// Gate 4 and the outage matrix: distinct control-plane and provider outcomes.

#[test]
fn control_plane_and_node_outages_have_distinct_outcomes() {
    let mut hosted = Hosted::start();
    let vault = hosted.create("vault", false, Publish::Direct);
    let id = vault.repository.clone();
    let dir = hosted.node_dir(&id);
    let token = hosted.token(&id);
    let gate = hosted.gate(&id);
    let mut log = hosted.log(&id);
    let mut heads = BTreeMap::new();
    for name in ["p", "q"] {
        log.create_layer(&layer(name), v(0)).unwrap();
        let path = format!("{name}.txt");
        heads.insert(
            name,
            write(
                &mut log,
                name,
                &format!("write-{name}"),
                &path,
                name.as_bytes(),
            ),
        );
    }
    let empty = log
        .create_layer(&layer("c"), v(0))
        .unwrap()
        .checkpoint()
        .object();
    let proxy = Proxy::start(serve_forever(log, hosted.gate(&id), &id));
    // The control plane over HTTP, behind its own proxy, and this device signed in to it.
    let control = Proxy::start(serve_control_plane(hosted.dir.path()));
    let home = signed_in_home(&hosted.session);
    let project = TempDir::new().unwrap();
    let project_dir = project.path().to_str().unwrap();
    let replay = || {
        let output = javelin(
            home.path(),
            control.addr,
            &["outbox", "replay", "--remote", &id, "--repo", project_dir],
        );
        (output.status.code(), stdout_json(&output))
    };

    // Attach while both are up: the CLI gets a token for the private repository and caches it.
    let attached = javelin(
        home.path(),
        control.addr,
        &[
            "attach",
            &format!("http://{}/{id}", proxy.addr),
            project_dir,
        ],
    );
    assert_eq!(attached.status.code(), Some(0), "{attached:?}");
    let (node_seen, control_seen) = (proxy.forwarded(), control.forwarded());

    // Both down. Local work needs neither.
    proxy.set(Mode::Reset);
    control.set(Mode::Reset);
    let plane = hosted.plane.take().unwrap();
    drop(plane);
    let local = TempDir::new().unwrap();
    let mut repo = Repository::init(local.path()).unwrap();
    let base = repo.layer_base(v(0)).unwrap();
    let start = repo.create_layer(&layer("offline"), &base).unwrap();
    let content = repo.put_blob(b"written offline").unwrap();
    let captured = repo
        .capture_layer(
            &op("capture-offline"),
            &start,
            &file("offline.txt", content),
        )
        .unwrap();
    assert!(matches!(captured, javelin::LayerUpdate::Applied { .. }));
    verified(
        "outage.local-layer-work",
        "with the node and the control plane both down, a Layer is created and captured locally",
    );

    // Node down: uploads, appends and Publish queue; direct node actions refuse.
    let changes = file("c.txt", content);
    let bytes = changes.canonical_bytes();
    let checkpoint = Kind::Checkpoint.id(&bytes);
    let mut outbox = Outbox::open(project.path(), &id).unwrap();
    assert_eq!(Kind::Blob.id(b"written offline"), content);
    // Changes objects travel apart from content objects.
    let blob = encode(&Message::Objects(vec![(
        Kind::Blob,
        content,
        b"written offline".to_vec(),
    )]))
    .unwrap();
    let upload = encode(&Message::Objects(vec![(
        Kind::Checkpoint,
        checkpoint,
        bytes,
    )]))
    .unwrap();
    let append = encode(&Message::LayerAppend {
        operation: op("append-c"),
        layer: layer("c"),
        parent: Some(empty),
        checkpoint,
    })
    .unwrap();
    outbox
        .enqueue(Record::Upload, "c", &op("upload-blob"), &blob)
        .unwrap();
    outbox
        .enqueue(Record::Upload, "c", &op("upload-c"), &upload)
        .unwrap();
    outbox
        .enqueue(Record::Append, "c", &op("append-c"), &append)
        .unwrap();
    outbox
        .enqueue(
            Record::Publish,
            "p",
            &op("publish-p"),
            &publish_payload("publish-p", "p", heads["p"], 0),
        )
        .unwrap();
    drop(outbox);
    let (code, down) = replay();
    assert_eq!(code, Some(1), "{down}");
    assert!(down["delivered"].as_array().unwrap().is_empty());
    let node_fault = down["code"].as_str().unwrap().to_string();
    assert_eq!(node_fault, NODE_UNAVAILABLE);
    assert_eq!(down["pending"], 4);
    assert_eq!(
        call(proxy.addr, &id, &Message::Resolve { version: None }),
        Err(NODE_UNAVAILABLE),
        "a direct read (what attach does) refuses"
    );
    assert_eq!(head(&dir), v(0));
    assert_eq!(proxy.forwarded(), node_seen, "nothing reached the node");

    // Control plane down, token still valid: node actions continue under applied permissions.
    proxy.set(Mode::Pass);
    gate.check(
        Some(&token),
        now(),
        Need {
            scope: Scope::Publish,
            layer: None,
        },
    )
    .unwrap();
    let (code, up) = replay();
    assert_eq!(code, Some(0), "{up}");
    assert_eq!(up["delivered"].as_array().unwrap().len(), 4);
    assert_eq!(up["pending"], 0);
    let outbox = Outbox::open(project.path(), &id).unwrap();
    let outcomes: Vec<Message> = (1..=4)
        .map(|seq| decode(outbox.entry(seq).unwrap().outcome.as_ref().unwrap()).unwrap())
        .collect();
    drop(outbox);
    assert_eq!(outcomes[0], Message::Stored { count: 1 });
    assert_eq!(outcomes[1], Message::Stored { count: 1 });
    assert!(matches!(
        outcomes[2],
        Message::LayerOutcome(LayerOutcome::Accepted { seq: 1, .. })
    ));
    assert_eq!(published(&outcomes[3]).0, v(1));
    assert_eq!(head(&dir), v(1));
    assert_eq!(
        control.forwarded(),
        control_seen,
        "the control plane was not reached"
    );

    // Control plane down, token expired: the record waits with control_plane_unavailable.
    let mut outbox = Outbox::open(project.path(), &id).unwrap();
    outbox
        .enqueue(
            Record::Publish,
            "q",
            &op("publish-q"),
            &publish_payload("publish-q", "q", heads["q"], 0),
        )
        .unwrap();
    drop(outbox);
    let cache_path = home.path().join("tokens").join(format!("repo-{id}.json"));
    let mut cache: Value = serde_json::from_slice(&fs::read(&cache_path).unwrap()).unwrap();
    cache["expires_at"] = json!(now() - 1);
    fs::write(&cache_path, cache.to_string()).unwrap();
    let forwarded = proxy.forwarded();
    let (code, expired) = replay();
    assert_eq!(code, Some(1), "{expired}");
    assert!(expired["delivered"].as_array().unwrap().is_empty());
    let plane_fault = expired["code"].as_str().unwrap().to_string();
    assert_eq!(plane_fault, CONTROL_PLANE_UNAVAILABLE);
    assert_ne!(plane_fault, node_fault);
    assert_eq!(expired["pending"], 1);
    assert_eq!(head(&dir), v(1));
    assert_eq!(
        proxy.forwarded(),
        forwarded,
        "nothing was sent without a credential"
    );

    // The control plane refuses everything else that needs it, with the same code.
    let create = javelin(
        home.path(),
        control.addr,
        &[
            "repo",
            "create",
            "acme/other",
            "--operation",
            "create-other",
        ],
    );
    assert_eq!(error_code(&create), CONTROL_PLANE_UNAVAILABLE);
    let login = javelin(home.path(), control.addr, &["login"]);
    assert_eq!(error_code(&login), CONTROL_PLANE_UNAVAILABLE);

    // The control plane is back: the CLI refreshes the token, then the record replays once.
    control.set(Mode::Pass);
    let (code, back) = replay();
    assert_eq!(code, Some(0), "{back}");
    assert_eq!(back["delivered"][0]["outcome"]["published"], 2);
    assert_eq!(head(&dir), v(2));
    let renewed: Value = serde_json::from_slice(&fs::read(&cache_path).unwrap()).unwrap();
    assert!(renewed["expires_at"].as_u64().unwrap() > now());
    verified(
        "outage.node-down-queue",
        "node down (proxy resets): upload, LayerAppend and PublishLayer stay queued in the outbox \
         and `javelin outbox replay` reports node_unavailable, nothing reaches the node and a \
         direct read refuses; node back with the control plane down: the CLI sends its cached \
         token, the node's gate authorizes it without the control plane, and the queue drains \
         in order, once",
    );
    verified(
        "gate4.distinct-outage-outcomes",
        "with the product's transport (`javelin outbox replay`, which sends the access token as \
         Authorization: Bearer): node down is node_unavailable; control plane down with the \
         cached token expired is control_plane_unavailable and nothing is sent; both keep the \
         record queued, and it replays once after the CLI refreshes the token through the \
         control plane's HTTP API with the device refresh token",
    );
    verified(
        "outage.control-plane-refusals",
        "with the control plane's HTTP API unreachable: repo create, login and the token \
         refresh behind a replay each fail with control_plane_unavailable",
    );
    not_verified(
        "outage.control-plane-fork",
        "fork has no client path yet (javelin_control::ControlPlane::fork_repository runs in \
         process only), and there is no membership API",
    );
    not_verified(
        "outage.stack-node-down",
        "Stack is not served by the node yet (LayerStack has no handler; U6 is open)",
    );
    not_verified(
        "outage.review-node-down",
        "Revise queueing and Approve/Comment refusal need a client path for review messages; \
         the node's wire serves them, but no client sends them yet",
    );
    not_verified(
        "outage.coordinator-transactions",
        "coordinator transaction behavior is accepted only against a real Cloudflare Durable \
         Object (docs/online-design.md section 6); a deployed account is out of scope",
    );
}
