//! A fork between two Durable Objects copies what a fork on one self-hosted node copies. The
//! self-hosted node forks with `fork::fork_for` over two data directories (`LocalNode::fork`);
//! the Durable Object node forks as the Worker does (`tools/node-cloudflare/src/worker.js`):
//! `fork-finished` on the target, `fork-export` on the source, `fork-import` on the target,
//! each object on its own pages. Hidden entries stay hidden for the readers outside their
//! audience, traces follow the reader, a fork past the target's budget is refused before it
//! writes, and an import that fails part-way is rolled back whole, as the object's
//! `transactionSync` rolls back a call marked `rollback`.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
    time::{SystemTime, UNIX_EPOCH},
};

use javelin::{
    FileEntry, LayerId, ObjectId, OperationId, POLICY_PATH, PublishAttribution, Reader, RepoPath,
    SparseChanges, VersionNumber,
};
use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_identity::{Scope, test_keys};
use javelin_node::{
    FileWorldLog, TraceVisibility, admin,
    auth::Gate,
    budget::Budget,
    fork,
    storage::{Directory, Storage},
};
use javelin_node_cloudflare::{
    pages::{PAGE, Pages, Result as PagesResult},
    service::{self, Response},
    vfs::PageStorage,
};
use javelin_protocol::{codec::PublishOutcome, receiver::WorldLog};
use serde_json::{Value, json};

const SOURCE: &str = "acme-app";
const TARGET: &str = "acme-fork";
const POLICY: &[u8] = b".env maintainers\ninternal/** members\n";
const SECRETS: [&[u8]; 3] = [b"TOKEN=1\n", b"TOKEN=2\n", b"notes\n"];
const PUBLIC_TRACE: &[u8] = b"public reasoning";
const SECRET_TRACE: &[u8] = b"secret reasoning";

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}

/// Each file's size and pages.
type Files = BTreeMap<String, (u64, BTreeMap<u64, Vec<u8>>)>;

/// Page rows with a Durable Object's transaction around each call: `begin` keeps the rows,
/// `rollback` restores them. A put or size write past `fail_after` fails, as a storage call
/// can fail; `puts` counts the writes.
#[derive(Default)]
struct TxPages {
    files: RefCell<Files>,
    saved: RefCell<Option<Files>>,
    puts: Cell<u64>,
    fail_after: Cell<Option<u64>>,
    failed: Cell<bool>,
}

impl TxPages {
    fn begin(&self) {
        *self.saved.borrow_mut() = Some(self.files.borrow().clone());
        self.puts.set(0);
        self.failed.set(false);
    }

    fn commit(&self) {
        self.saved.borrow_mut().take();
    }

    fn rollback(&self) {
        let saved = self.saved.borrow_mut().take().expect("a transaction");
        *self.files.borrow_mut() = saved;
    }

    fn write(&self) -> PagesResult<()> {
        self.puts.set(self.puts.get() + 1);
        if self.fail_after.get().is_some_and(|n| self.puts.get() > n) {
            self.failed.set(true);
            return Err("injected storage failure".into());
        }
        Ok(())
    }
}

impl Pages for TxPages {
    fn size(&self, file: &str) -> PagesResult<Option<u64>> {
        Ok(self.files.borrow().get(file).map(|(size, _)| *size))
    }
    fn set_size(&self, file: &str, size: u64) -> PagesResult<()> {
        self.write()?;
        let mut files = self.files.borrow_mut();
        let entry = files.entry(file.into()).or_default();
        entry.0 = size;
        entry.1.retain(|index, _| index * (PAGE as u64) < size);
        Ok(())
    }
    fn get(&self, file: &str, index: u64) -> PagesResult<Option<Vec<u8>>> {
        Ok(self
            .files
            .borrow()
            .get(file)
            .and_then(|(_, pages)| pages.get(&index).cloned()))
    }
    fn put(&self, file: &str, index: u64, bytes: &[u8]) -> PagesResult<()> {
        self.write()?;
        let mut files = self.files.borrow_mut();
        files
            .entry(file.into())
            .or_default()
            .1
            .insert(index, bytes.to_vec());
        Ok(())
    }
    fn remove(&self, file: &str) -> PagesResult<()> {
        self.files.borrow_mut().remove(file);
        Ok(())
    }
}

