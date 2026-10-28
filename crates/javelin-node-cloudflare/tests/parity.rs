//! The node core on Durable Object storage answers the wire exactly as the self-hosted node
//! does. One scenario runs on a self-hosted data directory (`Directory`) and on the pages a
//! Durable Object keeps (`PageStorage` over `MemPages`): object uploads, `LayerAppend`, the
//! World-log commit path (`PublishLayer`, its replay and a reused operation), the writer
//! lease (`LayerHead`, `LayerTake`), reads that
//! follow the caller's audiences (#99), a write refused for a hidden path, a Publish Request
//! with its list, review and comment, a version held by an embargo until it is released, and
//! the browser, anonymous and outsider refusals. Every reply must match, with object IDs masked:
//! hidden entries are salted per store, so IDs over them differ between any two stores. A
//! second scenario pushes role changes while each node serves, through the Durable Object's
//! own admin route, and the next request must be decided the same way on both.

use std::{
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use javelin::{
    FileEntry, LayerId, ObjectId, OperationId, POLICY_PATH, PublishAttribution, RepoPath,
    SparseChanges, VersionNumber,
};
use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_identity::{Scope, test_keys};
use javelin_node::{
    FileWorldLog, admin,
    auth::{Gate, LiveGate},
    storage::{Coordination, Directory, Storage},
    wire,
};
use javelin_node_cloudflare::{pages::MemPages, service, vfs::PageStorage};
use javelin_protocol::{
    Message,
    codec::{Kind, LayerOutcome, PublishOutcome, RequestId, decode, encode},
};

const REPO: &str = "acme-app";
const POLICY: &[u8] = b".env maintainers\n";
const SECRET: &[u8] = b"TOKEN=1\n";

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn layer(s: &str) -> LayerId {
    LayerId::parse(s).unwrap()
}
fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}

/// A public repository: `owner` holds Maintain, `member` holds Read.
fn gate() -> Gate {
    let mut gate = Gate::new(RepositoryId::parse(REPO).unwrap(), test_keys::keys());
    gate.apply(AppliedPermissions {
        repository: RepositoryId::parse(REPO).unwrap(),
        epoch: 1,
        visibility: Visibility::Public,
        roles: [
            (PrincipalId::parse("owner").unwrap(), Role::Maintain),
            (PrincipalId::parse("member").unwrap(), Role::Read),
        ]
        .into_iter()
        .collect(),
        required_approvals: 0,
    })
    .unwrap();
    gate
}

fn token(sub: &str) -> String {
    let now = now();
    let scopes = [
        Scope::Read,
        Scope::LayerWrite,
        Scope::Publish,
        Scope::ReviewWrite,
        Scope::ReviewApprove,
    ];
    let claims = test_keys::claims(sub, REPO, &scopes, now, now + 600, &format!("{sub}-jti"));
    test_keys::issuer().issue(&claims).unwrap()
}

/// The reply with every object ID replaced by `<id>`, including the hex IDs in a
/// changeset's JSON.
fn masked(reply: &Message) -> String {
    let mut text = match reply {
        Message::ChangesetPart { total, bytes } => {
            let json = String::from_utf8_lossy(bytes);
            let mut out = String::new();
            let mut run = String::new();
            for c in json.chars().chain(['\0']) {
                if c.is_ascii_hexdigit() {
                    run.push(c);
                    continue;
                }
                out.push_str(if run.len() == 64 { "<id>" } else { &run });
                run.clear();
                out.push(c);
            }
            format!("ChangesetPart {{ total: {total}, json: {out} }}")
        }
        other => format!("{other:?}"),
    };
    while let Some(at) = text.find("ObjectId([") {
        let end = at + text[at..].find("])").unwrap() + 2;
        text.replace_range(at..end, "<id>");
    }
    text
}

struct Session<'a> {
    log: &'a mut FileWorldLog,
    gate: Gate,
    seen: Vec<String>,
}

