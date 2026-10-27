//! The node core on Durable Object storage answers the wire exactly as the self-hosted node
//! does. One scenario runs on a self-hosted data directory (`Directory`) and on the pages a
//! Durable Object keeps (`PageStorage` over `MemPages`): object uploads, `LayerAppend`, the
//! World-log commit path (`PublishLayer`, its replay and a reused operation), reads that
//! follow the caller's audiences (#99), a write refused for a hidden path, a Publish Request,
//! and the browser and anonymous refusals. Every reply must match, with object IDs masked:
//! hidden entries are salted per store, so IDs over them differ between any two stores.

use std::{
    rc::Rc,
    time::{SystemTime, UNIX_EPOCH},
};

use javelin::{
    FileEntry, LayerId, ObjectId, OperationId, POLICY_PATH, PublishAttribution, RepoPath,
    SparseChanges, VersionNumber,
};
use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_identity::{Scope, test_keys};
use javelin_node::{
    FileWorldLog,
    auth::Gate,
    storage::{Coordination, Directory, Storage},
    wire,
};
use javelin_node_cloudflare::{pages::MemPages, vfs::PageStorage};
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
        let answer =
            wire::handle(self.log, &self.gate, token.as_deref(), origin, now(), &body).unwrap();
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

    /// Creates Layer `name` over `base` and appends `files` over the wire as `owner`.
    fn layer_with(&mut self, name: &str, base: u64, files: &[(&str, &[u8])]) -> ObjectId {
        let head = self.log.create_layer(&layer(name), v(base)).unwrap();
        let changes = self.upload("owner", files);
        let append = Message::LayerAppend {
            operation: op(&format!("append-{name}")),
            layer: layer(name),
            parent: Some(head.checkpoint().object()),
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

/// The scenario's replies, in order.
fn scenario(log: &mut FileWorldLog) -> Vec<String> {
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

fn run_on(storage: &(impl Storage + Coordination)) -> Vec<String> {
    let mut log = FileWorldLog::on(storage, storage).unwrap();
    scenario(&mut log)
}

#[test]
fn the_durable_object_node_answers_as_the_self_hosted_node() {
    let dir = tempfile::tempdir().unwrap();
    let native = run_on(&Directory::new(dir.path()));
    let object = run_on(&PageStorage::new(Rc::new(MemPages::default())));
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
    run_on(&first);
    drop(first);
    let storage = PageStorage::new(pages);
    let mut log = FileWorldLog::on(&storage, &storage).unwrap();
    assert_eq!(log.head().unwrap().number, v(2));
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
    let _log = FileWorldLog::on(&storage, &storage).unwrap();
    let error = storage.records().expect_err("node.db is open");
    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy),
        "{error}"
    );
}
