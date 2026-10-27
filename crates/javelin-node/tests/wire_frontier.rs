//! The public frontier on the node's wire, with real tokens and the node's clock moved by
//! the test, never by sleeping. A reader outside the repository (anonymous, or signed in with
//! no role) gets byte-for-byte the replies it would get from a node that never held the
//! versions above the frontier. The frontier moves on time and on release; members read the
//! head throughout; a fork at the frontier keeps the source's IDs.

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use javelin::{FileEntry, LayerId, ObjectId, OperationId, RepoPath, SparseChanges, VersionNumber};
use javelin_collaboration::{PrincipalId, Role, Visibility};
use javelin_identity::{Scope, test_keys};
use javelin_node::{
    FileWorldLog,
    auth::{self, Gate},
    bind_loopback, fork, serve,
};
use javelin_protocol::{
    Message,
    codec::{
        CheckReport, Conclusion, Disclosure, Embargoed, Kind, PublishOutcome, RequestId, decode,
        encode,
    },
    receiver::WorldLog,
};
use tempfile::TempDir;

#[path = "wire_support/mod.rs"]
mod wire_support;
use wire_support::{OWNER, permissions};

const REPO: &str = "repo-1";
/// Seven days, in seconds.
const WEEK: u64 = 7 * 86_400;
/// The node's clock when each test starts.
const T0: u64 = 1_900_000_000;

fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}
fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn layer(s: &str) -> LayerId {
    LayerId::parse(s).unwrap()
}

#[derive(Clone, Copy, Debug)]
enum Who {
    Anonymous,
    /// Signed in, with no role in the repository.
    Stranger,
    /// Read.
    Member,
    /// Maintain.
    Owner,
}

/// A node on a temporary directory, whose clock the test sets. Requests go over loopback
/// HTTP in sessions; between sessions the test holds the log.
struct Node {
    dir: TempDir,
    log: Option<FileWorldLog>,
    clock: Arc<AtomicU64>,
}