/// One Durable Object: its pages, and the node the Worker opens on them.
struct Object {
    pages: Rc<TxPages>,
    name: &'static str,
}

impl Object {
    fn new(name: &'static str) -> Self {
        Self {
            pages: Rc::new(TxPages::default()),
            name,
        }
    }

    fn node(&self) -> service::Node<PageStorage> {
        service::Node::new(
            RepositoryId::parse(self.name).unwrap(),
            PageStorage::new(self.pages.clone()),
            now,
        )
    }

    /// One admin call in one transaction, as `RepositoryNode.call` makes it: a failed storage
    /// call throws (`worker.rs`'s `settle`), and so does a reply marked `rollback`; either way
    /// the call's writes are rolled back.
    fn call(&self, action: &str, body: &[u8]) -> Response {
        self.pages.begin();
        let reply = self.node().admin(action, body);
        if self.pages.failed.get() || reply.rollback {
            self.pages.rollback();
        } else {
            self.pages.commit();
        }
        reply
    }

    fn log(&self) -> FileWorldLog {
        let storage = PageStorage::new(self.pages.clone());
        FileWorldLog::on(&storage, &storage, Box::new(now)).unwrap()
    }
}

fn json_of(reply: &Response) -> Value {
    serde_json::from_slice(&reply.body).unwrap()
}

/// The Worker's `fork`: the target's finished fork, or the source's bundle imported into it.
/// The answer as `(status, body)`, and the target's writes in its import.
fn object_fork(source: &Object, target: &Object, at: u64, reader: Option<Role>) -> (u16, Value) {
    let finished = target.call(
        "fork-finished",
        json!({ "source": source.name, "at": at })
            .to_string()
            .as_bytes(),
    );
    if finished.status != 200 {
        return (finished.status, json_of(&finished));
    }
    if !json_of(&finished)["forked"].is_null() {
        return (200, json_of(&finished)["forked"].clone());
    }
    let bundle = export(source, at, reader);
    if bundle.status != 200 {
        return (bundle.status, json_of(&bundle));
    }
    let imported = target.call("fork-import", &bundle.body);
    (imported.status, json_of(&imported))
}

fn export(source: &Object, at: u64, reader: Option<Role>) -> Response {
    let request = json!({ "at": at, "reader": reader.map(Role::name) });
    source.call("fork-export", request.to_string().as_bytes())
}

fn provision(object: &Object) {
    let (id, tree) = admin::genesis();
    let body = json!({ "genesis": { "id": id.to_string(), "tree": tree.to_string() } });
    assert_eq!(
        object.call("provision", body.to_string().as_bytes()).status,
        200
    );
}

fn publish(log: &mut FileWorldLog, name: &str, files: &[(&str, &[u8])], trace: Option<ObjectId>) {
    let layer = LayerId::parse(name).unwrap();
    let base = log.head().unwrap().number;
    log.create_layer(&layer, base).unwrap();
    let changes = SparseChanges::new(files.iter().map(|(path, bytes)| {
        let content = log.repository().put_blob(bytes).unwrap();
        (
            RepoPath::parse(path).unwrap(),
            Some(FileEntry::Regular {
                content,
                executable: false,
            }),
        )
    }))
    .unwrap();
    let op = |s: &str| OperationId::parse(&format!("{s}-{name}")).unwrap();
    let expected = log
        .append(&op("write"), &layer, &changes)
        .unwrap()
        .checkpoint()
        .object();
    let attribution = match trace {
        Some(context) => PublishAttribution::Agent {
            context: Some(context),
            allow_missing_trace: false,
        },
        None => PublishAttribution::Human,
    };
    let outcome = log
        .publish_layer(&op("publish"), &layer, expected, base, &attribution)
        .unwrap();
    assert!(
        matches!(outcome, PublishOutcome::Published { .. }),
        "{outcome:?}"
    );
}