impl Session<'_> {
    /// Sends `message` as `caller` (`None` is anonymous), from a browser when `origin`.
    fn send_from(&mut self, caller: Option<&str>, origin: bool, message: &Message) -> Message {
        let token = caller.map(token);
        let body = encode(message).unwrap();
        let now = self.log.now();
        let answer =
            wire::handle(self.log, &self.gate, token.as_deref(), origin, now, &body).unwrap();
        let reply = decode(&answer.body).unwrap();
        self.seen
            .push(format!("cors={} {}", answer.cors, masked(&reply)));
        reply
    }

    fn send(&mut self, caller: Option<&str>, message: &Message) -> Message {
        self.send_from(caller, false, message)
    }

    /// Uploads `files` and their changes object as `caller`; returns the changes' ID.
    fn upload(&mut self, caller: &str, files: &[(&str, &[u8])]) -> ObjectId {
        let blobs: Vec<_> = files
            .iter()
            .map(|(_, bytes)| (Kind::Blob, ObjectId::for_blob(bytes), bytes.to_vec()))
            .collect();
        self.send(
            Some(caller),
            &Message::Want(blobs.iter().map(|(k, id, _)| (*k, *id)).collect()),
        );
        self.send(Some(caller), &Message::Objects(blobs));
        let changes = SparseChanges::new(files.iter().map(|(path, bytes)| {
            (
                RepoPath::parse(path).unwrap(),
                Some(FileEntry::Regular {
                    content: ObjectId::for_blob(bytes),
                    executable: false,
                }),
            )
        }))
        .unwrap();
        let bytes = changes.canonical_bytes();
        let id = Kind::Checkpoint.id(&bytes);
        self.send(Some(caller), &Message::Want(vec![(Kind::Checkpoint, id)]));
        self.send(
            Some(caller),
            &Message::Objects(vec![(Kind::Checkpoint, id, bytes)]),
        );
        id
    }

    /// Creates Layer `name` over `base` and appends `files`, both over the wire as `owner`.
    fn layer_with(&mut self, name: &str, base: u64, files: &[(&str, &[u8])]) -> ObjectId {
        let create = Message::LayerCreate {
            layer: layer(name),
            base: v(base),
        };
        let head = match self.send(Some("owner"), &create) {
            Message::LayerOutcome(LayerOutcome::Accepted { checkpoint, .. }) => checkpoint,
            other => panic!("create refused: {other:?}"),
        };
        let changes = self.upload("owner", files);
        let append = Message::LayerAppend {
            operation: op(&format!("append-{name}")),
            layer: layer(name),
            parent: Some(head),
            checkpoint: changes,
        };
        match self.send(Some("owner"), &append) {
            Message::LayerOutcome(LayerOutcome::Accepted { checkpoint, .. }) => checkpoint,
            other => panic!("append refused: {other:?}"),
        }
    }
}

fn publish(name: &str, operation: &str, expected: ObjectId, base: u64) -> Message {
    Message::PublishLayer {
        operation: op(operation),
        layer: layer(name),
        expected,
        base: v(base),
        attribution: PublishAttribution::Human,
    }
}