impl Node {
    /// A public repository where `owner` holds Maintain and `member` holds Read, with World 1
    /// publishing `src/a` at `T0`.
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let mut permissions = permissions(REPO, Visibility::Public);
        permissions
            .roles
            .insert(PrincipalId::parse("member").unwrap(), Role::Read);
        auth::write_permissions(dir.path(), &permissions).unwrap();
        auth::write_keys(dir.path(), &test_keys::keys()).unwrap();
        let mut node = Self {
            dir,
            log: None,
            clock: Arc::new(AtomicU64::new(T0)),
        };
        node.open();
        node.publish("seed", &[("src/a", b"a\n")], None);
        node
    }

    /// Opens the log on the node's directory with the test's clock, as a restart would.
    fn open(&mut self) {
        let mut log = FileWorldLog::open(self.dir.path()).unwrap();
        let clock = self.clock.clone();
        log.set_clock(move || clock.load(Ordering::SeqCst));
        self.log = Some(log);
    }

    fn at(&self, now: u64) {
        self.clock.store(now, Ordering::SeqCst);
    }

    fn now(&self) -> u64 {
        self.clock.load(Ordering::SeqCst)
    }

    fn log(&mut self) -> &mut FileWorldLog {
        self.log.as_mut().unwrap()
    }

    /// A token for `who`, valid now by the node's clock.
    fn token(&self, who: Who) -> Option<String> {
        let sub = match who {
            Who::Anonymous => return None,
            Who::Stranger => "stranger",
            Who::Member => "member",
            Who::Owner => OWNER,
        };
        let scopes = [
            Scope::Read,
            Scope::LayerWrite,
            Scope::Publish,
            Scope::ReviewWrite,
            Scope::ReviewApprove,
        ];
        let now = self.now();
        let claims = test_keys::claims(sub, REPO, &scopes, now, now + 600, &format!("{sub}-{now}"));
        Some(test_keys::issuer().issue(&claims).unwrap())
    }

    /// Serves exactly `requests`, one connection each, and returns the replies.
    fn session(&mut self, requests: &[(Who, Message)]) -> Vec<Message> {
        let gate = Gate::load(self.dir.path(), REPO).unwrap();
        let listener = bind_loopback("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut log = self.log.take().unwrap();
        let count = requests.len();
        let server = thread::spawn(move || {
            serve(&listener, REPO, &mut log, &gate, Some(count)).unwrap();
            log
        });
        let replies = requests
            .iter()
            .map(|(who, message)| send(addr, self.token(*who).as_deref(), message))
            .collect();
        self.log = Some(server.join().unwrap());
        replies
    }

    fn ask(&mut self, who: Who, message: Message) -> Message {
        self.session(&[(who, message)]).remove(0)
    }

    /// Creates Layer `name` on the head and appends `files` to it; returns its checkpoint.
    fn append(&mut self, name: &str, files: &[(&str, &[u8])]) -> ObjectId {
        let log = self.log();
        let base = log.head().unwrap().number;
        log.create_layer(&layer(name), base).unwrap();
        let changes = SparseChanges::new(files.iter().map(|(p, bytes)| {
            let entry = FileEntry::Regular {
                content: log.repository().put_blob(bytes).unwrap(),
                executable: false,
            };
            (RepoPath::parse(p).unwrap(), Some(entry))
        }))
        .unwrap();
        log.append(&op(&format!("write-{name}")), &layer(name), &changes)
            .unwrap()
            .checkpoint()
            .object()
    }

    /// Publishes `files` as Layer `name` over the wire as the owner, after embargoing the
    /// Publish when `embargo` is set (`Some(until)`). Returns the new version.
    fn publish(
        &mut self,
        name: &str,
        files: &[(&str, &[u8])],
        embargo: Option<Option<u64>>,
    ) -> VersionNumber {
        self.publish_as(name, files, embargo, javelin::PublishAttribution::Human)
    }

    fn publish_as(
        &mut self,
        name: &str,
        files: &[(&str, &[u8])],
        embargo: Option<Option<u64>>,
        attribution: javelin::PublishAttribution,
    ) -> VersionNumber {
        let checkpoint = self.append(name, files);
        let base = self.log().head().unwrap().number;
        let operation = op(&format!("publish-{name}"));
        let mut requests = Vec::new();
        if let Some(until) = embargo {
            requests.push((
                Who::Owner,
                Message::Embargo {
                    operation: operation.clone(),
                    until,
                },
            ));
        }
        requests.push((
            Who::Owner,
            Message::PublishLayer {
                operation,
                layer: layer(name),
                expected: checkpoint,
                base,
                attribution,
            },
        ));
        match self.session(&requests).pop().unwrap() {
            Message::PublishOutcome(PublishOutcome::Published { number, .. }) => number,
            other => panic!("expected a Publish, got {other:?}"),
        }
    }

    fn disclosure(&mut self, who: Who) -> Disclosure {
        match self.ask(who, Message::ReadDisclosure) {
            Message::Disclosure(disclosure) => disclosure,
            other => panic!("expected a disclosure, got {other:?}"),
        }
    }

    fn resolve(&mut self, who: Who) -> u64 {
        match self.ask(who, Message::Resolve { version: None }) {
            Message::Resolved { number, .. } => number.get(),
            other => panic!("expected a version, got {other:?}"),
        }
    }

    fn set_delay(&mut self, delay: u64) {
        let reply = self.ask(Who::Owner, Message::SetDelay { delay });
        assert!(matches!(reply, Message::Disclosure(_)), "{reply:?}");
    }
}