/// The source: World 1 sets the policy, a maintainers' `.env` and a members' `internal/`;
/// World 2 changes `.env` and cites a public trace; World 3 cites a trace then restricted to
/// Maintain. Returns World 3's `(id, tree)`.
fn source(log: &mut FileWorldLog) -> (ObjectId, ObjectId) {
    publish(
        log,
        "one",
        &[
            (POLICY_PATH, POLICY),
            (".env", SECRETS[0]),
            ("internal/notes", SECRETS[2]),
            ("src/a", b"a\n"),
        ],
        None,
    );
    let public = log.repository().put_blob(PUBLIC_TRACE).unwrap();
    publish(
        log,
        "two",
        &[(".env", SECRETS[1]), ("src/a", b"b\n")],
        Some(public),
    );
    let secret = log.repository().put_blob(SECRET_TRACE).unwrap();
    publish(log, "three", &[("src/c", b"c\n")], Some(secret));
    let owner = PrincipalId::parse("owner").unwrap();
    let mut gate = Gate::new(RepositoryId::parse(SOURCE).unwrap(), test_keys::keys());
    gate.apply(AppliedPermissions {
        repository: RepositoryId::parse(SOURCE).unwrap(),
        epoch: 1,
        visibility: Visibility::Public,
        roles: [(owner, Role::Maintain)].into_iter().collect(),
        required_approvals: 0,
    })
    .unwrap();
    let at = now();
    let claims = test_keys::claims("owner", SOURCE, &[Scope::Publish], at, at + 600, "owner");
    let token = test_keys::issuer().issue(&claims).unwrap();
    log.traces()
        .restrict(&gate, log.repo(), Some(&token), at, secret)
        .unwrap();
    let head = log.head().unwrap();
    (head.id, head.tree)
}

