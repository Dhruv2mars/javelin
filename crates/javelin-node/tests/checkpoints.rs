//! Streaming checkpoints: captures upload to a node, which appends them to the Layer
//! with `LayerAppend`. Each test runs the node on loopback HTTP, one message per POST.

use std::{
    fs,
    io::{self, Cursor, Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

use javelin::{
    CheckpointSink, FileEntry, LayerHead, LayerId, LayerState, ObjectId, OperationId, RepoPath,
    Repository, SparseChanges, VersionNumber,
};
use javelin_node::{FileWorldLog, bind_loopback, serve};
use javelin_protocol::{
    codec::LayerOutcome,
    sender::{self, Uploader},
};
use tempfile::TempDir;

const NAME: &str = "demo";

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn layer(s: &str) -> LayerId {
    LayerId::parse(s).unwrap()
}
fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}

/// A node with Layer `work` over World 0. Returns the log and the Layer's first head.
fn node(dir: &TempDir) -> (FileWorldLog, ObjectId) {
    let mut log = FileWorldLog::open(dir.path()).unwrap();
    let head = log.create_layer(&layer("work"), v(0)).unwrap();
    (log, head.checkpoint().object())
}

/// The node on loopback. It serves one connection at a time until `finish`.
struct Served {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<FileWorldLog>,
}

fn serve_node(log: FileWorldLog) -> Served {
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

/// One request on its own connection. Returns the status and the body.
fn exchange(addr: SocketAddr, request: impl AsRef<[u8]>, body: &[u8]) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(request.as_ref()).unwrap();
    stream.write_all(body).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = std::str::from_utf8(&reply[..split]).unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, reply[split + 4..].to_vec())
}