/// The scenario's replies, in order. `clock` is the node's time, which the scenario moves.
fn scenario(log: &mut FileWorldLog, clock: &AtomicU64) -> Vec<String> {
    let mut s = Session {
        log,
        gate: gate(),
        seen: Vec::new(),
    };
    s.send(None, &Message::Hello { min: 2, max: 2 });
    s.send(Some("owner"), &Message::Permissions);
    s.send(None, &Message::Resolve { version: None });

    // The commit path: World 1 sets the policy, a maintainers' `.env` and a public file.
    let work = s.layer_with(
        "work",
        0,
        &[(POLICY_PATH, POLICY), (".env", SECRET), ("src/a", b"a\n")],
    );
    s.send(Some("owner"), &publish("work", "publish-work", work, 0));
    s.send(Some("owner"), &publish("work", "publish-work", work, 0));
    s.send(Some("owner"), &publish("work", "publish-work", work, 1));
    s.send(None, &Message::Resolve { version: None });
    s.send(Some("owner"), &Message::Resolve { version: None });

    // Reads follow the caller's audiences: `.env` is absent to everyone below Maintain.
    let secret = vec![(Kind::Blob, ObjectId::for_blob(SECRET))];
    s.send(None, &Message::Get(secret.clone()));
    s.send(Some("member"), &Message::Get(secret.clone()));
    s.send(Some("owner"), &Message::Get(secret));
    s.send(
        Some("member"),
        &Message::Changeset {
            version: v(1),
            offset: 0,
        },
    );
    s.send(
        Some("owner"),
        &Message::Changeset {
            version: v(1),
            offset: 0,
        },
    );

    // A member's write that touches `.env` is refused, naming no path.
    let head = s.log.create_layer(&layer("leak"), v(1)).unwrap();
    let leak = s.upload("member", &[(".env", b"TOKEN=2\n")]);
    s.send(
        Some("member"),
        &Message::LayerAppend {
            operation: op("append-leak"),
            layer: layer("leak"),
            parent: Some(head.checkpoint().object()),
            checkpoint: leak,
        },
    );
    // Its staged changes stay unknown to another caller.
    s.send(
        Some("owner"),
        &Message::Want(vec![(Kind::Checkpoint, leak)]),
    );

    // A Publish Request publishes World 2.
    let feature = s.layer_with("feature", 1, &[("src/b", b"b\n")]);
    // The first append took the Layer's writer lease for the owner; only the owner moves it.
    s.send(
        Some("owner"),
        &Message::LayerHead {
            layer: layer("feature"),
        },
    );
    for caller in ["member", "owner"] {
        s.send(
            Some(caller),
            &Message::LayerTake {
                operation: op(&format!("take-{caller}")),
                layer: layer("feature"),
            },
        );
    }

    let request = RequestId::parse("pr-1").unwrap();
    s.send(
        Some("owner"),
        &Message::OpenRequest {
            operation: op("open-1"),
            request: request.clone(),
            layer: layer("feature"),
            checkpoint: feature,
            base: v(1),
        },
    );
    s.send(
        Some("owner"),
        &Message::PublishRequest {
            operation: op("publish-pr-1"),
            request,
            revision: 1,
            base: v(1),
        },
    );
    s.send(None, &Message::Resolve { version: None });

    // The request loop's reads: the list and the review, for a member and an outsider, and a
    // comment on the published request.
    let request = RequestId::parse("pr-1").unwrap();
    s.send(
        Some("owner"),
        &Message::Comment {
            operation: op("comment-1"),
            request: request.clone(),
            revision: Some(1),
            body: "shipped".into(),
        },
    );
    for caller in [Some("owner"), Some("member"), Some("stranger"), None] {
        s.send(caller, &Message::ListRequests { after: None });
        s.send(
            caller,
            &Message::ReadReview {
                request: request.clone(),
                comments_from: 0,
            },
        );
    }

    // A held version: World 3 is embargoed for an hour, so outsiders stay at World 2 until a
    // maintainer releases it, while members read the head.
    s.send(Some("owner"), &Message::SetDelay { delay: 0 });
    s.send(
        Some("member"),
        &Message::Embargo {
            operation: op("publish-held"),
            until: None,
        },
    );
    let until = s.log.now() + 3600;
    s.send(
        Some("owner"),
        &Message::Embargo {
            operation: op("publish-held"),
            until: Some(until),
        },
    );
    let held = s.layer_with("held", 2, &[("src/c", b"c\n")]);
    s.send(Some("owner"), &publish("held", "publish-held", held, 2));
    for caller in [None, Some("member")] {
        s.send(caller, &Message::Resolve { version: None });
        s.send(caller, &Message::ReadDisclosure);
    }
    s.send(
        None,
        &Message::Resolve {
            version: Some(v(3)),
        },
    );
    // A signed-in caller without a role is refused Layer work before anything is looked up.
    s.send(
        Some("stranger"),
        &Message::LayerCreate {
            layer: layer("outside"),
            base: v(2),
        },
    );
    clock.fetch_add(60, Ordering::SeqCst);
    s.send(Some("owner"), &Message::Release { version: v(3) });
    s.send(None, &Message::Resolve { version: None });

    // Refusals before any handler: a browser write, an anonymous write, an unknown Layer.
    s.send_from(
        Some("owner"),
        true,
        &publish("feature", "publish-x", feature, 1),
    );
    s.send(None, &publish("feature", "publish-y", feature, 1));
    s.send(
        Some("owner"),
        &Message::LayerHead {
            layer: layer("none"),
        },
    );
    s.seen
}

/// A clock from `start`, which the scenario moves forward. Start near the real time: tokens
/// are issued at the real time and stay valid for ten minutes.
fn clock(start: u64) -> Arc<AtomicU64> {
    Arc::new(AtomicU64::new(start))
}