/// What a fork left in `log`, in terms both stores share (their salts differ, so the
/// commitments do): the head and whether it is the source's, the openings held, which hidden
/// bytes are stored, what the head's view shows, and the traces.
fn held(log: &FileWorldLog, head: (ObjectId, ObjectId)) -> Vec<String> {
    let repo = log.repo();
    let top = repo.head().unwrap();
    let mut seen = vec![format!(
        "head {} is the source's {}",
        top.number,
        (top.id, top.tree) == head
    )];
    let commitments: Vec<_> = (1..=top.number.get())
        .flat_map(|n| {
            repo.committed_at(v(n))
                .unwrap()
                .entries()
                .values()
                .filter_map(|entry| match entry {
                    FileEntry::Hidden { commitment, .. } => Some(*commitment),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let opened = repo.openings(&commitments, &Reader::maintainer()).unwrap();
    seen.push(format!(
        "{} commitments, {} opened",
        commitments.len(),
        opened.len()
    ));
    for bytes in SECRETS {
        seen.push(format!(
            "{:?} stored {}",
            String::from_utf8_lossy(bytes),
            repo.has_blob(ObjectId::for_blob(bytes)).unwrap()
        ));
    }
    if top.number.get() > 0 {
        let view = repo.manifest_at(top.number).unwrap();
        let mut paths: Vec<_> = view
            .entries()
            .iter()
            .map(|(path, entry)| {
                format!(
                    "{}{}",
                    path.as_str(),
                    if entry.is_hidden() { " (hidden)" } else { "" }
                )
            })
            .collect();
        paths.sort();
        seen.push(format!("view {}", paths.join(", ")));
    }
    for bytes in [PUBLIC_TRACE, SECRET_TRACE] {
        let trace = ObjectId::for_blob(bytes);
        seen.push(format!(
            "trace {:?} stored {} restricted {}",
            String::from_utf8_lossy(bytes),
            repo.has_blob(trace).unwrap(),
            log.traces().visibility(trace).unwrap() == TraceVisibility::Restricted
        ));
    }
    let omitted: Vec<_> = fork::omitted_traces(log)
        .unwrap()
        .iter()
        .map(|trace| (*trace == ObjectId::for_blob(SECRET_TRACE)).to_string())
        .collect();
    seen.push(format!("omitted secret trace {omitted:?}"));
    seen.push(format!(
        "origin {:?}",
        fork::origin(log)
            .unwrap()
            .map(|origin| (origin.source, origin.number.get()))
    ));
    repo.check().unwrap();
    seen
}

/// The self-hosted fork for `reader`: its outcome and what the target holds.
fn native_fork(reader: Option<Role>, budget: Option<Budget>) -> (String, Vec<String>) {
    let (src_dir, dst_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut src = FileWorldLog::open(src_dir.path()).unwrap();
    let head = source(&mut src);
    let target = Directory::new(dst_dir.path());
    if let Some(budget) = budget {
        target.repository().unwrap();
        admin::set_budget(&target, &budget).unwrap();
    }
    let mut dst = FileWorldLog::on(&target, &target, Box::new(now)).unwrap();
    let outcome = match fork::fork_for(&src, SOURCE, &mut dst, v(3), reader) {
        Ok(forked) => format!("forked at {}", forked.origin.number),
        Err(refusal) => refusal.code,
    };
    (outcome, held(&dst, head))
}

/// The same fork between two objects.
fn object_fork_for(reader: Option<Role>, budget: Option<Budget>) -> (String, Vec<String>) {
    let (src, dst) = (Object::new(SOURCE), Object::new(TARGET));
    let head = source(&mut src.log());
    provision(&dst);
    if let Some(budget) = budget {
        let body = serde_json::to_vec(&budget).unwrap();
        assert_eq!(dst.call("budget", &body).status, 200);
    }
    let (status, body) = object_fork(&src, &dst, 3, reader);
    let outcome = match status {
        200 => format!("forked at {}", body["origin"]["number"]),
        _ => body["error"].as_str().unwrap().to_string(),
    };
    (outcome, held(&dst.log(), head))
}

#[test]
fn a_fork_between_two_objects_copies_what_a_self_hosted_fork_copies() {
    for (reader, opened, maintainer) in [
        (Some(Role::Maintain), 6, true),
        (Some(Role::Review), 3, false),
        (None, 0, false),
    ] {
        let native = native_fork(reader, None);
        let object = object_fork_for(reader, None);
        assert_eq!(native, object, "{reader:?}");
        let (outcome, held) = native;
        assert_eq!(outcome, "forked at 3");
        assert_eq!(held[0], "head 3 is the source's true");
        assert_eq!(
            held[1],
            format!("6 commitments, {opened} opened"),
            "{reader:?}"
        );
        // The maintainers' `.env` reaches only a maintainer's fork; the members' notes reach
        // every member's.
        assert_eq!(held[2], format!("\"TOKEN=1\\n\" stored {maintainer}"));
        assert_eq!(held[3], format!("\"TOKEN=2\\n\" stored {maintainer}"));
        assert_eq!(held[4], format!("\"notes\\n\" stored {}", reader.is_some()));
        assert_eq!(held[5].contains(".env (hidden)"), !maintainer, "{held:?}");
        assert_eq!(
            held[5].contains("internal/notes"),
            reader.is_some(),
            "{held:?}"
        );
        assert_eq!(
            held[7],
            format!("trace \"secret reasoning\" stored {maintainer} restricted {maintainer}")
        );
    }
}

#[test]
fn both_nodes_refuse_a_fork_past_the_targets_budget_before_writing() {
    for budget in [
        Budget {
            rows_written_per_day: 64,
            ..Budget::default()
        },
        Budget {
            largest_object: 4,
            ..Budget::default()
        },
    ] {
        let native = native_fork(Some(Role::Maintain), Some(budget));
        let object = object_fork_for(Some(Role::Maintain), Some(budget));
        assert_eq!(native, object);
        assert_eq!(native.0, "quota_exceeded");
        assert_eq!(native.1[0], "head 0 is the source's false");
        assert_eq!(native.1.last().unwrap(), "origin None");
    }

    // On the object, the refused import wrote no page, even before its rollback.
    let (src, dst) = (Object::new(SOURCE), Object::new(TARGET));
    source(&mut src.log());
    provision(&dst);
    let tight = Budget {
        storage_bytes: 1,
        ..Budget::default()
    };
    assert_eq!(
        dst.call("budget", &serde_json::to_vec(&tight).unwrap())
            .status,
        200
    );
    let bundle = export(&src, 3, Some(Role::Maintain)).body;
    let mut node = dst.node();
    dst.pages.puts.set(0);
    let refused = node.admin("fork-import", &bundle);
    assert_eq!(json_of(&refused)["error"], "quota_exceeded");
    assert!(
        json_of(&refused)["detail"]
            .as_str()
            .unwrap()
            .starts_with("storage_bytes: ")
    );
    assert_eq!(dst.pages.puts.get(), 0, "a refused fork writes nothing");

    // Raised, the same fork completes.
    assert_eq!(dst.call("budget", b"{}").status, 200);
    let (status, body) = object_fork(&src, &dst, 3, Some(Role::Maintain));
    assert_eq!((status, &body["origin"]["number"]), (200, &json!(3)));
}

#[test]
fn an_import_that_fails_part_way_leaves_the_target_untouched() {
    let (src, dst) = (Object::new(SOURCE), Object::new(TARGET));
    let head = source(&mut src.log());
    provision(&dst);
    let bundle = export(&src, 3, Some(Role::Maintain)).body;
    let untouched = held(&dst.log(), head);

    // A bundle whose last version does not match: the import writes its objects and the first
    // versions, then refuses, and the whole call is rolled back.
    let mut tampered = bundle.clone();
    let origin = 5 + 8 + SOURCE.len() + 8;
    // Each version is its ID, tree, path count and path bytes.
    let last = origin + 32 + 8 + 80 * 3;
    tampered[origin] ^= 1;
    tampered[last] ^= 1;
    let refused = dst.call("fork-import", &tampered);
    assert_eq!(refused.status, 409, "{:?}", json_of(&refused));
    assert!(refused.rollback);
    assert!(
        dst.pages.puts.get() > 0,
        "the import wrote before it refused"
    );
    assert_eq!(held(&dst.log(), head), untouched);

    // A storage call that fails anywhere in the import rolls the call back: the target is
    // untouched, or the fork is complete when the failure came after it.
    let base = dst.pages.files.borrow().clone();
    dst.pages.begin();
    dst.node().admin("fork-import", &bundle);
    let writes = dst.pages.puts.get();
    dst.pages.rollback();
    assert!(writes > 10, "{writes}");
    let complete = {
        let whole = Object::new(TARGET);
        *whole.pages.files.borrow_mut() = base.clone();
        assert_eq!(whole.call("fork-import", &bundle).status, 200);
        held(&whole.log(), head)
    };
    for fail_after in (0..writes).step_by((writes / 40).max(1) as usize) {
        let target = Object::new(TARGET);
        *target.pages.files.borrow_mut() = base.clone();
        target.pages.fail_after.set(Some(fail_after));
        target.call("fork-import", &bundle);
        target.pages.fail_after.set(None);
        let after = held(&target.log(), head);
        assert!(
            after == untouched || after == complete,
            "failing after {fail_after} of {writes} writes left {after:#?}"
        );
        // The retry finishes it.
        let (status, _) = object_fork(&src, &target, 3, Some(Role::Maintain));
        assert_eq!(status, 200);
        assert_eq!(held(&target.log(), head), complete);
    }
}

#[test]
fn a_finished_fork_is_answered_by_the_target_and_another_is_refused() {
    let (src, dst) = (Object::new(SOURCE), Object::new(TARGET));
    source(&mut src.log());
    provision(&dst);
    let (status, first) = object_fork(&src, &dst, 3, Some(Role::Read));
    assert_eq!(status, 200);
    assert_eq!(first["omitted"].as_array().unwrap().len(), 1);
    let again = dst.call(
        "fork-finished",
        json!({ "source": SOURCE, "at": 3 }).to_string().as_bytes(),
    );
    assert_eq!(json_of(&again)["forked"], first);
    let other = dst.call(
        "fork-finished",
        json!({ "source": SOURCE, "at": 2 }).to_string().as_bytes(),
    );
    assert_eq!(
        (other.status, json_of(&other)["error"].clone()),
        (409, json!("invalid_input"))
    );
    let state = json_of(&dst.call("state", b""));
    assert_eq!(state["fork_of"], first);
    assert_eq!(
        (state["head"].clone(), state["frontier"].clone()),
        (json!(3), json!(3))
    );

    // A source past its head, or an object forked into itself, is refused.
    let past = export(&src, 9, Some(Role::Read));
    assert_eq!(
        (past.status, json_of(&past)["error"].clone()),
        (409, json!("invalid_input"))
    );
    let bundle = export(&src, 3, Some(Role::Read)).body;
    assert_eq!(src.call("fork-import", &bundle).status, 400);
}

/// A fork at the tightest budget that admits it ends within it, for each budget: the bound is
/// never below what the fork writes. One source is wide (many paths per version), the other
/// long (many versions of one deep path).
#[test]
fn an_admitted_fork_ends_within_the_targets_budget() {
    let files = |version: usize, count: usize| -> Vec<(String, Vec<u8>)> {
        (0..count)
            .map(|i| {
                (
                    format!("src/d{}/e{}/f{i}.txt", i % 5, i % 3),
                    format!("version {version} file {i}\n")
                        .repeat(version + 1)
                        .into_bytes(),
                )
            })
            .collect()
    };
    admitted_within((0..12).map(|version| files(version, 40)).collect());
    admitted_within((0..60).map(|version| files(version, 1)).collect());
}

fn admitted_within(versions: Vec<Vec<(String, Vec<u8>)>>) {
    let src = Object::new(SOURCE);
    {
        let mut log = src.log();
        for (version, files) in versions.iter().enumerate() {
            let refs: Vec<(&str, &[u8])> = files
                .iter()
                .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
                .collect();
            publish(&mut log, &format!("v{version}"), &refs, None);
        }
    }
    let shape = format!("{}x{}", versions.len(), versions[0].len());
    let versions = versions.len();
    let bundle = export(&src, versions as u64, Some(Role::Maintain)).body;
    let fork_under = |budget: &Value| {
        let dst = Object::new(TARGET);
        provision(&dst);
        assert_eq!(
            dst.call("budget", budget.to_string().as_bytes()).status,
            200
        );
        let status = dst.call("fork-import", &bundle).status;
        (status, json_of(&dst.call("budget", b""))["usage"].clone())
    };
    let (status, usage) = fork_under(&json!({}));
    assert_eq!(status, 200);
    for meter in ["storage_bytes", "rows_written_per_day"] {
        // The fork's own use, on top of what provisioning left.
        let used = usage[meter]["used"].as_u64().unwrap();
        let (mut low, mut high) = (0, used * 64);
        while low + 1 < high {
            let limit = (low + high) / 2;
            match fork_under(&json!({ meter: limit })).0 {
                200 => high = limit,
                409 => low = limit,
                other => panic!("{other}"),
            }
        }
        let (status, after) = fork_under(&json!({ meter: high }));
        assert_eq!(status, 200);
        let ended = after[meter]["used"].as_u64().unwrap();
        assert!(
            ended <= high,
            "{meter}: admitted at {high}, ended at {ended}"
        );
        println!("{shape} {meter}: used {used}, tightest admitted limit {high}");
    }
}

/// A self-hosted fork interrupted after storing an object is charged, when it resumes, only
/// for what it still writes: at the tightest budget that admitted the whole fork, the resumed
/// one is admitted too.
#[test]
fn a_resumed_fork_is_charged_only_for_what_it_still_writes() {
    let src_dir = tempfile::tempdir().unwrap();
    let mut src = FileWorldLog::open(src_dir.path()).unwrap();
    let large = vec![7u8; 1 << 20];
    publish(&mut src, "large", &[("large.bin", &large)], None);
    let fork_under = |storage_bytes: u64, stored: bool| {
        let dir = tempfile::tempdir().unwrap();
        let target = Directory::new(dir.path());
        let mut repository = target.repository().unwrap();
        if stored {
            repository.put_blob(&large).unwrap();
        }
        drop(repository);
        let budget = Budget {
            storage_bytes,
            ..Budget::default()
        };
        admin::set_budget(&target, &budget).unwrap();
        let mut dst = FileWorldLog::on(&target, &target, Box::new(now)).unwrap();
        fork::fork_for(&src, SOURCE, &mut dst, v(1), Some(Role::Maintain)).map(|_| ())
    };
    let (mut low, mut high) = (0, 64 << 20);
    while low + 1 < high {
        let limit = (low + high) / 2;
        match fork_under(limit, false) {
            Ok(()) => high = limit,
            Err(refusal) if refusal.code == "quota_exceeded" => low = limit,
            Err(other) => panic!("{other:?}"),
        }
    }
    fork_under(high, true).expect("the resumed fork fits where the whole one did");
}

/// A fork into many audiences: 32 hidden directories hold the same files under 32 groups, so
/// each object gets an `object_access` row per group, far more rows than objects. On the
/// self-hosted node, whose small pages leave little reserve, a budget one byte or row short of
/// what the fork adds refuses it.
#[test]
fn a_fork_into_many_audiences_is_bounded_by_its_access_rows() {
    let src_dir = tempfile::tempdir().unwrap();
    let mut src = FileWorldLog::open(src_dir.path()).unwrap();
    let policy: String = (0..32).map(|g| format!("h{g}/** group:g{g}\n")).collect();
    let mut files = vec![(POLICY_PATH.to_string(), policy.into_bytes())];
    for g in 0..32 {
        files.extend((0..300).map(|i| {
            (
                format!("h{g}/d{}/f{i}", i % 7),
                format!("{i}\n").into_bytes(),
            )
        }));
    }
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect();
    publish(&mut src, "grouped", &refs, None);
    // The use before and after a fork under `budget`, or the refusal.
    let fork_under = |budget: Budget| {
        let dir = tempfile::tempdir().unwrap();
        let target = Directory::new(dir.path());
        target.repository().unwrap();
        admin::set_budget(&target, &budget).unwrap();
        let mut dst = FileWorldLog::on(&target, &target, Box::new(now)).unwrap();
        let used = |log: &FileWorldLog| {
            let usage = javelin_node::budget::usage(log, now()).unwrap();
            [0, 1].map(|meter| usage.meters[meter].used.unwrap())
        };
        let before = used(&dst);
        fork::fork_for(&src, SOURCE, &mut dst, v(1), Some(Role::Maintain))
            .map(|_| (before, used(&dst)))
    };
    let (before, after) = fork_under(Budget::default()).unwrap();
    let short = Budget {
        storage_bytes: after[0] - 1,
        ..Budget::default()
    };
    assert_eq!(fork_under(short).unwrap_err().code, "quota_exceeded");
    let short = Budget {
        rows_written_per_day: after[1] - before[1] - 1,
        ..Budget::default()
    };
    assert_eq!(fork_under(short).unwrap_err().code, "quota_exceeded");
}