/// A protocol session over HTTP. A message is written as a frame, and sent as one POST
/// when its reply is first read. The node frames each request by `Content-Length`, so
/// the frame's own length prefix is dropped, and the reply is framed the same way.
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
            let (_, reply) = exchange(
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

/// The local repository's capture sink: each capture goes to the node through `Uploader`.
struct Upload {
    link: Link,
    uploader: Uploader,
}

impl CheckpointSink for Upload {
    fn upload(
        &mut self,
        source: &Repository,
        operation: &OperationId,
        _before: &LayerHead,
        changes: &SparseChanges,
    ) -> Result<(), String> {
        match self
            .uploader
            .upload(&mut self.link, source, operation, changes)
        {
            Ok(LayerOutcome::Accepted { .. }) => Ok(()),
            Ok(other) => Err(format!("{other:?}")),
            Err(error) => Err(format!("{}: {}", error.code, error.detail)),
        }
    }
}

fn file(content: javelin::ObjectId) -> Option<FileEntry> {
    Some(FileEntry::Regular {
        content,
        executable: false,
    })
}

fn changes(entries: impl IntoIterator<Item = (&'static str, Option<FileEntry>)>) -> SparseChanges {
    SparseChanges::new(
        entries
            .into_iter()
            .map(|(path, entry)| (RepoPath::parse(path).unwrap(), entry)),
    )
    .unwrap()
}

#[test]
fn three_captures_reach_the_node_at_the_local_head() {
    let node_dir = TempDir::new().unwrap();
    let (log, node_head) = node(&node_dir);
    let served = serve_node(log);

    let local_dir = TempDir::new().unwrap();
    let mut local = Repository::init(local_dir.path()).unwrap();
    let base = local.layer_base(v(0)).unwrap();
    local.create_layer(&layer("work"), &base).unwrap();
    local.set_checkpoint_sink(Box::new(Upload {
        link: Link::new(served.addr),
        uploader: Uploader::new(layer("work"), node_head),
    }));

    let work = TempDir::new().unwrap();
    fs::write(work.path().join("a.txt"), "one").unwrap();
    let first = local
        .capture_directory(&layer("work"), work.path())
        .unwrap();
    fs::write(work.path().join("a.txt"), "two").unwrap();
    fs::create_dir(work.path().join("dir")).unwrap();
    fs::write(work.path().join("dir/b.txt"), "b").unwrap();
    let second = local
        .capture_directory(&layer("work"), work.path())
        .unwrap();
    fs::remove_file(work.path().join("a.txt")).unwrap();
    let third = local
        .capture_directory(&layer("work"), work.path())
        .unwrap();
    for capture in [&first, &second, &third] {
        assert_eq!(capture.upload, Some(Ok(())));
    }

    let log = served.finish();
    let node = match log.layer_state(&layer("work")).unwrap() {
        LayerState::Active { head } => head,
        other => panic!("node Layer is not active: {other:?}"),
    };
    let local_head = match local.layer_state(&layer("work")).unwrap() {
        LayerState::Active { head } => head,
        other => panic!("local Layer is not active: {other:?}"),
    };
    assert_eq!(
        (node.revision(), local_head.revision()),
        (3, 3),
        "seq must match"
    );
    assert_eq!(
        log.repo().checkpoint_manifest(node.checkpoint()).unwrap(),
        local.checkpoint_manifest(local_head.checkpoint()).unwrap(),
        "node head must hold the local head's files"
    );
    log.repo().check().unwrap();
}

#[test]
fn a_retry_after_an_upload_without_its_append_appends_once() {
    let node_dir = TempDir::new().unwrap();
    let (log, head) = node(&node_dir);
    let served = serve_node(log);

    let source_dir = TempDir::new().unwrap();
    let mut source = Repository::init(source_dir.path()).unwrap();
    let content = source.put_blob(b"one").unwrap();
    let edit = changes([("a.txt", file(content))]);

    // The uploader stops after the changes object is stored, before any append.
    let mut link = Link::new(served.addr);
    sender::upload_blobs(&mut link, &source, &edit).unwrap();
    sender::upload_changes(&mut link, &edit).unwrap();
    drop(link);

    let mut retry = Link::new(served.addr);
    let outcome = Uploader::new(layer("work"), head)
        .upload(&mut retry, &source, &op("write-1"), &edit)
        .unwrap();
    assert_eq!(outcome_seq(&outcome), Some(1));
    let node = sender::layer_head(&mut retry, &layer("work")).unwrap();
    assert_eq!(node.seq, 1);

    let log = served.finish();
    assert_eq!(log.repo().layer_history(&layer("work")).unwrap().len(), 2);
}

#[test]
fn a_lost_append_reply_replays_the_same_seq() {
    let node_dir = TempDir::new().unwrap();
    let (log, head) = node(&node_dir);
    let served = serve_node(log);

    let source_dir = TempDir::new().unwrap();
    let mut source = Repository::init(source_dir.path()).unwrap();
    let content = source.put_blob(b"one").unwrap();
    let edit = changes([("a.txt", file(content))]);

    let mut link = Link::new(served.addr);
    let first = Uploader::new(layer("work"), head)
        .upload(&mut link, &source, &op("write-1"), &edit)
        .unwrap();
    // The reply to the first append is lost, so the uploader still names `head` as parent.
    let replay = Uploader::new(layer("work"), head)
        .upload(&mut link, &source, &op("write-1"), &edit)
        .unwrap();
    assert_eq!(first, replay);
    assert_eq!(outcome_seq(&first), Some(1));

    let log = served.finish();
    assert_eq!(log.repo().layer_history(&layer("work")).unwrap().len(), 2);
}

#[test]
fn a_stale_parent_is_refused_and_leaves_the_layer_unchanged() {
    let node_dir = TempDir::new().unwrap();
    let (log, head) = node(&node_dir);
    let served = serve_node(log);

    let source_dir = TempDir::new().unwrap();
    let mut source = Repository::init(source_dir.path()).unwrap();
    let one = source.put_blob(b"one").unwrap();
    let two = source.put_blob(b"two").unwrap();

    let mut link = Link::new(served.addr);
    let accepted = Uploader::new(layer("work"), head)
        .upload(
            &mut link,
            &source,
            &op("write-1"),
            &changes([("a.txt", file(one))]),
        )
        .unwrap();
    let advanced = match accepted {
        LayerOutcome::Accepted { checkpoint, .. } => checkpoint,
        other => panic!("first append refused: {other:?}"),
    };

    // A writer that still holds the original head: its append must not land.
    let stale = changes([("b.txt", file(two))]);
    let mut stale_link = Link::new(served.addr);
    sender::upload_blobs(&mut stale_link, &source, &stale).unwrap();
    let stale_id = sender::upload_changes(&mut stale_link, &stale).unwrap();
    let outcome = sender::append(
        &mut stale_link,
        &op("write-2"),
        &layer("work"),
        Some(head),
        stale_id,
    )
    .unwrap();
    assert_eq!(outcome, LayerOutcome::Stale { head: advanced });

    let node = sender::layer_head(&mut stale_link, &layer("work")).unwrap();
    assert_eq!((node.seq, node.head), (1, advanced));

    let log = served.finish();
    log.repo().check().unwrap();
}

#[test]
fn a_blob_missing_from_the_node_is_refused_as_missing_object() {
    let node_dir = TempDir::new().unwrap();
    let (log, head) = node(&node_dir);
    let served = serve_node(log);

    let source_dir = TempDir::new().unwrap();
    let mut source = Repository::init(source_dir.path()).unwrap();
    let content = source.put_blob(b"never uploaded").unwrap();
    let edit = changes([("a.txt", file(content))]);

    // Only the changes object is uploaded; the blob it names is not.
    let mut link = Link::new(served.addr);
    let id = sender::upload_changes(&mut link, &edit).unwrap();
    let outcome =
        sender::append(&mut link, &op("write-1"), &layer("work"), Some(head), id).unwrap();
    assert_eq!(
        outcome,
        LayerOutcome::Refused {
            code: "missing_object".into()
        }
    );
    let node = sender::layer_head(&mut link, &layer("work")).unwrap();
    assert_eq!(node.seq, 0);

    let log = served.finish();
    assert_eq!(log.repo().layer_history(&layer("work")).unwrap().len(), 1);
}

#[test]
fn symlinks_upload_and_reverted_contents_append_after_the_current_head() {
    let node_dir = TempDir::new().unwrap();
    let (log, head) = node(&node_dir);
    let served = serve_node(log);

    let source_dir = TempDir::new().unwrap();
    let mut source = Repository::init(source_dir.path()).unwrap();
    let one = source.put_blob(b"one").unwrap();
    let two = source.put_blob(b"two").unwrap();
    let target = source.put_blob(b"a.txt").unwrap();
    let state = |content| {
        changes([
            ("a.txt", file(content)),
            ("link", Some(FileEntry::Symlink { target })),
        ])
    };

    // The third capture reverts to the first, so its checkpoint recurs in the history.
    let mut link = Link::new(served.addr);
    let mut uploader = Uploader::new(layer("work"), head);
    let mut seqs = Vec::new();
    for (n, content) in [one, two, one, two].into_iter().enumerate() {
        let outcome = uploader
            .upload(
                &mut link,
                &source,
                &op(&format!("write-{n}")),
                &state(content),
            )
            .unwrap();
        seqs.push(outcome_seq(&outcome));
    }
    assert_eq!(seqs, [Some(1), Some(2), Some(3), Some(4)]);

    // A lost reply to the second append: its parent is now also checkpoint 3.
    let first = match &Uploader::new(layer("work"), head)
        .upload(&mut link, &source, &op("write-0"), &state(one))
        .unwrap()
    {
        LayerOutcome::Accepted { checkpoint, .. } => *checkpoint,
        other => panic!("replay refused: {other:?}"),
    };
    let replay = Uploader::new(layer("work"), first)
        .upload(&mut link, &source, &op("write-1"), &state(two))
        .unwrap();
    assert_eq!(outcome_seq(&replay), Some(2));

    let log = served.finish();
    assert_eq!(log.repo().layer_history(&layer("work")).unwrap().len(), 5);
    log.repo().check().unwrap();
}

fn outcome_seq(outcome: &LayerOutcome) -> Option<u64> {
    match outcome {
        LayerOutcome::Accepted { seq, .. } => Some(*seq),
        _ => None,
    }
}
