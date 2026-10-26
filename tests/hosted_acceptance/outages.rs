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
    let proxy = Proxy::start(serve_forever(log, &id));

    // Both down. Local work needs neither.
    proxy.set(Mode::Reset);
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
    let client = TempDir::new().unwrap();
    let mut outbox = Outbox::open(client.path(), "vault").unwrap();
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
    let mut wire = Wire {
        addr: proxy.addr,
        name: id.clone(),
        clock: now(),
        token_expires: now() + hosted.session.expires_in,
        refresh_token: hosted.session.refresh_token.clone(),
        plane: None,
    };
    let down = outbox.replay(&mut wire).unwrap();
    assert!(down.delivered.is_empty());
    let node_fault = down.stalled.unwrap().fault;
    assert_eq!(node_fault.code(), NODE_UNAVAILABLE);
    assert_eq!(outbox.pending().count(), 4);
    assert_eq!(
        call(proxy.addr, &id, &Message::Resolve { version: None }),
        Err(NODE_UNAVAILABLE),
        "a direct read (what attach does) refuses"
    );
    assert_eq!(head(&dir), v(0));
    assert_eq!(proxy.forwarded(), 0, "nothing reached the node");

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
    let up = outbox.replay(&mut wire).unwrap();
    assert!(up.stalled.is_none());
    assert_eq!(up.delivered.len(), 4);
    let outcomes: Vec<Message> = up
        .delivered
        .iter()
        .map(|delivered| decode(&delivered.outcome).unwrap())
        .collect();
    assert_eq!(outcomes[0], Message::Stored { count: 1 });
    assert_eq!(outcomes[1], Message::Stored { count: 1 });
    assert!(matches!(
        outcomes[2],
        Message::LayerOutcome(LayerOutcome::Accepted { seq: 1, .. })
    ));
    assert_eq!(published(&outcomes[3]).0, v(1));
    assert_eq!(head(&dir), v(1));

    // Control plane down, token expired: the record waits with control_plane_unavailable.
    outbox
        .enqueue(
            Record::Publish,
            "q",
            &op("publish-q"),
            &publish_payload("publish-q", "q", heads["q"], 0),
        )
        .unwrap();
    wire.clock = wire.token_expires;
    let expired = outbox.replay(&mut wire).unwrap();
    assert!(expired.delivered.is_empty());
    let plane_fault = expired.stalled.unwrap().fault;
    assert_eq!(plane_fault.code(), CONTROL_PLANE_UNAVAILABLE);
    assert_ne!(plane_fault.code(), node_fault.code());
    assert_eq!(outbox.pending().count(), 1);
    assert_eq!(head(&dir), v(1), "nothing was sent without a credential");

    // The control plane is back: refresh, then the record replays once.
    let plane = ControlPlane::open(hosted.dir.path()).unwrap();
    let mut wire = Wire {
        plane: Some(&plane),
        ..wire
    };
    let back = outbox.replay(&mut wire).unwrap();
    assert!(back.stalled.is_none());
    assert_eq!(
        published(&decode(&back.delivered[0].outcome).unwrap()).0,
        v(2)
    );
    assert_eq!(head(&dir), v(2));
    verified(
        "outage.node-down-queue",
        "node down (proxy resets): upload, LayerAppend and PublishLayer stay queued in the outbox \
         with node_unavailable, nothing reaches the node and a direct read refuses; node back with \
         the control plane down: the node's gate authorizes the still-valid token without the \
         control plane and the queue drains in order, once",
    );
    not_verified(
        "gate4.distinct-outage-outcomes",
        "the product has no client transport yet that sends a credential and refreshes it at the \
         control plane, so control_plane_unavailable on an expired token comes from this suite's \
         own transport; only the outbox keeping a record queued under each fault, with distinct \
         codes, and replaying it once is proven (TASKS.md U14)",
    );
    not_verified(
        "outage.control-plane-refusals",
        "repo create, fork and refresh against an unreachable control plane: the product has no \
         network control-plane client yet, and an unreadable control directory surfaces as \
         storage_error rather than control_plane_unavailable (TASKS.md U14; the ignored gap_* tests)",
    );
    not_verified(
        "outage.stack-node-down",
        "Stack is not served by the node yet (LayerStack has no handler; U6 is open)",
    );
    not_verified(
        "outage.review-node-down",
        "Revise queueing and Approve/Comment refusal need a client path for review messages; \
         review is served in-process only (javelin_node::review), not on the node's wire",
    );
    not_verified(
        "outage.coordinator-transactions",
        "coordinator transaction behavior is accepted only against a real Cloudflare Durable \
         Object (docs/online-design.md section 6); a deployed account is out of scope",
    );
}