fn open(storage: &(impl Storage + Coordination), clock: &Arc<AtomicU64>) -> FileWorldLog {
    let clock = clock.clone();
    FileWorldLog::on(
        storage,
        storage,
        Box::new(move || clock.load(Ordering::SeqCst)),
    )
    .unwrap()
}

fn run_on(storage: &(impl Storage + Coordination), start: u64) -> Vec<String> {
    let clock = clock(start);
    scenario(&mut open(storage, &clock), &clock)
}

#[test]
fn the_durable_object_node_answers_as_the_self_hosted_node() {
    let dir = tempfile::tempdir().unwrap();
    let start = now();
    let native = run_on(&Directory::new(dir.path()), start);
    let object = run_on(&PageStorage::new(Rc::new(MemPages::default())), start);
    assert!(
        native.iter().any(|reply| reply.contains("Published")),
        "{native:#?}"
    );
    for (step, (a, b)) in native.iter().zip(&object).enumerate() {
        assert_eq!(a, b, "reply {step} differs");
    }
    assert_eq!(native.len(), object.len());
}

#[test]
fn a_reopened_node_keeps_what_it_committed() {
    let pages = Rc::new(MemPages::default());
    let first = PageStorage::new(pages.clone());
    run_on(&first, now());
    drop(first);
    let storage = PageStorage::new(pages);
    let mut log = open(&storage, &clock(now()));
    assert_eq!(log.head().unwrap().number, v(3));
    let mut s = Session {
        log: &mut log,
        gate: gate(),
        seen: Vec::new(),
    };
    // The replay comes from the decided record, not a new decision.
    let replay = s.send(
        Some("owner"),
        &publish("work", "publish-work", ObjectId::for_blob(b"x"), 0),
    );
    assert!(
        matches!(
            &replay,
            Message::PublishOutcome(PublishOutcome::Refused { code }) if code == "operation_id_reused"
        ),
        "{replay:?}"
    );
}

#[test]
fn a_second_connection_to_an_open_database_is_refused() {
    let storage = PageStorage::new(Rc::new(MemPages::default()));
    let _log = open(&storage, &clock(now()));
    let error = storage.records().expect_err("node.db is open");
    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy),
        "{error}"
    );
}

/// The permissions of private repository `REPO` at `epoch`: `owner` holds Maintain and
/// `member` holds `member`, in the JSON layout the control plane pushes.
fn private(epoch: u64, member: Option<Role>) -> AppliedPermissions {
    let mut roles = vec![(PrincipalId::parse("owner").unwrap(), Role::Maintain)];
    if let Some(role) = member {
        roles.push((PrincipalId::parse("member").unwrap(), role));
    }
    AppliedPermissions {
        repository: RepositoryId::parse(REPO).unwrap(),
        epoch,
        visibility: Visibility::Private,
        roles: roles.into_iter().collect(),
        required_approvals: 0,
    }
}

fn permissions_json(permissions: &AppliedPermissions) -> Vec<u8> {
    let roles: serde_json::Map<String, serde_json::Value> = permissions
        .roles
        .iter()
        .map(|(principal, role)| (principal.as_str().to_string(), role.name().into()))
        .collect();
    serde_json::json!({
        "repository": permissions.repository.as_str(),
        "epoch": permissions.epoch,
        "public": permissions.visibility == Visibility::Public,
        "roles": roles,
        "required_approvals": permissions.required_approvals,
    })
    .to_string()
    .into_bytes()
}

/// A node that serves while permissions are pushed to it.
trait Serving {
    /// The push's outcome: `applied`, or the refusal's code.
    fn push(&mut self, permissions: &AppliedPermissions) -> String;
    fn send(&mut self, caller: &str, message: &Message) -> String;
}

/// The self-hosted node: its data directory, read by `LiveGate` for each request, as
/// `javelin-node serve` reads it, and pushed to with `admin::apply`, as `LocalNode` pushes.
struct SelfHosted {
    storage: Directory,
    log: FileWorldLog,
    gate: LiveGate,
}

impl Serving for SelfHosted {
    fn push(&mut self, permissions: &AppliedPermissions) -> String {
        match admin::apply(&self.storage, permissions) {
            Ok(_) => "applied".into(),
            Err(admin::ApplyError::Refused(refusal)) => refusal.code,
            Err(admin::ApplyError::Storage(error)) => panic!("{error}"),
        }
    }

