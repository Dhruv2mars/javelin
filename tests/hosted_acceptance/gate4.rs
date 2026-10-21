//! Gate 4: revision authorization, lost replies, forks and restricted traces.

use super::support::*;

// ---------------------------------------------------------------------------------------------
// Gate 4: exact revision authorization and immutable prior revisions.

#[test]
fn exact_revision_authorization_and_immutable_prior_revisions() {
    let hosted = Hosted::start();
    let site = hosted.create("site", true, Publish::Requests);
    let id = site.repository.as_str();
    let reviewers = [("alice", Role::Review), ("rita", Role::Review)];
    hosted.push(&site, 2, &reviewers, 1);
    let gate = hosted.gate(id);
    let dir = hosted.node_dir(id);
    let mut log = hosted.log(id);
    log.create_layer(&layer("work"), v(0)).unwrap();
    let alice = member(id, "alice", REVIEWER);
    let rita = member(id, "rita", REVIEWER);
    let maintainer = hosted.token(id);

    let one = write(&mut log, "work", "write-1", "README.md", b"one");
    assert_eq!(
        accepted(send(
            &mut log,
            &gate,
            &alice,
            open_request("open", "pr-1", "work", one, 0)
        )),
        1
    );
    assert_eq!(
        accepted(send(
            &mut log,
            &gate,
            &rita,
            approve("approve-1", "pr-1", 1)
        )),
        1
    );
    let first_rows = review_rows(&dir, "pr-1");
    assert_eq!(first_rows.len(), 2);

    let two = write(&mut log, "work", "write-2", "README.md", b"two");
    let approved_tree = tree_of(&mut log, two);
    assert_eq!(
        accepted(send(
            &mut log,
            &gate,
            &alice,
            revise("revise-1", "pr-1", 1, two)
        )),
        2
    );
    // Revision 1 and its approval are unchanged byte for byte.
    let second_rows = review_rows(&dir, "pr-1");
    assert_eq!(second_rows[..2], first_rows[..]);

    // Exact authorization.
    assert_eq!(
        refused(send(
            &mut log,
            &gate,
            &maintainer,
            publish_request("publish-a", "pr-1", 2, 0)
        )),
        "not_enough_approvals",
        "an approval of revision 1 does not count for revision 2"
    );
    assert_eq!(
        refused(send(
            &mut log,
            &gate,
            &maintainer,
            publish_request("publish-b", "pr-1", 1, 0)
        )),
        "stale_revision",
        "a superseded revision cannot be published"
    );
    assert_eq!(
        refused(send(
            &mut log,
            &gate,
            &alice,
            approve("approve-self", "pr-1", 2)
        )),
        "self_approval"
    );
    assert_eq!(
        refused(send(
            &mut log,
            &gate,
            &rita,
            revise("revise-rita", "pr-1", 2, two)
        )),
        "not_author"
    );
    assert_eq!(
        refused(send(
            &mut log,
            &gate,
            &rita,
            approve("approve-old", "pr-1", 1)
        )),
        "stale_revision",
        "an approval names the exact current revision"
    );

    // Immutable prior revisions: revision 2 cannot be submitted again.
    let three = write(&mut log, "work", "write-3", "README.md", b"three");
    assert_eq!(
        refused(send(
            &mut log,
            &gate,
            &alice,
            revise("revise-again", "pr-1", 1, three)
        )),
        "stale_revision"
    );

    // An approver whose role is removed at a newer epoch no longer counts.
    assert_eq!(
        accepted(send(
            &mut log,
            &gate,
            &rita,
            approve("approve-2", "pr-1", 2)
        )),
        2
    );
    // A permissions push takes the node's lock, which an open log holds.
    drop(log);
    hosted.push(&site, 3, &reviewers[..1], 1);
    let without_rita = hosted.gate(id);
    let mut log = hosted.log(id);
    assert_eq!(
        refused(send(
            &mut log,
            &without_rita,
            &maintainer,
            publish_request("publish-c", "pr-1", 2, 0)
        )),
        "not_enough_approvals"
    );
    drop(log);
    hosted.push(&site, 4, &reviewers, 1);
    let gate = hosted.gate(id);
    let mut log = hosted.log(id);

    // The Layer has moved on to "three"; revision 2 still publishes its own tree.
    let reply = send(
        &mut log,
        &gate,
        &maintainer,
        publish_request("publish-d", "pr-1", 2, 0),
    );
    let (number, tree) = published(&reply);
    assert_eq!(number, v(1));
    assert_eq!(tree, approved_tree);
    let final_rows = review_rows(&dir, "pr-1");
    assert_eq!(final_rows[..second_rows.len()], second_rows[..]);
    assert_eq!(
        send(
            &mut log,
            &gate,
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
    verified(
        "gate4.exact-revision-authorization",
        "an approval of revision 1 does not count for revision 2 (not_enough_approvals); a \
         superseded revision is stale_revision; self-approval and non-author revise are refused; \
         an approver removed at a newer epoch blocks Publish; Publish publishes the approved \
         revision's tree although the Layer moved on",
    );
    verified(
        "gate4.immutable-prior-revisions",
        "re-submitting a revision is stale_revision; the stored rows of every earlier revision \
         (command, reply and hash chain) are byte-identical after each later revise, approval \
         and Publish",
    );
}

// ---------------------------------------------------------------------------------------------
// Gate 4: one durable outcome after a lost reply.

#[test]
fn one_durable_outcome_after_a_lost_reply() {
    let hosted = Hosted::start();
    let vault = hosted.create("vault", false, Publish::Direct);
    let id = vault.repository.clone();
    let dir = hosted.node_dir(&id);
    let mut log = hosted.log(&id);
    let mut heads = BTreeMap::new();
    for name in ["a", "b"] {
        log.create_layer(&layer(name), v(0)).unwrap();
        let head = write(
            &mut log,
            name,
            &format!("write-{name}"),
            &format!("{name}.txt"),
            name.as_bytes(),
        );
        heads.insert(name, head);
    }
    // The node serves one connection, so it can stop after the lost reply.
    let (addr, server) = serve_then_return(log, &id, 1);
    let proxy = Proxy::start(addr);

    // PublishLayer: the node commits, the reply is lost, the node restarts, the client retries.
    let publish_a = Message::PublishLayer {
        operation: op("publish-a"),
        layer: layer("a"),
        expected: heads["a"],
        base: v(0),
        attribution: PublishAttribution::Human,
    };
    proxy.set(Mode::DropReplyOnce);
    assert_eq!(call(proxy.addr, &id, &publish_a), Err(NODE_UNAVAILABLE));
    assert_eq!(
        head(&dir),
        v(1),
        "the node decided before the reply was lost"
    );
    // The node stops and reopens from disk: the retry meets only what it stored.
    drop(server.join().unwrap());
    let proxy = Proxy::start(serve_forever(hosted.log(&id), &id));
    let retry = call(proxy.addr, &id, &publish_a).unwrap();
    let again = call(proxy.addr, &id, &publish_a).unwrap();
    assert_eq!(published(&retry).0, v(1));
    assert_eq!(retry, again);
    assert_eq!(head(&dir), v(1), "one World version for the operation");
    let other = Message::PublishLayer {
        operation: op("publish-a"),
        layer: layer("b"),
        expected: heads["b"],
        base: v(1),
        attribution: PublishAttribution::Human,
    };
    assert_eq!(
        call(proxy.addr, &id, &other).unwrap(),
        Message::PublishOutcome(PublishOutcome::Refused {
            code: "operation_id_reused".into()
        })
    );

    // The outbox: the reply is lost mid-replay; the record stays queued and replays once.
    let client = TempDir::new().unwrap();
    let mut outbox = Outbox::open(client.path(), "vault").unwrap();
    let seq = outbox
        .enqueue(
            Record::Publish,
            "b",
            &op("publish-b"),
            &publish_payload("publish-b", "b", heads["b"], 0),
        )
        .unwrap();
    let mut wire = Wire {
        addr: proxy.addr,
        name: id.clone(),
        clock: now(),
        token_expires: now() + hosted.session.expires_in,
        refresh_token: hosted.session.refresh_token.clone(),
        plane: hosted.plane.as_ref(),
    };
    proxy.set(Mode::DropReplyOnce);
    let lost = outbox.replay(&mut wire).unwrap();
    assert!(lost.delivered.is_empty());
    assert_eq!(lost.stalled.unwrap().fault.code(), NODE_UNAVAILABLE);
    assert_eq!(head(&dir), v(2));
    // The client restarts; the journal still holds the record.
    drop(outbox);
    let mut outbox = Outbox::open(client.path(), "vault").unwrap();
    assert_eq!(outbox.pending().count(), 1);
    let replayed = outbox.replay(&mut wire).unwrap();
    assert_eq!(replayed.delivered.len(), 1);
    assert_eq!(replayed.delivered[0].seq, seq);
    assert_eq!(
        published(&decode(&replayed.delivered[0].outcome).unwrap()).0,
        v(2)
    );
    assert_eq!(outbox.pending().count(), 0);
    assert!(outbox.replay(&mut wire).unwrap().delivered.is_empty());
    assert_eq!(
        head(&dir),
        v(2),
        "one World version for the queued operation"
    );

    // PublishRequest: the review partition records one outcome for the operation.
    let site = hosted.create("site", true, Publish::Requests);
    hosted.push(
        &site,
        2,
        &[("alice", Role::Review), ("rita", Role::Review)],
        1,
    );
    let site_id = site.repository.as_str();
    let gate = hosted.gate(site_id);
    let mut log = hosted.log(site_id);
    log.create_layer(&layer("work"), v(0)).unwrap();
    let (alice, rita) = (
        member(site_id, "alice", REVIEWER),
        member(site_id, "rita", REVIEWER),
    );
    let maintainer = hosted.token(site_id);
    let one = write(&mut log, "work", "write-1", "a.txt", b"one");
    send(
        &mut log,
        &gate,
        &alice,
        open_request("open", "pr-1", "work", one, 0),
    );
    send(&mut log, &gate, &rita, approve("approve-1", "pr-1", 1));
    let first = send(
        &mut log,
        &gate,
        &maintainer,
        publish_request("publish", "pr-1", 1, 0),
    );
    // The node restarts before the retry.
    drop(log);
    let mut log = hosted.log(site_id);
    let retry = send(
        &mut log,
        &gate,
        &maintainer,
        publish_request("publish", "pr-1", 1, 0),
    );
    assert_eq!(first, retry);
    assert_eq!(published(&first).0, v(1));
    assert_eq!(
        refused(send(
            &mut log,
            &gate,
            &maintainer,
            publish_request("publish-2", "pr-1", 1, 0)
        )),
        "not_open"
    );
    assert_eq!(log.head().unwrap().number, v(1));
    verified(
        "gate4.lost-reply-one-outcome",
        "PublishLayer over loopback HTTP with the reply dropped after commit, then a node \
         restart: the retry returns the identical outcome and the World advanced once; the \
         operation ID is bound to its request (operation_id_reused); a queued outbox Publish whose \
         reply was lost stays queued across a client restart and replays to the same single \
         version; a PublishRequest repeated after a node restart returns the recorded outcome",
    );
    assert!(proxy.forwarded() >= 5);
}

// ---------------------------------------------------------------------------------------------
// Gate 4: inherited fork data without inherited credentials.

#[test]
fn a_fork_inherits_data_but_not_credentials() {
    let hosted = Hosted::start();
    let site = hosted.create("site", true, Publish::Requests);
    let source = site.repository.clone();
    hosted.push(
        &site,
        2,
        &[("alice", Role::Review), ("rita", Role::Maintain)],
        1,
    );
    let (content, published_tree) = {
        let mut log = hosted.log(&source);
        log.create_layer(&layer("main"), v(0)).unwrap();
        let content = log.repository().put_blob(b"inherited content").unwrap();
        let head = log
            .append(&op("write-1"), &layer("main"), &file("README.md", content))
            .unwrap()
            .checkpoint()
            .object();
        let outcome = log
            .publish_layer(
                &op("publish-1"),
                &layer("main"),
                head,
                v(0),
                &PublishAttribution::Human,
            )
            .unwrap();
        let PublishOutcome::Published { tree, .. } = outcome else {
            panic!("publish: {outcome:?}");
        };
        // An open request in the source: review state is the source's alone.
        let gate = hosted.gate(&source);
        log.create_layer(&layer("draft"), v(1)).unwrap();
        let draft = write(&mut log, "draft", "write-2", "b.txt", b"draft");
        let alice = member(&source, "alice", REVIEWER);
        assert_eq!(
            accepted(send(
                &mut log,
                &gate,
                &alice,
                open_request("open", "pr-1", "draft", draft, 1)
            )),
            1
        );
        (content, tree)
    };

    let plane = hosted.plane();
    let fork_request = ForkRequest {
        source: source.clone(),
        at: 1,
        target: create_request("site-fork", true, Publish::Requests),
    };
    let forked = plane
        .fork_repository(&hosted.session.access_token, &fork_request, now())
        .unwrap();
    let retried = plane
        .fork_repository(&hosted.session.access_token, &fork_request, now())
        .unwrap();
    assert_eq!(retried.repository.repository, forked.repository.repository);
    assert!(!retried.repository.created);
    let target = forked.repository.repository.clone();
    assert_ne!(target, source);

    // Data: version 1 has the same ID and tree, and its content is in the fork's own store.
    let source_repo = Repository::open(hosted.node_dir(&source)).unwrap();
    let fork_repo = Repository::open(hosted.node_dir(&target)).unwrap();
    let (theirs, ours) = (
        source_repo.version(v(1)).unwrap(),
        fork_repo.version(v(1)).unwrap(),
    );
    assert_eq!((ours.id, ours.tree), (theirs.id, theirs.tree));
    assert_eq!(ours.tree, published_tree);
    assert_eq!(fork_repo.head().unwrap().number, v(1));
    assert_eq!(fork_repo.read_blob(content).unwrap(), b"inherited content");
    assert_eq!(forked.origin.source, source);
    assert_eq!(forked.origin.number, v(1));
    assert_eq!(forked.origin.version, theirs.id);

    // Credentials: the fork has its own permissions, with the forker as its only member.
    let fork_gate = hosted.gate(&target);
    let applied = fork_gate.permissions().unwrap();
    assert_eq!(applied.epoch, 1);
    assert_eq!(applied.required_approvals, 0);
    assert_eq!(
        applied
            .roles
            .keys()
            .map(PrincipalId::as_str)
            .collect::<Vec<_>>(),
        ["admin"]
    );
    let t = now();
    let need = |scope| Need { scope, layer: None };
    let source_member = member(&source, "rita", &[Scope::Read, Scope::Publish]);
    let as_fork_member = member(&target, "rita", &[Scope::Read, Scope::Publish]);
    let source_token = hosted.token(&source);
    assert_eq!(
        code_of(fork_gate.check(Some(&source_member), t, need(Scope::Publish))),
        "forbidden",
        "a source member's token"
    );
    assert_eq!(
        code_of(fork_gate.check(Some(&as_fork_member), t, need(Scope::Publish))),
        "forbidden",
        "a source member's role does not carry over"
    );
    assert_eq!(
        code_of(fork_gate.check(Some(&source_token), t, need(Scope::Read))),
        "forbidden",
        "the creator's token for the source"
    );
    let fork_token = hosted.token(&target);
    fork_gate
        .check(Some(&fork_token), t, need(Scope::Publish))
        .unwrap();

    // Review state stays with the source; the fork moves on its own.
    let mut fork_log = hosted.log(&target);
    let reply = review::handle(
        &mut fork_log,
        &fork_gate,
        Some(&fork_token),
        t,
        &Message::ReadRequest {
            request: request("pr-1"),
        },
    );
    assert!(
        reply.is_err(),
        "the source's request is not in the fork: {reply:?}"
    );
    fork_log.create_layer(&layer("next"), v(1)).unwrap();
    let next = write(&mut fork_log, "next", "write-f", "c.txt", b"fork only");
    let outcome = fork_log
        .publish_layer(
            &op("publish-f"),
            &layer("next"),
            next,
            v(1),
            &PublishAttribution::Human,
        )
        .unwrap();
    assert!(matches!(outcome, PublishOutcome::Published { number, .. } if number == v(2)));
    assert_eq!(head(&hosted.node_dir(&source)), v(1));
    verified(
        "gate4.fork-data-without-credentials",
        "fork through the control plane at version 1: same version ID and tree, content readable \
         from the fork's own store, fork_of recorded, retry returns the same fork; the fork's \
         permissions hold only the forker at epoch 1; source members' roles and every source \
         token are refused (forbidden); the source's Publish Requests are not in the fork; \
         publishing on the fork leaves the source unchanged",
    );
}

// ---------------------------------------------------------------------------------------------
// Gate 4: no access to restricted traces.

#[test]
fn restricted_traces_stay_with_maintainers() {
    const SECRET: &[u8] = b"agent trace: the staging key is in vault 7";
    let hosted = Hosted::start();
    let site = hosted.create("site", true, Publish::Requests);
    let id = site.repository.clone();
    hosted.push(&site, 2, &[("rita", Role::Review), ("bob", Role::Read)], 1);
    let gate = hosted.gate(&id);
    let maintainer = hosted.token(&id);
    let rita = member(
        &id,
        "rita",
        &[Scope::Read, Scope::Publish, Scope::ReviewApprove],
    );
    let bob = member(&id, "bob", &[Scope::Read]);

    let mut log = hosted.log(&id);
    log.create_layer(&layer("agent"), v(0)).unwrap();
    let content = log.repository().put_blob(b"public content").unwrap();
    let head = log
        .append(&op("write-1"), &layer("agent"), &file("out.txt", content))
        .unwrap()
        .checkpoint()
        .object();
    let trace = log.repository().put_blob(SECRET).unwrap();
    let outcome = log
        .publish_layer(
            &op("publish-1"),
            &layer("agent"),
            head,
            v(0),
            &PublishAttribution::Agent {
                context: Some(trace),
                allow_missing_trace: false,
            },
        )
        .unwrap();
    assert!(matches!(outcome, PublishOutcome::Published { .. }));

    let t = now();
    let traces = log.traces();
    assert_eq!(
        code_of(traces.restrict(&gate, log.repo(), Some(&rita), t, trace)),
        "forbidden",
        "only Maintain restricts"
    );
    traces
        .restrict(&gate, log.repo(), Some(&maintainer), t, trace)
        .unwrap();
    assert_eq!(
        traces.visibility(trace).unwrap(),
        TraceVisibility::Restricted
    );
    for (who, token) in [
        ("anonymous", None),
        ("Read", Some(&bob)),
        ("Review", Some(&rita)),
    ] {
        assert_eq!(
            code_of(traces.read(&gate, log.repo(), token.map(String::as_str), t, trace)),
            "forbidden",
            "{who} reads a restricted trace"
        );
    }
    assert_eq!(
        traces
            .read(&gate, log.repo(), Some(&maintainer), t, trace)
            .unwrap(),
        SECRET
    );

    // The wire never serves it, while ordinary content is served.
    let (addr, server) = serve_then_return(log, &id, 2);
    let refused_get = call(addr, &id, &Message::Get(vec![(Kind::Blob, trace)])).unwrap();
    let served = call(addr, &id, &Message::Get(vec![(Kind::Blob, content)])).unwrap();
    let log = server.join().unwrap();
    assert!(matches!(refused_get, Message::Error { ref code, .. } if code == "forbidden"));
    assert!(matches!(served, Message::Objects(ref objects) if objects.len() == 1));

    // A fork by a reader without Maintain leaves the trace out and marks it.
    let reader_fork = TempDir::new().unwrap();
    let mut target = FileWorldLog::open(reader_fork.path()).unwrap();
    let forked = fork::fork(&log, &id, &mut target, v(1), Role::Read).unwrap();
    assert_eq!(forked.omitted, [trace]);
    assert_eq!(fork::omitted_traces(&target).unwrap(), [trace]);
    assert!(!target.repo().has_blob(trace).unwrap());
    assert!(target.repo().has_blob(content).unwrap());
    assert!(!tree_contains(reader_fork.path(), SECRET));
    drop(log);

    // The forker's own fork (Maintain in the source) keeps the trace, still restricted.
    let maintained = hosted
        .plane()
        .fork_repository(
            &hosted.session.access_token,
            &ForkRequest {
                source: id.clone(),
                at: 1,
                target: create_request("site-fork", true, Publish::Requests),
            },
            now(),
        )
        .unwrap();
    assert!(maintained.omitted.is_empty());
    let fork_log = hosted.log(&maintained.repository.repository);
    assert_eq!(
        fork_log.traces().visibility(trace).unwrap(),
        TraceVisibility::Restricted
    );
    let fork_gate = hosted.gate(&maintained.repository.repository);
    assert_eq!(
        code_of(
            fork_log
                .traces()
                .read(&fork_gate, fork_log.repo(), None, now(), trace)
        ),
        "forbidden",
        "anonymous read of the restricted trace in the public fork"
    );
    verified(
        "gate4.restricted-traces",
        "only Maintain restricts; anonymous, Read and Review callers get forbidden; the loopback \
         wire refuses Get for the trace (forbidden) while serving other content; a Read-role fork \
         omits and marks it and its bytes are absent from the fork's directory; the maintainer's \
         fork keeps it restricted",
    );
}

/// Whether any file under `dir` holds `needle` verbatim.
fn tree_contains(dir: &Path, needle: &[u8]) -> bool {
    fs::read_dir(dir).unwrap().flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            tree_contains(&path, needle)
        } else {
            fs::read(&path).is_ok_and(|bytes| bytes.windows(needle.len()).any(|w| w == needle))
        }
    })
}