/// One message with an optional bearer token; returns the decoded reply.
fn send(addr: SocketAddr, token: Option<&str>, message: &Message) -> Message {
    let body = encode(message).unwrap();
    let mut stream = TcpStream::connect(addr).unwrap();
    let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    write!(
        stream,
        "POST /v2/r/{REPO} HTTP/1.1\r\n{auth}Content-Length: {}\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    decode(&reply[split + 4..]).unwrap()
}

/// What a probe may name: the held version's tree, its new blob and its checkpoint.
struct Probe {
    tree: ObjectId,
    blob: ObjectId,
    checkpoint: ObjectId,
}

/// Every read message an outsider can send, aimed at the version above the frontier and at
/// the versions around it.
fn battery(probe: &Probe) -> Vec<Message> {
    let mut out = vec![
        Message::Hello { min: 2, max: 2 },
        Message::Permissions,
        Message::ReadDisclosure,
        Message::Resolve { version: None },
        Message::Get(vec![(Kind::Tree, probe.tree)]),
        Message::Get(vec![(Kind::Blob, probe.blob)]),
        Message::Get(vec![(Kind::Checkpoint, probe.checkpoint)]),
        Message::Want(vec![(Kind::Blob, probe.blob), (Kind::Tree, probe.tree)]),
        Message::Want(vec![(Kind::Checkpoint, probe.checkpoint)]),
        Message::Open(vec![probe.blob, probe.tree]),
        Message::LayerHead {
            layer: layer("seed"),
        },
        Message::LayerHead {
            layer: layer("fix"),
        },
        Message::ReadRequest {
            request: RequestId::parse("request-fix").unwrap(),
        },
        // A public repository grants a signed-in caller the write scopes; without a role it
        // still reaches no Layer, request or check result.
        Message::LayerStack {
            operation: op("stack-probe"),
            target: layer("fix"),
            target_head: probe.checkpoint,
            source: probe.checkpoint,
        },
        Message::LayerAppend {
            operation: op("append-probe"),
            layer: layer("fix"),
            parent: Some(probe.checkpoint),
            checkpoint: probe.checkpoint,
        },
        Message::OpenRequest {
            operation: op("open-probe"),
            request: RequestId::parse("request-fix").unwrap(),
            layer: layer("fix"),
            checkpoint: probe.checkpoint,
            base: v(1),
        },
        Message::CheckReport(CheckReport {
            operation: op("check-probe"),
            repository: REPO.into(),
            subject_tree: probe.tree,
            subject_checkpoint: None,
            name: "ci/unit".into(),
            conclusion: Conclusion::Success,
            details_url: None,
            summary: None,
            started_at: T0,
            completed_at: T0,
        }),
        Message::SetDelay { delay: 0 },
        Message::Release { version: v(2) },
        Message::Embargo {
            operation: op("publish-fix"),
            until: None,
        },
    ];
    for n in 0..=3 {
        out.push(Message::Resolve {
            version: Some(v(n)),
        });
        out.push(Message::Changeset {
            version: v(n),
            offset: 0,
        });
        out.push(Message::Attribution { version: v(n) });
    }
    out
}

/// The replies an outsider gets to the battery, encoded, as anonymous and as a stranger.
fn outsider_view(node: &mut Node, probe: &Probe) -> Vec<Vec<u8>> {
    let requests: Vec<_> = [Who::Anonymous, Who::Stranger]
        .into_iter()
        .flat_map(|who| battery(probe).into_iter().map(move |m| (who, m)))
        .collect();
    node.session(&requests)
        .iter()
        .map(|reply| encode(reply).unwrap())
        .collect()
}

/// `held` holds World 2 above the frontier; `plain` never had it. An outsider's replies are
/// the same, byte for byte, and they say World 1 is the newest version.
fn assert_indistinguishable(held: &mut Node, plain: &mut Node) {
    let tree = held.log().repo().version(v(2)).unwrap().tree;
    let probe = Probe {
        tree,
        blob: ObjectId::for_blob(b"patched\n"),
        checkpoint: match held.log().layer_state(&layer("fix")).unwrap() {
            javelin::LayerState::Published { head, .. } => head.checkpoint().object(),
            other => panic!("fix is not published: {other:?}"),
        },
    };
    let seen_held = outsider_view(held, &probe);
    let seen_plain = outsider_view(plain, &probe);
    for (i, (a, b)) in seen_held.iter().zip(&seen_plain).enumerate() {
        assert_eq!(
            decode(a).unwrap(),
            decode(b).unwrap(),
            "reply {i} differs from a node without World 2"
        );
    }
    assert_eq!(seen_held, seen_plain);
    assert_eq!(held.resolve(Who::Anonymous), 1);
    // A member reads the head all along, and the held content with it.
    assert_eq!(held.resolve(Who::Member), 2);
    let blob = held.ask(Who::Member, Message::Get(vec![(Kind::Blob, probe.blob)]));
    assert!(
        matches!(blob, Message::Objects(ref o) if o.len() == 1),
        "{blob:?}"
    );
}

#[test]
fn an_embargoed_version_is_invisible_outside_the_repository() {
    let mut held = Node::new();
    let mut plain = Node::new();
    held.at(T0 + 10);
    plain.at(T0 + 10);
    let fix = held.publish(
        "fix",
        &[("src/a", b"patched\n"), ("src/fix", b"patched\n")],
        Some(None),
    );
    assert_eq!(fix, v(2));
    assert_indistinguishable(&mut held, &mut plain);

    // The member's view names the embargo; an outsider's does not.
    let member = held.disclosure(Who::Member);
    assert_eq!(member.frontier, v(1));
    assert_eq!(member.delay, Some(0));
    assert_eq!(
        member.embargoes,
        vec![Embargoed {
            version: v(2),
            until: None
        }]
    );
    let outsider = held.disclosure(Who::Anonymous);
    assert_eq!(
        outsider,
        Disclosure {
            delayed: false,
            delay: None,
            frontier: v(1),
            embargoes: Vec::new()
        }
    );
}

#[test]
fn a_restricted_trace_of_a_held_version_is_absent_outside_the_repository() {
    let mut node = Node::new();
    node.at(T0 + 10);
    let trace = node
        .log()
        .repository()
        .put_blob(b"private reasoning")
        .unwrap();
    node.publish_as(
        "fix",
        &[("src/a", b"patched\n")],
        Some(None),
        javelin::PublishAttribution::Agent {
            context: Some(trace),
            allow_missing_trace: false,
        },
    );
    let gate = Gate::load(node.dir.path(), REPO).unwrap();
    let owner = node.token(Who::Owner);
    let now = node.now();
    let log = node.log();
    log.traces()
        .restrict(&gate, log.repo(), owner.as_deref(), now, trace)
        .unwrap();
    let never = ObjectId::for_blob(b"never stored");
    let replies = node.session(&[
        (Who::Anonymous, Message::Get(vec![(Kind::Blob, never)])),
        (Who::Anonymous, Message::Get(vec![(Kind::Blob, trace)])),
        (Who::Member, Message::Get(vec![(Kind::Blob, trace)])),
    ]);
    let detail = |reply: &Message| match reply {
        Message::Error { code, detail } => (code.clone(), detail.clone()),
        other => panic!("expected a refusal, got {other:?}"),
    };
    let (code, absent) = detail(&replies[0]);
    assert_eq!(
        detail(&replies[1]),
        (code, absent.replace(&never.to_string(), &trace.to_string()))
    );
    // A member may read the version, so it is told the trace is restricted.
    assert_eq!(detail(&replies[2]).0, "forbidden");
}

#[test]
fn a_delayed_version_is_invisible_outside_the_repository() {
    let mut held = Node::new();
    let mut plain = Node::new();
    for node in [&mut held, &mut plain] {
        node.set_delay(WEEK);
        node.at(T0 + WEEK + 10);
    }
    held.publish(
        "fix",
        &[("src/a", b"patched\n"), ("src/fix", b"patched\n")],
        None,
    );
    held.at(T0 + WEEK + 20);
    plain.at(T0 + WEEK + 20);
    assert_indistinguishable(&mut held, &mut plain);
    let outsider = held.disclosure(Who::Anonymous);
    assert!(outsider.delayed);
    assert_eq!(outsider.delay, None, "an outsider is not told the delay");
    assert_eq!(held.disclosure(Who::Member).delay, Some(WEEK));
}

#[test]
fn the_frontier_moves_when_the_delay_passes_and_ids_never_change() {
    let mut node = Node::new();
    node.set_delay(WEEK);
    // World 1 was published at T0, so it is held until T0 + WEEK.
    node.at(T0 + WEEK - 1);
    assert_eq!(node.resolve(Who::Anonymous), 0);
    node.at(T0 + WEEK);
    assert_eq!(node.resolve(Who::Anonymous), 1);

    node.at(T0 + WEEK + 100);
    node.publish("two", &[("src/b", b"b\n")], None);
    node.at(T0 + WEEK + 200);
    node.publish("three", &[("src/c", b"c\n")], None);
    let ids: Vec<_> = (0..=3)
        .map(|n| node.log().repo().version(v(n)).unwrap().id)
        .collect();

    // Each version becomes public on its own, a week after it was published.
    for (now, frontier) in [
        (T0 + 2 * WEEK + 99, 1),
        (T0 + 2 * WEEK + 100, 2),
        (T0 + 2 * WEEK + 199, 2),
        (T0 + 2 * WEEK + 200, 3),
    ] {
        node.at(now);
        let reply = node.ask(Who::Anonymous, Message::Resolve { version: None });
        let Message::Resolved { number, id, .. } = reply else {
            panic!("{reply:?}");
        };
        assert_eq!(number.get(), frontier, "frontier at {now}");
        assert_eq!(id, ids[frontier as usize], "disclosure never changes an ID");
        assert_eq!(node.resolve(Who::Member), 3, "a member reads the head");
    }

    // Disclosure is one-way: a longer delay takes nothing back.
    node.set_delay(10 * WEEK);
    assert_eq!(node.resolve(Who::Anonymous), 3);
    assert_eq!(node.disclosure(Who::Member).frontier, v(3));
}

#[test]
fn turning_the_delay_off_shows_the_head_at_once() {
    let mut node = Node::new();
    node.set_delay(WEEK);
    node.at(T0 + 10);
    node.publish("two", &[("src/b", b"b\n")], None);
    assert_eq!(node.resolve(Who::Anonymous), 0);
    node.set_delay(0);
    assert_eq!(node.resolve(Who::Anonymous), 2);
}

#[test]
fn a_release_discloses_every_waiting_version_at_once() {
    let mut node = Node::new();
    node.at(T0 + 10);
    node.publish("fix", &[("src/a", b"patched\n")], Some(None));
    node.publish("two", &[("src/b", b"b\n")], None);
    node.publish("three", &[("src/c", b"c\n")], None);
    assert_eq!(node.resolve(Who::Anonymous), 1);
    assert_eq!(node.resolve(Who::Member), 4);

    // Only Maintain releases, and a refusal changes nothing.
    for who in [Who::Anonymous, Who::Stranger, Who::Member] {
        let reply = node.ask(who, Message::Release { version: v(2) });
        assert!(matches!(reply, Message::Error { .. }), "{who:?}: {reply:?}");
    }
    assert_eq!(node.resolve(Who::Anonymous), 1);
    let released = node.ask(Who::Owner, Message::Release { version: v(2) });
    let Message::Disclosure(released) = released else {
        panic!("{released:?}");
    };
    assert_eq!(released.frontier, v(4));
    assert!(released.embargoes.is_empty());
    assert_eq!(node.resolve(Who::Anonymous), 4);
    // Releasing again changes nothing; a version without an embargo is refused.
    assert!(matches!(
        node.ask(Who::Owner, Message::Release { version: v(2) }),
        Message::Disclosure(_)
    ));
    assert!(matches!(
        node.ask(Who::Owner, Message::Release { version: v(3) }),
        Message::Error { .. }
    ));
}

#[test]
fn an_embargo_with_a_time_releases_itself_and_waits_for_the_delay() {
    let mut node = Node::new();
    node.set_delay(3600);
    node.at(T0 + 10);
    let until = T0 + 2 * WEEK;
    node.publish("fix", &[("src/a", b"patched\n")], Some(Some(until)));
    // The delay alone would disclose World 2 an hour after it was published.
    node.at(T0 + 10 + 3600);
    assert_eq!(node.resolve(Who::Anonymous), 1);
    node.at(until - 1);
    assert_eq!(node.resolve(Who::Anonymous), 1);
    node.at(until);
    assert_eq!(node.resolve(Who::Anonymous), 2);

    // Released by hand before the delay passes, a version still waits for the delay.
    node.at(until + 10);
    node.publish("late", &[("src/b", b"b\n")], Some(None));
    node.ask(Who::Owner, Message::Release { version: v(3) });
    assert_eq!(node.resolve(Who::Anonymous), 2);
    node.at(until + 10 + 3600);
    assert_eq!(node.resolve(Who::Anonymous), 3);
}

#[test]
fn only_maintain_changes_the_disclosure_and_an_embargo_comes_before_its_publish() {
    let mut node = Node::new();
    for who in [Who::Anonymous, Who::Stranger, Who::Member] {
        for message in [
            Message::SetDelay { delay: WEEK },
            Message::Embargo {
                operation: op("publish-next"),
                until: None,
            },
        ] {
            let reply = node.ask(who, message);
            assert!(matches!(reply, Message::Error { .. }), "{who:?}: {reply:?}");
        }
    }
    assert_eq!(node.disclosure(Who::Member).delay, Some(0));

    // World 1 is already published: embargoing it now could only hide what was public.
    let reply = node.ask(
        Who::Owner,
        Message::Embargo {
            operation: op("publish-seed"),
            until: None,
        },
    );
    assert!(
        matches!(reply, Message::Error { ref code, .. } if code == "invalid_input"),
        "{reply:?}"
    );

    // Repeating an embargo is safe, before and after its Publish; changing it after is not.
    let embargo = Message::Embargo {
        operation: op("publish-fix"),
        until: Some(T0 + WEEK),
    };
    assert!(matches!(
        node.ask(Who::Owner, embargo.clone()),
        Message::Disclosure(_)
    ));
    node.publish("fix", &[("src/a", b"patched\n")], Some(Some(T0 + WEEK)));
    assert!(matches!(
        node.ask(Who::Owner, embargo),
        Message::Disclosure(_)
    ));
    let changed = node.ask(
        Who::Owner,
        Message::Embargo {
            operation: op("publish-fix"),
            until: None,
        },
    );
    assert!(matches!(changed, Message::Error { .. }), "{changed:?}");
    assert_eq!(node.resolve(Who::Anonymous), 1);
}

#[test]
fn the_frontier_survives_a_restart() {
    let mut node = Node::new();
    node.set_delay(WEEK);
    node.at(T0 + WEEK + 10);
    node.publish("fix", &[("src/a", b"patched\n")], Some(None));
    drop(node.log.take());
    node.open();
    node.at(T0 + 10 * WEEK);
    assert_eq!(node.resolve(Who::Anonymous), 1);
    node.ask(Who::Owner, Message::Release { version: v(2) });
    drop(node.log.take());
    node.open();
    assert_eq!(node.resolve(Who::Anonymous), 2);
}

#[test]
fn a_browser_reads_the_disclosure() {
    let mut node = Node::new();
    node.set_delay(WEEK);
    let gate = Gate::load(node.dir.path(), REPO).unwrap();
    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let mut log = node.log.take().unwrap();
    let server = thread::spawn(move || {
        serve(&listener, REPO, &mut log, &gate, Some(1)).unwrap();
        log
    });
    let body = encode(&Message::ReadDisclosure).unwrap();
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "POST /v2/r/{REPO} HTTP/1.1\r\nOrigin: https://javelin.codes\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    node.log = Some(server.join().unwrap());
    let text = String::from_utf8_lossy(&reply);
    assert!(text.contains("Access-Control-Allow-Origin: *"), "{text}");
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let Message::Disclosure(disclosure) = decode(&reply[split + 4..]).unwrap() else {
        panic!("{text}");
    };
    assert!(disclosure.delayed);
    assert_eq!(disclosure.delay, None);
}

#[test]
fn an_outsider_forks_at_the_frontier_with_the_sources_ids() {
    let mut node = Node::new();
    node.at(T0 + 10);
    node.publish("fix", &[("src/a", b"patched\n")], Some(None));
    let source = node.log.take().unwrap();
    let target_dir = TempDir::new().unwrap();
    let mut target = FileWorldLog::open(target_dir.path()).unwrap();

    // Past the frontier and past the head are refused alike.
    let above = fork::fork_for(&source, REPO, &mut target, v(2), None).unwrap_err();
    let past = fork::fork_for(&source, REPO, &mut target, v(9), None).unwrap_err();
    assert_eq!(above.code, past.code);
    assert_eq!(above.detail.replace('2', "9"), past.detail);
    assert_eq!(target.head().unwrap().number, v(0));

    let forked = fork::fork_for(&source, REPO, &mut target, v(1), None).unwrap();
    assert_eq!(forked.origin.number, v(1));
    for n in 0..=1 {
        let (copied, original) = (
            target.repo().version(v(n)).unwrap(),
            source.repo().version(v(n)).unwrap(),
        );
        assert_eq!((copied.id, copied.tree), (original.id, original.tree));
    }
    assert_eq!(target.head().unwrap().number, v(1));
    // A member forks at the head.
    let member_dir = TempDir::new().unwrap();
    let mut member = FileWorldLog::open(member_dir.path()).unwrap();
    fork::fork_for(&source, REPO, &mut member, v(2), Some(Role::Read)).unwrap();
    assert_eq!(
        member.head().unwrap().id,
        source.repo().version(v(2)).unwrap().id
    );
}
