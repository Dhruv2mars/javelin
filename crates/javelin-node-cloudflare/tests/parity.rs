//! The node core on Durable Object storage answers the wire exactly as the self-hosted node
//! does. One scenario runs on a self-hosted data directory (`Directory`) and on the pages a
//! Durable Object keeps (`PageStorage` over `MemPages`): object uploads, `LayerAppend`, the
//! World-log commit path (`PublishLayer`, its replay and a reused operation), the writer
//! lease (`LayerHead`, `LayerTake`), reads that
//! follow the caller's audiences (#99), a write refused for a hidden path, a Publish Request
//! with its list, review and comment, a version held by an embargo until it is released, and
//! the browser, anonymous and outsider refusals. Every reply must match, with object IDs masked:
//! hidden entries are salted per store, so IDs over them differ between any two stores.

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
    auth::Gate,
    budget::{Budget, LARGEST_OBJECT, ROWS_WRITTEN_PER_DAY, STORAGE_BYTES},
    storage::{Coordination, Directory, Storage},
    wire,
};
use javelin_node_cloudflare::{pages::MemPages, vfs::PageStorage};
use javelin_protocol::{
    Message,
    codec::{Kind, LayerOutcome, PublishOutcome, RequestId, Usage, decode, encode},
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

/// A ten-minute token for `sub`, issued at `now` by the node's clock.
fn token(sub: &str, now: u64) -> String {
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
        let now = self.log.now();
        let token = caller.map(|sub| token(sub, now));
        let body = encode(message).unwrap();
        let answer =
            wire::handle(self.log, &self.gate, token.as_deref(), origin, now, &body).unwrap();
        let reply = decode(&answer.body).unwrap();
        self.seen
            .push(format!("cors={} {}", answer.cors, masked_use(&reply)));
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

/// The reply with the digits of a budget's numbers masked: each store counts its own pages,
/// so the use differs between stores while the decision must not.
fn masked_use(reply: &Message) -> String {
    let text = masked(reply);
    let counted = match reply {
        Message::Usage(_) => true,
        Message::Error { code, .. } => code == "quota_exceeded",
        _ => false,
    };
    if !counted {
        return text;
    }
    let mut out = String::new();
    let mut digits = false;
    for c in text.chars() {
        if c.is_ascii_digit() {
            if !digits {
                out.push('#');
            }
            digits = true;
        } else {
            digits = false;
            out.push(c);
        }
    }
    out
}

fn usage_of(s: &mut Session<'_>) -> Usage {
    match s.send(Some("owner"), &Message::ReadUsage) {
        Message::Usage(usage) => usage,
        other => panic!("usage refused: {other:?}"),
    }
}

fn used(usage: &Usage, budget: &str) -> u64 {
    usage
        .meters
        .iter()
        .find(|meter| meter.budget == budget)
        .and_then(|meter| meter.used)
        .unwrap_or_else(|| panic!("no use for {budget}: {usage:?}"))
}

/// The owner replaces the budget, as the admin route does: stored, then applied.
fn set(storage: &dyn Storage, s: &mut Session<'_>, budget: Budget) {
    admin::set_budget(storage, &budget).unwrap();
    s.log.set_budget(budget);
}

fn quota(reply: &Message) -> &str {
    match reply {
        Message::Error { code, detail } if code == "quota_exceeded" => detail,
        other => panic!("expected quota_exceeded, got {other:?}"),
    }
}

/// A budget a few KB above what the node holds refuses the crossing upload and Publish,
/// changes nothing, names the budget to members and gives maintainers the numbers, and
/// accepts the same writes once the owner raises it. A day's rows reset at midnight UTC.
fn budget_scenario(
    storage: &dyn Storage,
    log: &mut FileWorldLog,
    clock: &AtomicU64,
) -> Vec<String> {
    let mut s = Session {
        log,
        gate: gate(),
        seen: Vec::new(),
    };
    let seeded = s.layer_with("seed", 0, &[("src/a", b"a\n")]);
    s.send(Some("owner"), &publish("seed", "publish-seed", seeded, 0));
    let ready = s.layer_with("ready", 1, &[("src/b", b"b\n")]);

    // Only Maintain reads the node's use.
    let usage = usage_of(&mut s);
    for caller in [Some("member"), Some("stranger"), None] {
        let reply = s.send(caller, &Message::ReadUsage);
        assert!(matches!(&reply, Message::Error { .. }), "{reply:?}");
    }

    // A few KB of storage left: the upload and the Publish are refused, and nothing changes.
    let stored = used(&usage, STORAGE_BYTES);
    set(
        storage,
        &mut s,
        Budget {
            storage_bytes: stored + 4096,
            ..Budget::default()
        },
    );
    let big = vec![7u8; 16 << 10];
    let blob = (Kind::Blob, ObjectId::for_blob(&big), big.clone());
    let refused = s.send(Some("owner"), &Message::Objects(vec![blob.clone()]));
    assert!(
        quota(&refused).starts_with("storage_bytes: "),
        "{refused:?}"
    );
    let refused = s.send(Some("member"), &Message::Objects(vec![blob.clone()]));
    assert_eq!(quota(&refused), STORAGE_BYTES);
    let anonymous = s.send(None, &Message::Objects(vec![blob.clone()]));
    assert!(
        matches!(&anonymous, Message::Error { code, .. } if code == "unauthorized"),
        "{anonymous:?}"
    );
    let missing = s.send(Some("owner"), &Message::Want(vec![(blob.0, blob.1)]));
    assert_eq!(missing, Message::Missing(vec![(blob.0, blob.1)]));
    let refused = s.send(Some("owner"), &publish("ready", "publish-ready", ready, 1));
    quota(&refused);
    let head = s.send(Some("owner"), &Message::Resolve { version: None });
    assert!(
        format!("{head:?}").contains("number: VersionNumber(1)"),
        "{head:?}"
    );
    assert_eq!(s.log.head().unwrap().number, v(1));
    assert!(
        s.log.publication(&op("publish-ready")).unwrap().is_none(),
        "a refused Publish leaves no record"
    );

    // Raised: the same writes are accepted, the Publish under its first operation ID.
    set(storage, &mut s, Budget::default());
    let stored = s.send(Some("owner"), &Message::Objects(vec![blob.clone()]));
    assert!(matches!(&stored, Message::Stored { .. }), "{stored:?}");
    let published = s.send(Some("owner"), &publish("ready", "publish-ready", ready, 1));
    assert!(
        matches!(
            &published,
            Message::PublishOutcome(PublishOutcome::Published { number, .. }) if *number == v(2)
        ),
        "{published:?}"
    );

    // An object larger than the owner allows is refused whatever storage is left.
    set(
        storage,
        &mut s,
        Budget {
            largest_object: 4096,
            ..Budget::default()
        },
    );
    let other = vec![8u8; 16 << 10];
    let large = (Kind::Blob, ObjectId::for_blob(&other), other);
    let refused = s.send(Some("owner"), &Message::Objects(vec![large.clone()]));
    assert_eq!(
        quota(&refused),
        "largest_object: an object of 16384 bytes is larger than the 4096 allowed"
    );
    let refused = s.send(Some("member"), &Message::Objects(vec![large.clone()]));
    assert_eq!(quota(&refused), LARGEST_OBJECT);

    // Today's rows are spent: refused until midnight UTC, accepted the next day.
    let rows = used(&usage_of(&mut s), ROWS_WRITTEN_PER_DAY);
    assert!(rows > 0, "the writes above were counted");
    set(
        storage,
        &mut s,
        Budget {
            rows_written_per_day: rows + 16,
            ..Budget::default()
        },
    );
    let create = Message::LayerCreate {
        layer: layer("tomorrow"),
        base: v(2),
    };
    let refused = s.send(Some("owner"), &create);
    assert!(
        quota(&refused).starts_with("rows_written_per_day: "),
        "{refused:?}"
    );
    clock.fetch_add(86_400, Ordering::SeqCst);
    let created = s.send(Some("owner"), &create);
    assert!(
        matches!(
            &created,
            Message::LayerOutcome(LayerOutcome::Accepted { .. })
        ),
        "{created:?}"
    );
    let usage = usage_of(&mut s);
    assert!(used(&usage, ROWS_WRITTEN_PER_DAY) < rows, "{usage:?}");
    s.seen
}

fn budget_on(storage: &(impl Storage + Coordination), start: u64) -> Vec<String> {
    let clock = clock(start);
    let mut log = open(storage, &clock);
    budget_scenario(storage, &mut log, &clock)
}

#[test]
fn both_nodes_refuse_a_write_that_crosses_the_budget_and_accept_it_once_raised() {
    let dir = tempfile::tempdir().unwrap();
    let start = now();
    let native: Vec<_> = budget_on(&Directory::new(dir.path()), start);
    let object: Vec<_> = budget_on(&PageStorage::new(Rc::new(MemPages::default())), start);
    assert_eq!(native.len(), object.len());
    for (step, (a, b)) in native.iter().zip(&object).enumerate() {
        assert_eq!(a, b, "reply {step} differs");
    }
}

/// Page rows as a Durable Object writes them: every `put` and `set_size` is one row.
#[derive(Default)]
struct CountingPages {
    pages: MemPages,
    rows: std::cell::Cell<u64>,
}

impl javelin_node_cloudflare::pages::Pages for CountingPages {
    fn size(&self, file: &str) -> javelin_node_cloudflare::pages::Result<Option<u64>> {
        self.pages.size(file)
    }
    fn set_size(&self, file: &str, size: u64) -> javelin_node_cloudflare::pages::Result<()> {
        self.rows.set(self.rows.get() + 1);
        self.pages.set_size(file, size)
    }
    fn get(
        &self,
        file: &str,
        index: u64,
    ) -> javelin_node_cloudflare::pages::Result<Option<Vec<u8>>> {
        self.pages.get(file, index)
    }
    fn put(
        &self,
        file: &str,
        index: u64,
        bytes: &[u8],
    ) -> javelin_node_cloudflare::pages::Result<()> {
        self.rows.set(self.rows.get() + 1);
        self.pages.put(file, index, bytes)
    }
    fn remove(&self, file: &str) -> javelin_node_cloudflare::pages::Result<()> {
        self.pages.remove(file)
    }
}

/// The rows the node counts against its daily budget are the page rows the Durable Object
/// writes, with at most a few more: never fewer, so the budget never lets the quota through.
#[test]
fn the_rows_counted_are_the_rows_the_object_writes() {
    let pages = Rc::new(CountingPages::default());
    let storage = PageStorage::new(pages.clone());
    let clock = clock(now());
    let mut log = open(&storage, &clock);
    let mut s = Session {
        log: &mut log,
        gate: gate(),
        seen: Vec::new(),
    };
    let before = pages.rows.get();
    let seeded = s.layer_with("seed", 0, &[("src/a", b"a\n")]);
    s.send(Some("owner"), &publish("seed", "publish-seed", seeded, 0));
    let big = vec![7u8; 600 << 10];
    let blob = (Kind::Blob, ObjectId::for_blob(&big), big);
    s.send(Some("owner"), &Message::Objects(vec![blob]));
    let written = pages.rows.get() - before;
    let counted = used(&usage_of(&mut s), ROWS_WRITTEN_PER_DAY);
    assert!(
        written <= counted && counted <= written + written / 4,
        "counted {counted} rows for {written} written"
    );
    // One large upload on its own: each page that grows the file writes its size row too.
    let before = (pages.rows.get(), counted);
    let large = vec![9u8; 8 << 20];
    let blob = (Kind::Blob, ObjectId::for_blob(&large), large);
    s.send(Some("owner"), &Message::Objects(vec![blob]));
    let written = pages.rows.get() - before.0;
    let counted = used(&usage_of(&mut s), ROWS_WRITTEN_PER_DAY) - before.1;
    assert!(
        written <= counted && counted <= written + 8,
        "counted {counted} rows for {written} written"
    );
}

/// Files at `count` paths in nested directories, for a Publish that composes many trees.
fn many_files(name: &str, count: usize, prefix: &str) -> Vec<(String, Vec<u8>)> {
    (0..count)
        .map(|i| {
            (
                format!("{prefix}d{}/e{}/{name}{i}.txt", i % 10, i % 7),
                format!("{name} {i}\n").into_bytes(),
            )
        })
        .collect()
}

fn as_refs(files: &[(String, Vec<u8>)]) -> Vec<(&str, &[u8])> {
    files
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect()
}

/// A write the budget admits never ends past it: raising each limit from the use in growing
/// steps, the first limit that admits a many-object upload or a many-file Publish holds what it wrote.
/// A Publish replays its decided outcome however little budget is left.
fn bounds_scenario(storage: &dyn Storage, log: &mut FileWorldLog) {
    let mut s = Session {
        log,
        gate: gate(),
        seen: Vec::new(),
    };
    // Paths near the longest allowed, which the changeset and blame each store.
    let long = format!("{}/", vec!["p".repeat(250); 15].join("/"));
    for (meter, prefix) in [
        (STORAGE_BYTES, ""),
        (ROWS_WRITTEN_PER_DAY, ""),
        (STORAGE_BYTES, long.as_str()),
    ] {
        // Fresh writes for each meter, prepared under the default budget.
        set(storage, &mut s, Budget::default());
        let name = format!("wide-{}-{}", meter.len(), prefix.len());
        let files = many_files(&name, if prefix.is_empty() { 60 } else { 100 }, prefix);
        let wide = s.layer_with(&name, 0, &as_refs(&files));
        let blobs: Vec<_> = (0..60)
            .map(|i| {
                let bytes = format!("small {meter} {i}\n").into_bytes();
                (Kind::Blob, ObjectId::for_blob(&bytes), bytes)
            })
            .collect();
        let writes = [
            Message::Objects(blobs),
            publish(&name, &format!("publish-{name}"), wide, 0),
        ];
        let step = if meter == STORAGE_BYTES { 16 << 10 } else { 8 };
        for write in writes {
            let mut headroom = 0;
            loop {
                let limit = used(&usage_of(&mut s), meter) + headroom;
                let mut budget = Budget::default();
                if meter == STORAGE_BYTES {
                    budget.storage_bytes = limit;
                } else {
                    budget.rows_written_per_day = limit;
                }
                set(storage, &mut s, budget);
                let reply = s.send(Some("owner"), &write);
                if matches!(&reply, Message::Error { code, .. } if code == "quota_exceeded") {
                    headroom = (headroom + step).max(headroom * 3 / 2);
                    continue;
                }
                assert!(!matches!(reply, Message::Error { .. }), "{reply:?}");
                let after = used(&usage_of(&mut s), meter);
                assert!(
                    after <= limit,
                    "{meter}: admitted at {limit}, ended at {after}"
                );
                break;
            }
        }
        // With no budget left at all, the decided Publish replays, and a new Publish, even
        // one that would only record a refusal, writes nothing.
        set(
            storage,
            &mut s,
            Budget {
                rows_written_per_day: 0,
                storage_bytes: 0,
                ..Budget::default()
            },
        );
        let replay = s.send(
            Some("owner"),
            &publish(&name, &format!("publish-{name}"), wide, 0),
        );
        assert!(
            matches!(
                &replay,
                Message::PublishOutcome(PublishOutcome::Published { .. })
            ),
            "{replay:?}"
        );
        let before = usage_of(&mut s);
        let unknown = s.send(Some("owner"), &publish("none", "publish-none", wide, 0));
        quota(&unknown);
        assert_eq!(usage_of(&mut s), before, "a refused Publish writes nothing");
    }
}

/// A Publish Request refused by the budget leaves nothing behind, not even its revision's
/// Layer, and publishes once the budget is raised.
fn request_scenario(storage: &dyn Storage, log: &mut FileWorldLog, page: u64) {
    let mut s = Session {
        log,
        gate: gate(),
        seen: Vec::new(),
    };
    let files = many_files("r", 100, "");
    let checkpoint = s.layer_with("review", 0, &as_refs(&files));
    let request = RequestId::parse("pr-budget").unwrap();
    let opened = s.send(
        Some("owner"),
        &Message::OpenRequest {
            operation: op("open-budget"),
            request: request.clone(),
            layer: layer("review"),
            checkpoint,
            base: v(0),
        },
    );
    assert!(matches!(&opened, Message::ReviewOutcome(_)), "{opened:?}");
    let publish = Message::PublishRequest {
        operation: op("publish-budget"),
        request,
        revision: 1,
        base: v(0),
    };
    // Room for the request's own records (16 pages), not for a Publish (64 pages and more).
    let usage = usage_of(&mut s);
    set(
        storage,
        &mut s,
        Budget {
            storage_bytes: used(&usage, STORAGE_BYTES) + 40 * page,
            ..Budget::default()
        },
    );
    let before = usage_of(&mut s);
    let refused = s.send(Some("owner"), &publish);
    quota(&refused);
    assert_eq!(
        usage_of(&mut s),
        before,
        "a refused Publish Request writes nothing, its revision's Layer included"
    );
    set(storage, &mut s, Budget::default());
    let published = s.send(Some("owner"), &publish);
    assert!(
        matches!(
            &published,
            Message::PublishOutcome(PublishOutcome::Published { .. })
        ),
        "{published:?}"
    );
}

#[test]
fn a_publish_request_past_the_budget_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let native = Directory::new(dir.path());
    request_scenario(&native, &mut open(&native, &clock(now())), 4096);
    let object = PageStorage::new(Rc::new(MemPages::default()));
    request_scenario(&object, &mut open(&object, &clock(now())), 64 << 10);
}

#[test]
fn an_admitted_write_ends_within_the_budget_and_a_publish_replays_past_it() {
    let dir = tempfile::tempdir().unwrap();
    let native = Directory::new(dir.path());
    bounds_scenario(&native, &mut open(&native, &clock(now())));
    let object = PageStorage::new(Rc::new(MemPages::default()));
    bounds_scenario(&object, &mut open(&object, &clock(now())));
}

/// Outcomes that record paths but make no version are bounded too: a Publish that conflicts
/// over many long paths, directly or through a Publish Request whose revision Layer is new,
/// is refused before anything is written, and recorded once the budget is raised. A fresh
/// node's first Publish Request, refused, writes nothing either.
fn conflict_scenario(storage: &dyn Storage, log: &mut FileWorldLog, page: u64) {
    let mut s = Session {
        log,
        gate: gate(),
        seen: Vec::new(),
    };
    set(
        storage,
        &mut s,
        Budget {
            rows_written_per_day: 0,
            storage_bytes: 0,
            ..Budget::default()
        },
    );
    // The first request stamps World 0's disclosure time, a read's only write.
    usage_of(&mut s);
    let before = usage_of(&mut s);
    let first = Message::PublishRequest {
        operation: op("publish-first"),
        request: RequestId::parse("pr-none").unwrap(),
        revision: 1,
        base: v(0),
    };
    quota(&s.send(Some("owner"), &first));
    assert_eq!(
        usage_of(&mut s),
        before,
        "a fresh node's refused request writes nothing"
    );

    set(storage, &mut s, Budget::default());
    let long = format!("{}/", vec!["c".repeat(250); 15].join("/"));
    let ours = many_files("x", 100, &long);
    let theirs: Vec<_> = ours
        .iter()
        .map(|(path, _)| (path.clone(), b"theirs\n".to_vec()))
        .collect();
    let first = s.layer_with("ours", 0, &as_refs(&ours));
    let second = s.layer_with("theirs", 0, &as_refs(&theirs));
    let request = RequestId::parse("pr-conflict").unwrap();
    let opened = s.send(
        Some("owner"),
        &Message::OpenRequest {
            operation: op("open-conflict"),
            request: request.clone(),
            layer: layer("theirs"),
            checkpoint: second,
            base: v(0),
        },
    );
    assert!(matches!(&opened, Message::ReviewOutcome(_)), "{opened:?}");
    let published = s.send(Some("owner"), &publish("ours", "publish-ours", first, 0));
    assert!(
        matches!(
            &published,
            Message::PublishOutcome(PublishOutcome::Published { .. })
        ),
        "{published:?}"
    );

    let conflicts = [
        publish("theirs", "publish-theirs", second, 0),
        Message::PublishRequest {
            operation: op("publish-conflict"),
            request,
            revision: 1,
            base: v(0),
        },
    ];
    for conflict in conflicts {
        // Room for the outcome's request (16 pages), not for its paths (64 pages and more).
        let usage = usage_of(&mut s);
        set(
            storage,
            &mut s,
            Budget {
                storage_bytes: used(&usage, STORAGE_BYTES) + 40 * page,
                ..Budget::default()
            },
        );
        let before = usage_of(&mut s);
        quota(&s.send(Some("owner"), &conflict));
        assert_eq!(
            usage_of(&mut s),
            before,
            "a refused conflict writes nothing"
        );
        set(storage, &mut s, Budget::default());
        let recorded = s.send(Some("owner"), &conflict);
        assert!(
            matches!(
                &recorded,
                Message::PublishOutcome(PublishOutcome::Conflict { .. })
            ),
            "{recorded:?}"
        );
    }
}

#[test]
fn outcomes_without_a_version_are_bounded_before_they_are_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let native = Directory::new(dir.path());
    conflict_scenario(&native, &mut open(&native, &clock(now())), 4096);
    let object = PageStorage::new(Rc::new(MemPages::default()));
    conflict_scenario(&object, &mut open(&object, &clock(now())), 64 << 10);
}
