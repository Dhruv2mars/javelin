//! Server-side Stack: `LayerStack` on a node over loopback HTTP, and a local Layer
//! refreshed from the head it produces. The node and a local store are seeded with the
//! same World and Layers, so the same Stack can run on both.

use std::{
    io::{self, Cursor, Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

use javelin::{
    AdvanceOutcome, FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, Manifest, ObjectId,
    OperationId, RepoPath, Repository, SparseChanges, VersionNumber,
};
use javelin_merge::Resource;
use javelin_node::{FileWorldLog, MERGE_LIMIT, bind_loopback, serve};
use javelin_protocol::codec::LayerOutcome;
use javelin_stack::{
    StackOutcome,
    remote::{self, LAYER_DIVERGED, Refresh},
    stack_layer,
};
use tempfile::TempDir;

const NAME: &str = "demo";
const BASE: &[u8] = b"one\ntwo\nthree\nfour\nfive\n";
const OURS: &[u8] = b"ONE\ntwo\nthree\nfour\nfive\n";
const THEIRS: &[u8] = b"one\ntwo\nthree\nfour\nFIVE\n";
const MERGED: &[u8] = b"ONE\ntwo\nthree\nfour\nFIVE\n";

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn layer(s: &str) -> LayerId {
    LayerId::parse(s).unwrap()
}
fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}
fn path(s: &str) -> RepoPath {
    RepoPath::parse(s).unwrap()
}

fn file(repo: &mut Repository, bytes: &[u8]) -> Option<FileEntry> {
    Some(FileEntry::Regular {
        content: repo.put_blob(bytes).unwrap(),
        executable: false,
    })
}

fn changes(entries: Vec<(&str, Option<FileEntry>)>) -> SparseChanges {
    SparseChanges::new(entries.into_iter().map(|(p, e)| (path(p), e))).unwrap()
}

fn capture(
    repo: &mut Repository,
    operation: &str,
    head: &LayerHead,
    changes: &SparseChanges,
) -> LayerHead {
    match repo.capture_layer(&op(operation), head, changes).unwrap() {
        LayerUpdate::Applied {
            state: LayerState::Active { head },
        } => head,
        other => panic!("{other:?}"),
    }
}

fn active(repo: &Repository, name: &str) -> LayerHead {
    match repo.layer_state(&layer(name)).unwrap() {
        LayerState::Active { head } => head,
        other => panic!("{other:?}"),
    }
}

/// World 1 holds `notes.txt`. Layer `target` edits its first line and Layer `source`
/// its last line and adds `new.txt`, both over World 1. Running this on two stores gives
/// both the same contents; their IDs differ, because each store has its own identity.
fn seed(dir: &Path, target: &[u8], source: &[u8]) -> (LayerHead, LayerHead) {
    let mut repo = Repository::init(dir).unwrap();
    let base = file(&mut repo, BASE).unwrap();
    let tree = repo
        .put_snapshot(&Manifest::new([(path("notes.txt"), base)]).unwrap())
        .unwrap();
    let outcome = repo.advance_to_tree(&op("seed-1"), v(0), tree).unwrap();
    assert!(matches!(outcome, AdvanceOutcome::Accepted { .. }));
    let base = repo.layer_base(v(1)).unwrap();
    let t = repo.create_layer(&layer("target"), &base).unwrap();
    let s = repo.create_layer(&layer("source"), &base).unwrap();
    let ours = changes(vec![("notes.txt", file(&mut repo, target))]);
    let theirs = changes(vec![
        ("notes.txt", file(&mut repo, source)),
        ("new.txt", file(&mut repo, b"new\n")),
    ]);
    (
        capture(&mut repo, "target-1", &t, &ours),
        capture(&mut repo, "source-1", &s, &theirs),
    )
}

/// The node on loopback. It serves one connection at a time until `finish`.
struct Served {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<FileWorldLog>,
}

fn serve_node(dir: &Path) -> Served {
    let log = FileWorldLog::open(dir).unwrap();
    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = thread::spawn(move || {
        let mut log = log;
        while !flag.load(Ordering::SeqCst) {
            serve(&listener, NAME, &mut log, Some(1)).unwrap();
        }
        log
    });
    Served { addr, stop, thread }
}

impl Served {
    /// Stops the node and returns its log. One last connection wakes the accept loop.
    fn finish(self) -> FileWorldLog {
        self.stop.store(true, Ordering::SeqCst);
        let _ = exchange(self.addr, "GET /v2/r/demo HTTP/1.1\r\n\r\n", &[]);
        self.thread.join().unwrap()
    }
}

/// One request on its own connection. Returns the body.
fn exchange(addr: SocketAddr, request: impl AsRef<[u8]>, body: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(request.as_ref()).unwrap();
    stream.write_all(body).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    reply[split + 4..].to_vec()
}

/// A protocol session over HTTP: each frame is sent as one POST when its reply is read.
struct Link {
    addr: SocketAddr,
    outbound: Vec<u8>,
    inbox: Cursor<Vec<u8>>,
}

impl Link {
    fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            outbound: Vec::new(),
            inbox: Cursor::new(Vec::new()),
        }
    }
}