    fn send(&mut self, caller: &str, message: &Message) -> String {
        let gate = self.gate.current(&self.storage).unwrap();
        let now = self.log.now();
        let token = token(caller);
        let answer = wire::handle(
            &mut self.log,
            gate,
            Some(&token),
            false,
            now,
            &encode(message).unwrap(),
        )
        .unwrap();
        masked(&decode(&answer.body).unwrap())
    }
}

/// The Durable Object's service, reached only through its admin routes and protocol
/// version 2, as the Worker reaches it.
struct DurableObject(service::Node<PageStorage>);

impl Serving for DurableObject {
    fn push(&mut self, permissions: &AppliedPermissions) -> String {
        let answer = self.0.admin("permissions", &permissions_json(permissions));
        let body: serde_json::Value = serde_json::from_slice(&answer.body).unwrap();
        match answer.status {
            200 => "applied".into(),
            _ => body["error"].as_str().unwrap().into(),
        }
    }

    fn send(&mut self, caller: &str, message: &Message) -> String {
        let authorization = format!("Bearer {}", token(caller));
        let answer = self
            .0
            .v2(Some(&authorization), false, &encode(message).unwrap());
        assert_eq!(answer.status, 200, "{answer:?}");
        masked(&decode(&answer.body).unwrap())
    }
}

/// Requests and pushes, interleaved; the replies and push outcomes in order.
fn role_changes(node: &mut dyn Serving) -> Vec<String> {
    let mut seen = Vec::new();
    let create = |name: &str| Message::LayerCreate {
        layer: layer(name),
        base: v(0),
    };
    let list = Message::ListRequests { after: None };
    // Only Maintain sets the disclosure delay.
    let maintain = Message::SetDelay { delay: 0 };
    seen.push(node.push(&private(1, Some(Role::Read))));
    seen.push(node.send("member", &maintain));
    seen.push(node.send("member", &list));
    seen.push(node.send("member", &create("member-a")));
    // Removed while serving: refused from the next request on.
    seen.push(node.push(&private(2, None)));
    seen.push(node.send("member", &maintain));
    seen.push(node.send("member", &list));
    seen.push(node.send("member", &create("member-b")));
    // A stale push is refused and changes nothing.
    seen.push(node.push(&private(2, Some(Role::Maintain))));
    seen.push(node.send("member", &list));
    // Granted Maintain: the next request holds it.
    seen.push(node.push(&private(3, Some(Role::Maintain))));
    seen.push(node.send("member", &maintain));
    seen.push(node.send("member", &create("member-c")));
    seen.push(node.send("owner", &maintain));
    seen
}

#[test]
fn a_role_change_applies_from_the_next_request_on_both_nodes() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Directory::new(dir.path());
    javelin_node::auth::store_keys(&storage, &test_keys::keys()).unwrap();
    let log = FileWorldLog::on(&storage, &storage, Box::new(now)).unwrap();
    let native = role_changes(&mut SelfHosted {
        storage,
        log,
        gate: LiveGate::new(REPO),
    });

    let mut object = service::Node::new(
        RepositoryId::parse(REPO).unwrap(),
        PageStorage::new(Rc::new(MemPages::default())),
        now,
    );
    let (id, tree) = admin::genesis();
    let provision =
        serde_json::json!({"genesis": {"id": id.to_string(), "tree": tree.to_string()}});
    assert_eq!(
        object
            .admin("provision", provision.to_string().as_bytes())
            .status,
        200
    );
    assert_eq!(
        object.admin("keys", &test_keys::keys().to_json()).status,
        200
    );
    let durable = role_changes(&mut DurableObject(object));

    assert_eq!(native[5], durable[5]);
    assert!(native[5].contains("forbidden"), "{native:#?}");
    assert_eq!(native[8], "stale_permissions");
    assert!(native[1].contains("forbidden"), "{native:#?}");
    assert!(native[11].contains("Disclosure"), "{native:#?}");
    for (step, (a, b)) in native.iter().zip(&durable).enumerate() {
        assert_eq!(a, b, "step {step} differs");
    }
    assert_eq!(native.len(), durable.len());
}
