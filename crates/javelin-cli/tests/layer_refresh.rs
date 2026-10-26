//! `javelin layer refresh` against a loopback node: after a Stack on the node, the local
//! Layer and its workspace move to the Stack's head, and a later capture keeps it.

use std::{
    fs,
    io::{self, Cursor, Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    path::Path,
    process::{Command, Output},
    thread,
};

use javelin::{
    AdvanceOutcome, FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, Manifest, OperationId,
    RepoPath, Repository, SparseChanges, VersionNumber,
};
use javelin_node::{FileWorldLog, bind_loopback, serve};
use javelin_protocol::codec::LayerOutcome;
use serde_json::Value;
use tempfile::TempDir;

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
fn path(s: &str) -> RepoPath {
    RepoPath::parse(s).unwrap()
}

fn file(repo: &mut Repository, bytes: &[u8]) -> (RepoPath, Option<FileEntry>) {
    (
        path("notes.txt"),
        Some(FileEntry::Regular {
            content: repo.put_blob(bytes).unwrap(),
            executable: false,
        }),
    )
}

fn capture(
    repo: &mut Repository,
    operation: &str,
    head: &LayerHead,
    changes: SparseChanges,
) -> LayerHead {
    match repo.capture_layer(&op(operation), head, &changes).unwrap() {
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

/// A node whose World 1 holds `notes.txt`, with Layer `target` editing its first line and
/// Layer `source` its last line plus `new.txt`. Serves repository `demo` until the test
/// process exits. Returns the URL and the two heads.
fn start_node(dir: &Path) -> (String, SocketAddr, LayerHead, LayerHead) {
    let mut repo = Repository::init(dir).unwrap();
    let (notes, base) = file(&mut repo, BASE);
    let tree = repo
        .put_snapshot(&Manifest::new([(notes, base.unwrap())]).unwrap())
        .unwrap();
    let outcome = repo
        .advance_to_tree(&op("seed-1"), VersionNumber::new(0).unwrap(), tree)
        .unwrap();
    assert!(matches!(outcome, AdvanceOutcome::Accepted { .. }));
    let world = repo.layer_base(VersionNumber::new(1).unwrap()).unwrap();
    let target = repo.create_layer(&layer("target"), &world).unwrap();
    let source = repo.create_layer(&layer("source"), &world).unwrap();
    let ours = SparseChanges::new([file(&mut repo, OURS)]).unwrap();
    let new = (
        path("new.txt"),
        Some(FileEntry::Regular {
            content: repo.put_blob(b"new\n").unwrap(),
            executable: false,
        }),
    );
    let theirs = SparseChanges::new([file(&mut repo, THEIRS), new]).unwrap();
    let target = capture(&mut repo, "target-1", &target, ours);
    let source = capture(&mut repo, "source-1", &source, theirs);
    drop(repo);

    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let dir = dir.to_path_buf();
    thread::spawn(move || {
        let mut log = FileWorldLog::open(&dir).unwrap();
        serve(&listener, "demo", &mut log, None).unwrap();
    });
    (format!("http://{addr}/demo"), addr, target, source)
}

/// A protocol session over HTTP: each frame is sent as one POST when its reply is read.
struct Link {
    addr: SocketAddr,
    outbound: Vec<u8>,
    inbox: Cursor<Vec<u8>>,
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
            let mut stream = TcpStream::connect(self.addr)?;
            write!(
                stream,
                "POST /v2/r/demo HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )?;
            stream.write_all(body)?;
            stream.shutdown(Shutdown::Write)?;
            let mut reply = Vec::new();
            stream.read_to_end(&mut reply)?;
            let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            let body = &reply[split + 4..];
            let mut framed = (body.len() as u32).to_be_bytes().to_vec();
            framed.extend(body);
            self.inbox = Cursor::new(framed);
        }
        self.inbox.read(buf)
    }
}

fn javelin(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_javelin"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

/// Runs with `--json` and returns the structured output for the expected exit code.
fn json(cwd: &Path, args: &[&str], code: i32) -> Value {
    let mut args = args.to_vec();
    args.push("--json");
    let output = javelin(cwd, &args);
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    let bytes = if code == 0 {
        &output.stdout
    } else {
        &output.stderr
    };
    serde_json::from_slice(bytes).unwrap()
}

#[test]
fn refresh_brings_the_layer_and_its_workspace_to_a_node_stack() {
    let node = TempDir::new().unwrap();
    let (url, addr, target, source) = start_node(node.path());
    let root = TempDir::new().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    json(root.path(), &["attach", &url, "project"], 0);
    let repo = project.to_str().unwrap();

    // The local Layer mirrors the node's: the same base and the same first capture.
    json(
        root.path(),
        &["workspace", "create", "target", "--repo", repo],
        0,
    );
    let workspace = Repository::open(&project)
        .unwrap()
        .workspace_root(&layer("target"))
        .unwrap();
    fs::write(workspace.join("notes.txt"), OURS).unwrap();
    json(root.path(), &["checkpoint", "target", "--repo", repo], 0);
    let current = json(
        root.path(),
        &["layer", "refresh", "target", "--repo", repo],
        0,
    );
    assert_eq!(current["status"], "current");
    assert_eq!(current["node"]["seq"], 1);

    // A Stack on the node moves its head; the local Layer has not seen it.
    let mut link = Link {
        addr,
        outbound: Vec::new(),
        inbox: Cursor::new(Vec::new()),
    };
    let outcome = javelin_stack::remote::stack(
        &mut link,
        &op("stack-1"),
        &layer("target"),
        target.checkpoint().object(),
        source.checkpoint().object(),
    )
    .unwrap();
    let LayerOutcome::Accepted { seq, checkpoint } = outcome else {
        panic!("{outcome:?}");
    };
    // An edit made before the refresh is kept.
    fs::write(workspace.join("draft.txt"), b"draft\n").unwrap();

    let refreshed = json(
        root.path(),
        &["layer", "refresh", "target", "--repo", repo],
        0,
    );
    assert_eq!(refreshed["status"], "imported");
    assert_eq!(refreshed["node"]["seq"], seq);
    assert_eq!(refreshed["node"]["head"], checkpoint.to_string());
    assert_eq!(refreshed["head"]["revision"], 2);
    assert_eq!(refreshed["workspace"]["refreshed"], true);
    assert_eq!(fs::read(workspace.join("notes.txt")).unwrap(), MERGED);
    assert_eq!(fs::read(workspace.join("new.txt")).unwrap(), b"new\n");
    assert_eq!(fs::read(workspace.join("draft.txt")).unwrap(), b"draft\n");

    // Capture after the refresh builds on the Stack's head rather than reverting it.
    json(root.path(), &["checkpoint", "target", "--repo", repo], 0);
    let local = Repository::open(&project).unwrap();
    let head = active(&local, "target");
    assert_eq!(head.revision(), 3);
    let manifest = local.checkpoint_manifest(head.checkpoint()).unwrap();
    let notes = manifest.entries()[&path("notes.txt")].content();
    assert_eq!(local.read_blob(notes).unwrap(), MERGED);
    assert!(manifest.entries().contains_key(&path("new.txt")));
    assert!(manifest.entries().contains_key(&path("draft.txt")));
    local.check().unwrap();
    drop(local);

    // That capture never reached the node, so a refresh refuses to replace it.
    let diverged = json(
        root.path(),
        &["layer", "refresh", "target", "--repo", repo],
        1,
    );
    assert_eq!(diverged["error"]["code"], "layer_diverged");
}

#[test]
fn refresh_needs_an_attached_repository() {
    let root = TempDir::new().unwrap();
    json(root.path(), &["init"], 0);
    let refused = json(root.path(), &["layer", "refresh", "target"], 2);
    assert_eq!(refused["error"]["code"], "invalid_input");
}