impl Write for Link {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.outbound.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for Link {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.inbox.position() as usize >= self.inbox.get_ref().len() {
            if self.outbound.is_empty() {
                return Ok(0);
            }
            let frame = std::mem::take(&mut self.outbound);
            let body = &frame[4..];
            let reply = exchange(
                self.addr,
                format!(
                    "POST /v2/r/{NAME} HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                ),
                body,
            );
            let mut framed = (reply.len() as u32).to_be_bytes().to_vec();
            framed.extend(reply);
            self.inbox = Cursor::new(framed);
        }
        self.inbox.read(buf)
    }
}

fn node_stack(
    link: &mut Link,
    operation: &str,
    target: ObjectId,
    source: ObjectId,
) -> LayerOutcome {
    remote::stack(link, &op(operation), &layer("target"), target, source).unwrap()
}

#[test]
fn golden_node_and_local_stacks_produce_the_same_changes() {
    let local_dir = TempDir::new().unwrap();
    let (target, source) = seed(local_dir.path(), OURS, THEIRS);
    let mut local = Repository::open(local_dir.path()).unwrap();
    let StackOutcome::Updated(LayerUpdate::Applied {
        state: LayerState::Active { head: stacked },
    }) = stack_layer(&mut local, &op("stack-1"), &target, source.checkpoint()).unwrap()
    else {
        panic!("local Stack did not apply");
    };

    let node_dir = TempDir::new().unwrap();
    let (node_target, node_source) = seed(node_dir.path(), OURS, THEIRS);
    let served = serve_node(node_dir.path());
    let mut link = Link::new(served.addr);
    let outcome = node_stack(
        &mut link,
        "stack-1",
        node_target.checkpoint().object(),
        node_source.checkpoint().object(),
    );
    let node = served.finish();
    let node = node.repo();
    let LayerOutcome::Accepted { seq, checkpoint } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(seq, stacked.revision());
    assert_eq!(seq, 2);
    let node_head = active(node, "target");
    assert_eq!(node_head.checkpoint().object(), checkpoint);

    let ours = local.read_checkpoint(stacked.checkpoint()).unwrap();
    let theirs = node.read_checkpoint(node_head.checkpoint()).unwrap();
    let bytes = ours.changes().canonical_bytes();
    assert_eq!(bytes, theirs.changes().canonical_bytes());
    // Pinned, so a change to the engine or the encoding shows up here as well.
    assert_eq!(
        ObjectId::for_blob(&bytes).to_string(),
        "e5084b5dde1bdfab0c7956ec159e2381af1e85f1b93426d851eb763dcbc336a1"
    );
    assert_eq!(
        local.checkpoint_manifest(stacked.checkpoint()).unwrap(),
        node.checkpoint_manifest(node_head.checkpoint()).unwrap()
    );
    let merged = local
        .checkpoint_manifest(stacked.checkpoint())
        .unwrap()
        .entries()[&path("notes.txt")]
        .content();
    assert_eq!(local.read_blob(merged).unwrap(), MERGED);
    assert_eq!(theirs.stack_sources(), [node_source.checkpoint()]);
    node.check().unwrap();
}

#[test]
fn a_conflict_is_refused_and_writes_nothing() {
    let node_dir = TempDir::new().unwrap();
    let (target, source) = seed(node_dir.path(), OURS, b"One\ntwo\nthree\nfour\nfive\n");
    let before = Repository::open(node_dir.path())
        .unwrap()
        .check()
        .unwrap()
        .objects;
    let served = serve_node(node_dir.path());
    let mut link = Link::new(served.addr);
    let outcome = node_stack(
        &mut link,
        "stack-1",
        target.checkpoint().object(),
        source.checkpoint().object(),
    );
    assert_eq!(
        outcome,
        LayerOutcome::Refused {
            code: "conflict".into()
        }
    );
    let log = served.finish();
    let node = log.repo();
    assert_eq!(active(node, "target"), target);
    assert_eq!(node.check().unwrap().objects, before);
    assert_eq!(
        node.stack_operation(&op("stack-1"), &target, source.checkpoint())
            .unwrap(),
        None
    );
}

#[test]
fn a_merge_over_a_limit_is_refused_and_writes_nothing() {
    let large = b"line\n".repeat(Resource::Lines.limit() as usize + 1);
    let node_dir = TempDir::new().unwrap();
    let (target, source) = seed(node_dir.path(), &large, THEIRS);
    let before = Repository::open(node_dir.path())
        .unwrap()
        .check()
        .unwrap()
        .objects;
    let served = serve_node(node_dir.path());
    let mut link = Link::new(served.addr);
    let outcome = node_stack(
        &mut link,
        "stack-1",
        target.checkpoint().object(),
        source.checkpoint().object(),
    );
    assert_eq!(
        outcome,
        LayerOutcome::Refused {
            code: MERGE_LIMIT.into()
        }
    );
    let log = served.finish();
    let node = log.repo();
    assert_eq!(active(node, "target"), target);
    assert_eq!(node.check().unwrap().objects, before);
}

#[test]
fn a_stale_target_head_is_refused_and_a_retry_after_refresh_succeeds() {
    let node_dir = TempDir::new().unwrap();
    let (target, source) = seed(node_dir.path(), OURS, THEIRS);
    // The target moves on after the client last read it.
    let moved = {
        let mut repo = Repository::open(node_dir.path()).unwrap();
        let extra = changes(vec![
            ("notes.txt", file(&mut repo, OURS)),
            ("extra.txt", file(&mut repo, b"extra\n")),
        ]);
        capture(&mut repo, "target-2", &target, &extra)
    };
    let served = serve_node(node_dir.path());
    let mut link = Link::new(served.addr);
    let stale = node_stack(
        &mut link,
        "stack-1",
        target.checkpoint().object(),
        source.checkpoint().object(),
    );
    assert_eq!(
        stale,
        LayerOutcome::Stale {
            head: moved.checkpoint().object()
        }
    );
    // The client reads the head and retries with the same operation ID.
    let head = javelin_protocol::sender::layer_head(&mut link, &layer("target")).unwrap();
    assert_eq!(head.head, moved.checkpoint().object());
    let accepted = node_stack(
        &mut link,
        "stack-1",
        head.head,
        source.checkpoint().object(),
    );
    let LayerOutcome::Accepted { seq, checkpoint } = accepted else {
        panic!("{accepted:?}");
    };
    assert_eq!(seq, 3);
    // A lost reply: the retry gets the same outcome, even after the head moves again.
    assert_eq!(
        node_stack(
            &mut link,
            "stack-1",
            head.head,
            source.checkpoint().object()
        ),
        accepted
    );
    drop(served.finish());
    let mut repo = Repository::open(node_dir.path()).unwrap();
    let now = active(&repo, "target");
    assert_eq!(now.checkpoint().object(), checkpoint);
    let extra = changes(vec![("later.txt", file(&mut repo, b"later\n"))]);
    let later = repo.read_checkpoint(now.checkpoint()).unwrap();
    let mut all: Vec<_> = later.changes().entries().clone().into_iter().collect();
    all.extend(extra.entries().clone());
    capture(
        &mut repo,
        "target-4",
        &now,
        &SparseChanges::new(all).unwrap(),
    );
    drop(repo);
    let served = serve_node(node_dir.path());
    let mut link = Link::new(served.addr);
    assert_eq!(
        node_stack(
            &mut link,
            "stack-1",
            head.head,
            source.checkpoint().object()
        ),
        accepted
    );
    // The same ID for a different Stack is refused.
    assert_eq!(
        node_stack(
            &mut link,
            "stack-1",
            head.head,
            target.checkpoint().object()
        ),
        LayerOutcome::Refused {
            code: "operation_id_reused".into()
        }
    );
    served.finish().repo().check().unwrap();
}

#[test]
fn refresh_imports_the_nodes_stacked_head() {
    let local_dir = TempDir::new().unwrap();
    seed(local_dir.path(), OURS, THEIRS);
    let node_dir = TempDir::new().unwrap();
    let (node_target, node_source) = seed(node_dir.path(), OURS, THEIRS);
    let served = serve_node(node_dir.path());
    let mut link = Link::new(served.addr);
    let mut local = Repository::open(local_dir.path()).unwrap();

    // In step with the node: nothing to do.
    assert_eq!(
        remote::refresh(&mut link, &mut local, &layer("target")).unwrap(),
        Refresh::Current {
            seq: 1,
            head: node_target.checkpoint().object()
        }
    );

    let LayerOutcome::Accepted { seq, checkpoint } = node_stack(
        &mut link,
        "stack-1",
        node_target.checkpoint().object(),
        node_source.checkpoint().object(),
    ) else {
        panic!("node Stack did not apply");
    };
    let refreshed = remote::refresh(&mut link, &mut local, &layer("target")).unwrap();
    let Refresh::Imported {
        seq: imported_seq,
        head,
        ..
    } = refreshed
    else {
        panic!("{refreshed:?}");
    };
    assert_eq!((imported_seq, head), (seq, checkpoint));
    let now = active(&local, "target");
    assert_eq!(now.revision(), 2);
    let manifest = local.checkpoint_manifest(now.checkpoint()).unwrap();
    let merged = manifest.entries()[&path("notes.txt")].content();
    assert_eq!(local.read_blob(merged).unwrap(), MERGED);
    let new = manifest.entries()[&path("new.txt")].content();
    assert_eq!(local.read_blob(new).unwrap(), b"new\n");
    local.check().unwrap();

    // Again: already current, and nothing moves.
    assert_eq!(
        remote::refresh(&mut link, &mut local, &layer("target")).unwrap(),
        Refresh::Current { seq, head }
    );
    assert_eq!(active(&local, "target"), now);

    // A local capture the node lacks is never replaced by the node's head.
    let mine = {
        let ours = local.read_checkpoint(now.checkpoint()).unwrap();
        let mut all: Vec<_> = ours.changes().entries().clone().into_iter().collect();
        all.push((path("mine.txt"), file(&mut local, b"mine\n")));
        capture(
            &mut local,
            "target-3",
            &now,
            &SparseChanges::new(all).unwrap(),
        )
    };
    let error = remote::refresh(&mut link, &mut local, &layer("target")).unwrap_err();
    assert_eq!(error.code, LAYER_DIVERGED);
    assert_eq!(active(&local, "target"), mine);
    served.finish().repo().check().unwrap();
}

#[test]
fn layers_at_the_same_step_refresh_to_the_same_head() {
    // `twin` holds the same capture as `target` on both stores, so a node Stack of the
    // same source gives both the same head, at the same local revision.
    let twin = |dir: &Path| {
        let mut repo = Repository::open(dir).unwrap();
        let base = repo.layer_base(v(1)).unwrap();
        let head = repo.create_layer(&layer("twin"), &base).unwrap();
        let ours = changes(vec![("notes.txt", file(&mut repo, OURS))]);
        capture(&mut repo, "twin-1", &head, &ours)
    };
    let local_dir = TempDir::new().unwrap();
    seed(local_dir.path(), OURS, THEIRS);
    twin(local_dir.path());
    let node_dir = TempDir::new().unwrap();
    let (node_target, node_source) = seed(node_dir.path(), OURS, THEIRS);
    let node_twin = twin(node_dir.path());
    assert_eq!(node_twin.checkpoint(), node_target.checkpoint());
    let served = serve_node(node_dir.path());
    let mut link = Link::new(served.addr);
    let mut heads = Vec::new();
    for name in ["target", "twin"] {
        let outcome = remote::stack(
            &mut link,
            &op(&format!("stack-{name}")),
            &layer(name),
            node_target.checkpoint().object(),
            node_source.checkpoint().object(),
        )
        .unwrap();
        let LayerOutcome::Accepted { checkpoint, .. } = outcome else {
            panic!("{outcome:?}");
        };
        heads.push(checkpoint);
    }
    assert_eq!(heads[0], heads[1]);

    let mut local = Repository::open(local_dir.path()).unwrap();
    for name in ["target", "twin"] {
        let refreshed = remote::refresh(&mut link, &mut local, &layer(name)).unwrap();
        assert!(
            matches!(refreshed, Refresh::Imported { .. }),
            "{refreshed:?}"
        );
        assert_eq!(active(&local, name).revision(), 2);
    }
    local.check().unwrap();
    served.finish().repo().check().unwrap();
}
