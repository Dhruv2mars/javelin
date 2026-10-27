//! A Layer follows its owner across devices, end to end with the real binary and signed
//! tokens: the laptop's first append takes the Layer's writer lease, `javelin layer take`
//! on the desktop moves it and materializes the Layer, the laptop's queued append replays
//! into `layer_stale` and stays in its outbox, and `javelin layer clone` keeps its work as a
//! new Layer. Nothing is lost and nothing is forced (`docs/direction.md`, Layers follow
//! their owner).

use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::mpsc,
    thread,
};

use javelin::{
    AdvanceOutcome, CheckpointId, FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, Manifest,
    ObjectId, OperationId, RepoPath, Repository, SparseChanges, VersionNumber,
    outbox::{Kind, Outbox},
};
use javelin_collaboration::{PrincipalId, Role, Visibility};
use javelin_node::{FileWorldLog, bind_loopback, serve};
use javelin_protocol::{
    Message,
    codec::{Kind as ObjectKind, LayerOutcome, LeaseHolder, decode, encode},
    receiver::WorldLog,
};
use serde_json::Value;
use tempfile::TempDir;

#[path = "../../javelin-node/tests/wire_support/mod.rs"]
mod wire_support;

const REPO: &str = "demo";

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}

fn layer(s: &str) -> LayerId {
    LayerId::parse(s).unwrap()
}

/// A public node with Layer `work` over World 0, serving until the test process exits.
/// `owner` and `stranger` both hold Maintain. Returns its URL and the Layer's first head.
fn start_node(dir: &Path) -> (String, ObjectId) {
    let mut log = FileWorldLog::open(dir).unwrap();
    let first = log
        .create_layer(&layer("work"), VersionNumber::new(0).unwrap())
        .unwrap()
        .checkpoint()
        .object();
    let mut gate = wire_support::gate(REPO);
    let mut permissions = wire_support::permissions(REPO, Visibility::Public);
    permissions.epoch = 2;
    permissions
        .roles
        .insert(PrincipalId::parse("stranger").unwrap(), Role::Maintain);
    gate.apply(permissions).unwrap();
    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        let mut log = log;
        let _ = serve(&listener, REPO, &mut log, &gate, None);
    });
    (url, first)
}

/// One device: a project attached to the node, and the token its commands carry.
struct Device {
    home: PathBuf,
    project: PathBuf,
    token: String,
}

impl Device {
    fn attach(root: &Path, name: &str, url: &str, sub: &str) -> Self {
        let device = Self {
            home: root.join(format!("{name}-home")),
            project: root.join(name),
            token: wire_support::device_token(sub, REPO, &format!("dev-{name}"), None),
        };
        fs::create_dir_all(&device.project).unwrap();
        let attached = device.run(&["attach", &format!("{url}/{REPO}"), device.repo()]);
        assert_eq!(attached.status.code(), Some(0), "{attached:?}");
        device
    }

    fn repo(&self) -> &str {
        self.project.to_str().unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_javelin"))
            .current_dir(&self.project)
            .env("JAVELIN_HOME", &self.home)
            .env("JAVELIN_TOKEN", &self.token)
            .env_remove("JAVELIN_CONTROL_URL")
            .arg("--json")
            .args(args)
            .output()
            .unwrap()
    }

    /// Runs and returns the exit code and JSON: stdout for an outcome, stderr for an error.
    fn json(&self, args: &[&str]) -> (Option<i32>, Value) {
        let output = self.run(args);
        let bytes = if output.stdout.is_empty() {
            &output.stderr
        } else {
            &output.stdout
        };
        let value = serde_json::from_slice(bytes).unwrap_or_else(|_| panic!("{output:?}"));
        (output.status.code(), value)
    }

    fn replay(&self) -> (Option<i32>, Value) {
        self.json(&["outbox", "replay", "--remote", REPO, "--repo", self.repo()])
    }

    /// Captures `notes.txt` = `bytes` locally on Layer `work`, then queues its upload and
    /// its append after the node's `parent`, as streaming checkpoints do. Returns the local
    /// head.
    fn capture_and_queue(&self, n: u32, bytes: &[u8], parent: ObjectId) -> LayerHead {
        let mut repo = Repository::open(&self.project).unwrap();
        let head = match repo.layer_state(&layer("work")) {
            Ok(LayerState::Active { head }) => head,
            Err(_) => {
                let base = repo.layer_base(VersionNumber::new(0).unwrap()).unwrap();
                repo.create_layer(&layer("work"), &base).unwrap()
            }
            Ok(other) => panic!("{other:?}"),
        };
        let content = repo.put_blob(bytes).unwrap();
        let changes = SparseChanges::new([(
            RepoPath::parse("notes.txt").unwrap(),
            Some(FileEntry::Regular {
                content,
                executable: false,
            }),
        )])
        .unwrap();
        let head = match repo
            .capture_layer(&op(&format!("capture-{n}")), &head, &changes)
            .unwrap()
        {
            LayerUpdate::Applied {
                state: LayerState::Active { head },
            } => head,
            other => panic!("{other:?}"),
        };
        let checkpoint = changes.canonical_bytes();
        let id = ObjectKind::Checkpoint.id(&checkpoint);
        let mut outbox = Outbox::open(&self.project, REPO).unwrap();
        // Blobs and changes objects go in separate batches.
        let blob = encode(&Message::Objects(vec![(
            ObjectKind::Blob,
            content,
            bytes.to_vec(),
        )]))
        .unwrap();
        let upload = encode(&Message::Objects(vec![(
            ObjectKind::Checkpoint,
            id,
            checkpoint,
        )]))
        .unwrap();
        let append = encode(&Message::LayerAppend {
            operation: op(&format!("append-{n}")),
            layer: layer("work"),
            parent: Some(parent),
            checkpoint: id,
        })
        .unwrap();
        outbox
            .enqueue(Kind::Upload, "work", &op(&format!("blob-{n}")), &blob)
            .unwrap();
        outbox
            .enqueue(Kind::Upload, "work", &op(&format!("upload-{n}")), &upload)
            .unwrap();
        outbox
            .enqueue(Kind::Append, "work", &op(&format!("append-{n}")), &append)
            .unwrap();
        head
    }
}

#[test]
fn a_layer_follows_its_owner_and_the_old_device_keeps_its_queued_work() {
    let root = TempDir::new().unwrap();
    let (url, first) = start_node(&root.path().join("node"));
    let laptop = Device::attach(root.path(), "laptop", &url, wire_support::OWNER);
    let desktop = Device::attach(root.path(), "desktop", &url, wire_support::OWNER);

    // The laptop's first append takes the lease.
    laptop.capture_and_queue(1, b"edit 1\n", first);
    let (code, value) = laptop.replay();
    assert_eq!(code, Some(0), "{value}");
    assert_eq!(value["pending"], 0);
    let reply = Outbox::open(&laptop.project, REPO)
        .unwrap()
        .entry(3)
        .unwrap()
        .outcome
        .clone()
        .unwrap();
    let Ok(Message::LayerOutcome(LayerOutcome::Accepted { seq: 1, checkpoint })) = decode(&reply)
    else {
        panic!("the first append was not accepted: {:?}", decode(&reply));
    };

    // The laptop captures again, offline: the append waits in its outbox.
    let kept = laptop.capture_and_queue(2, b"edit 2\n", checkpoint);

    // The desktop takes the lease and gets the Layer at the node's head, in a directory.
    let (code, taken) = desktop.json(&["layer", "take", "work", "--repo", desktop.repo()]);
    assert_eq!(code, Some(0), "{taken}");
    assert_eq!(taken["node"]["seq"], 1);
    assert_eq!(taken["node"]["head"], checkpoint.to_string());
    assert_eq!(taken["created"], true);
    assert_eq!(taken["status"], "imported");
    let workspace = PathBuf::from(taken["root"].as_str().unwrap());
    assert_eq!(fs::read(workspace.join("notes.txt")).unwrap(), b"edit 1\n");

    // Taking again from the same device changes nothing and refreshes the directory.
    let (code, again) = desktop.json(&["layer", "take", "work", "--repo", desktop.repo()]);
    assert_eq!(code, Some(0), "{again}");
    assert_eq!(again["status"], "current");
    assert_eq!(again["created"], false);

    // The laptop's queued append replays into layer_stale. It stays in the outbox, reported.
    let (code, value) = laptop.replay();
    assert_eq!(code, Some(1), "{value}");
    assert_eq!(value["code"], "layer_stale");
    assert_eq!(value["pending"], 0);
    let stale = &value["stale"][0];
    assert_eq!(stale["layer"], "work");
    assert_eq!(stale["operation"], "append-2");
    assert_eq!(stale["node_head"], checkpoint.to_string());
    assert_eq!(stale["local_head"], serde_json::json!(kept.checkpoint()));
    let outbox = Outbox::open(&laptop.project, REPO).unwrap();
    let record = outbox.entry(6).unwrap();
    assert_eq!(record.operation.as_str(), "append-2");
    assert!(!record.payload.is_empty(), "the queued request is kept");
    drop(outbox);
    let (code, status) = laptop.json(&[
        "outbox",
        "status",
        "--remote",
        REPO,
        "--repo",
        laptop.repo(),
    ]);
    assert_eq!(code, Some(0), "{status}");
    assert_eq!(status["stale"][0]["operation"], "append-2");

    // Nothing was forced: the node is where the desktop left it.
    let (code, state) = desktop.json(&["layer", "refresh", "work", "--repo", desktop.repo()]);
    assert_eq!(code, Some(0), "{state}");
    assert_eq!(state["status"], "current");
    assert_eq!(state["node"]["seq"], 1);

    // The laptop keeps its work as a new Layer, from the checkpoint the report names.
    let local = stale["local_head"].as_str().unwrap();
    let (code, cloned) = laptop.json(&[
        "layer",
        "clone",
        "rescued",
        "--checkpoint",
        local,
        "--repo",
        laptop.repo(),
    ]);
    assert_eq!(code, Some(0), "{cloned}");
    let (code, opened) = laptop.json(&["workspace", "open", "rescued", "--repo", laptop.repo()]);
    assert_eq!(code, Some(0), "{opened}");
    let rescued = PathBuf::from(opened["workspace"]["root"].as_str().unwrap());
    assert_eq!(fs::read(rescued.join("notes.txt")).unwrap(), b"edit 2\n");
    let repo = Repository::open(&laptop.project).unwrap();
    let LayerState::Active { head } = repo.layer_state(&layer("rescued")).unwrap() else {
        panic!("the clone is not active");
    };
    assert_eq!(
        repo.read_checkpoint(head.checkpoint()).unwrap().changes(),
        repo.read_checkpoint(CheckpointId::parse(local).unwrap())
            .unwrap()
            .changes()
    );
}

#[test]
fn another_principal_cannot_take_the_lease() {
    let root = TempDir::new().unwrap();
    let (url, first) = start_node(&root.path().join("node"));
    let laptop = Device::attach(root.path(), "laptop", &url, wire_support::OWNER);
    laptop.capture_and_queue(1, b"edit 1\n", first);
    assert_eq!(laptop.replay().0, Some(0));

    // `stranger` may write to the repository, but the Layer is `owner`'s.
    let intruder = Device {
        home: root.path().join("intruder-home"),
        project: laptop.project.clone(),
        token: wire_support::device_token("stranger", REPO, "dev-stranger", None),
    };
    let (code, value) = intruder.json(&["layer", "take", "work", "--repo", laptop.repo()]);
    assert_eq!(code, Some(1), "{value}");
    assert_eq!(value["error"]["code"], "forbidden");

    // A Layer-limited token for another Layer cannot take this one's lease.
    let agent = Device {
        home: root.path().join("agent-home"),
        project: laptop.project.clone(),
        token: wire_support::device_token(wire_support::OWNER, REPO, "dev-agent", Some("other")),
    };
    let (code, value) = agent.json(&["layer", "take", "work", "--repo", laptop.repo()]);
    assert_eq!(code, Some(1), "{value}");
    assert_eq!(value["error"]["code"], "forbidden");
}

type Job = Box<dyn FnOnce(&mut FileWorldLog) + Send>;

/// A node served in process that can change between requests: `change` runs a job on its
/// log between two connections, the way a Publish on the node would move its World.
struct Live {
    url: String,
    addr: SocketAddr,
    jobs: mpsc::Sender<Job>,
    done: mpsc::Receiver<()>,
}

impl Live {
    fn start(dir: &Path, seed: impl FnOnce(&mut FileWorldLog)) -> Self {
        let mut log = FileWorldLog::open(dir).unwrap();
        seed(&mut log);
        let listener = bind_loopback("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (jobs, queue) = mpsc::channel::<Job>();
        let (finished, done) = mpsc::channel();
        thread::spawn(move || {
            let gate = wire_support::gate(REPO);
            loop {
                let _ = serve(&listener, REPO, &mut log, &gate, Some(1));
                while let Ok(job) = queue.try_recv() {
                    job(&mut log);
                    finished.send(()).unwrap();
                }
            }
        });
        Self {
            url: format!("http://{addr}"),
            addr,
            jobs,
            done,
        }
    }

    /// Runs `job` on the node's log. A bare connection wakes the serving loop.
    fn change(&self, job: impl FnOnce(&mut FileWorldLog) + Send + 'static) {
        self.jobs.send(Box::new(job)).unwrap();
        drop(TcpStream::connect(self.addr).unwrap());
        self.done.recv().unwrap();
    }

    /// Who holds `name`'s writer lease, as the caller of `token` sees it.
    fn lease(&self, token: &str, name: &str) -> LeaseHolder {
        let body = encode(&Message::LayerHead { layer: layer(name) }).unwrap();
        let mut stream = TcpStream::connect(self.addr).unwrap();
        write!(
            stream,
            "POST /v2/r/{REPO} HTTP/1.1\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).unwrap();
        let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        match decode(&reply[split + 4..]).unwrap() {
            Message::LayerState { lease, .. } => lease,
            other => panic!("not a Layer state: {other:?}"),
        }
    }
}

/// Advances the node's World to a tree holding only `world.txt` = `bytes`.
fn advance(log: &mut FileWorldLog, operation: &str, bytes: &[u8]) -> VersionNumber {
    let repo = log.repository();
    let content = repo.put_blob(bytes).unwrap();
    let tree = repo
        .put_snapshot(
            &Manifest::new([(
                RepoPath::parse("world.txt").unwrap(),
                FileEntry::Regular {
                    content,
                    executable: false,
                },
            )])
            .unwrap(),
        )
        .unwrap();
    let head = repo.head().unwrap().number;
    match repo.advance_to_tree(&op(operation), head, tree).unwrap() {
        AdvanceOutcome::Accepted { version } => version.number,
        other => panic!("{other:?}"),
    }
}

#[test]
fn take_finds_the_base_world_by_tree_and_refuses_before_moving_the_lease() {
    let root = TempDir::new().unwrap();
    // Remote Worlds 1 and 2; attach makes remote World 2 local World 1.
    let node = Live::start(&root.path().join("node"), |log| {
        let one = advance(log, "world-1", b"one\n");
        let two = advance(log, "world-2", b"two\n");
        log.create_layer(&layer("ancient"), one).unwrap();
        log.create_layer(&layer("work"), two).unwrap();
        let content = log.repository().put_blob(b"notes\n").unwrap();
        let changes = SparseChanges::new([(
            RepoPath::parse("notes.txt").unwrap(),
            Some(FileEntry::Regular {
                content,
                executable: false,
            }),
        )])
        .unwrap();
        log.append(&op("write-1"), &layer("work"), &changes)
            .unwrap();
    });
    let desktop = Device::attach(root.path(), "desktop", &node.url, wire_support::OWNER);
    let laptop = Device::attach(root.path(), "laptop", &node.url, wire_support::OWNER);

    // `work`'s base is remote World 2, which this repository holds as World 1.
    let (code, taken) = desktop.json(&["layer", "take", "work", "--repo", desktop.repo()]);
    assert_eq!(code, Some(0), "{taken}");
    assert_eq!(taken["holds_lease"], true);
    let workspace = PathBuf::from(taken["root"].as_str().unwrap());
    assert_eq!(fs::read(workspace.join("world.txt")).unwrap(), b"two\n");
    assert_eq!(fs::read(workspace.join("notes.txt")).unwrap(), b"notes\n");
    let local = Repository::open(&desktop.project).unwrap();
    let LayerState::Active { head } = local.layer_state(&layer("work")).unwrap() else {
        panic!("the taken Layer is not active");
    };
    let base = local
        .read_checkpoint(head.checkpoint())
        .unwrap()
        .base()
        .version();
    assert_eq!(base.get(), 1, "remote World 2 is local World 1");
    drop(local);

    // `ancient`'s base, remote World 1, predates the attachment and is not held here: the
    // take is refused before the lease moves, and no local Layer is made.
    let (code, refused) = desktop.json(&["layer", "take", "ancient", "--repo", desktop.repo()]);
    assert_eq!(code, Some(2), "{refused}");
    assert_eq!(refused["error"]["code"], "invalid_input");
    assert_eq!(node.lease(&desktop.token, "ancient"), LeaseHolder::Free);
    assert!(
        Repository::open(&desktop.project)
            .unwrap()
            .layer_state(&layer("ancient"))
            .is_err()
    );

    // A base newer than the attachment is fetched, as attach does, and becomes local World 2.
    node.change(|log| {
        let three = advance(log, "world-3", b"three\n");
        log.create_layer(&layer("newer"), three).unwrap();
    });
    let (code, newer) = desktop.json(&["layer", "take", "newer", "--repo", desktop.repo()]);
    assert_eq!(code, Some(0), "{newer}");
    let workspace = PathBuf::from(newer["root"].as_str().unwrap());
    assert_eq!(fs::read(workspace.join("world.txt")).unwrap(), b"three\n");
    let local = Repository::open(&desktop.project).unwrap();
    assert_eq!(local.head().unwrap().number.get(), 2);
    assert_eq!(local.attachment().unwrap().unwrap().head.number, 3);
    drop(local);

    // A replayed take reports what it recorded, and says the lease has moved on since.
    let (code, mine) = laptop.json(&[
        "layer",
        "take",
        "work",
        "--operation",
        "take-laptop",
        "--repo",
        laptop.repo(),
    ]);
    assert_eq!(code, Some(0), "{mine}");
    assert_eq!(mine["lease"], "this_device");
    let (code, _) = desktop.json(&["layer", "take", "work", "--repo", desktop.repo()]);
    assert_eq!(code, Some(0));
    let replay = laptop.run(&[
        "layer",
        "take",
        "work",
        "--operation",
        "take-laptop",
        "--repo",
        laptop.repo(),
    ]);
    let replayed: Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(replay.status.code(), Some(0), "{replayed}");
    assert_eq!(replayed["holds_lease"], false);
    assert_eq!(replayed["lease"], "another_device");
    assert_eq!(replayed["node"], mine["node"], "the recorded take");
    let text = Command::new(env!("CARGO_BIN_EXE_javelin"))
        .env("JAVELIN_HOME", &laptop.home)
        .env("JAVELIN_TOKEN", &laptop.token)
        .args([
            "layer",
            "take",
            "work",
            "--operation",
            "take-laptop",
            "--repo",
            laptop.repo(),
        ])
        .output()
        .unwrap();
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("Take take-laptop is recorded"), "{text}");
    assert!(
        text.contains("Another device holds the lease now"),
        "{text}"
    );
}

#[test]
fn take_never_overwrites_a_local_world_the_node_lacks() {
    let root = TempDir::new().unwrap();
    let node = Live::start(&root.path().join("node"), |log| {
        advance(log, "world-1", b"one\n");
    });
    let desktop = Device::attach(root.path(), "desktop", &node.url, wire_support::OWNER);
    let attached = Repository::open(&desktop.project).unwrap().head().unwrap();

    // A local Publish in the attached repository: its World now has a version the node lacks.
    let (code, created) = desktop.json(&["workspace", "create", "mine", "--repo", desktop.repo()]);
    assert_eq!(code, Some(0), "{created}");
    let dir = PathBuf::from(created["workspace"]["root"].as_str().unwrap());
    fs::write(dir.join("local.txt"), b"only here\n").unwrap();
    let (code, published) = desktop.json(&[
        "workspace",
        "publish",
        "mine",
        "--operation",
        "publish-local",
        "--repo",
        desktop.repo(),
    ]);
    assert_eq!(code, Some(0), "{published}");
    let local = Repository::open(&desktop.project).unwrap().head().unwrap();
    assert_eq!(local.number.get(), attached.number.get() + 1);

    // The node moves on, with a Layer over its newer World.
    node.change(|log| {
        let two = advance(log, "world-2", b"two\n");
        log.create_layer(&layer("newer"), two).unwrap();
    });

    // Following the node would replace the local World's tree: refused before the lease moves.
    let (code, refused) = desktop.json(&["layer", "take", "newer", "--repo", desktop.repo()]);
    assert_eq!(code, Some(1), "{refused}");
    assert_eq!(refused["error"]["code"], "local_world_diverged");
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(message.contains(&local.id.to_string()), "{message}");
    assert!(message.contains("fresh `javelin attach`"), "{message}");
    assert_eq!(node.lease(&desktop.token, "newer"), LeaseHolder::Free);
    let repo = Repository::open(&desktop.project).unwrap();
    assert_eq!(repo.head().unwrap(), local, "the local World is untouched");
    assert_eq!(repo.attachment().unwrap().unwrap().head.number, 1);
    assert!(repo.layer_state(&layer("newer")).is_err());
    drop(repo);

    // A fresh attach of the same node takes it.
    let fresh = Device::attach(root.path(), "fresh", &node.url, wire_support::OWNER);
    let (code, taken) = fresh.json(&["layer", "take", "newer", "--repo", fresh.repo()]);
    assert_eq!(code, Some(0), "{taken}");
}

#[test]
fn a_take_stopped_between_the_sync_and_attach_json_is_finished_by_the_next_take() {
    let root = TempDir::new().unwrap();
    let node = Live::start(&root.path().join("node"), |log| {
        advance(log, "world-1", b"one\n");
    });
    let desktop = Device::attach(root.path(), "desktop", &node.url, wire_support::OWNER);
    node.change(|log| {
        let two = advance(log, "world-2", b"two\n");
        log.create_layer(&layer("newer"), two).unwrap();
    });

    // The take stops after the local World follows the node, before `attach.json` says so,
    // and before the lease moves.
    let stopped = Command::new(env!("CARGO_BIN_EXE_javelin"))
        .env("JAVELIN_HOME", &desktop.home)
        .env("JAVELIN_TOKEN", &desktop.token)
        .env("JAVELIN_FAILPOINT", "take_after_sync_advance")
        .args(["--json", "layer", "take", "newer", "--repo", desktop.repo()])
        .output()
        .unwrap();
    assert_eq!(stopped.status.code(), Some(86), "{stopped:?}");
    let repo = Repository::open(&desktop.project).unwrap();
    assert_eq!(repo.head().unwrap().number.get(), 2, "the World advanced");
    assert_eq!(repo.attachment().unwrap().unwrap().head.number, 1);
    drop(repo);
    assert_eq!(node.lease(&desktop.token, "newer"), LeaseHolder::Free);

    // The retry finishes `attach.json` from the node, then takes the Layer.
    let (code, taken) = desktop.json(&["layer", "take", "newer", "--repo", desktop.repo()]);
    assert_eq!(code, Some(0), "{taken}");
    let workspace = PathBuf::from(taken["root"].as_str().unwrap());
    assert_eq!(fs::read(workspace.join("world.txt")).unwrap(), b"two\n");
    let repo = Repository::open(&desktop.project).unwrap();
    assert_eq!(repo.head().unwrap().number.get(), 2, "no second advance");
    assert_eq!(repo.attachment().unwrap().unwrap().head.number, 2);
    drop(repo);

    // A later take follows the node again from the repaired attachment.
    node.change(|log| {
        let three = advance(log, "world-3", b"three\n");
        log.create_layer(&layer("later"), three).unwrap();
    });
    let (code, later) = desktop.json(&["layer", "take", "later", "--repo", desktop.repo()]);
    assert_eq!(code, Some(0), "{later}");
    let repo = Repository::open(&desktop.project).unwrap();
    assert_eq!(repo.head().unwrap().number.get(), 3);
    assert_eq!(repo.attachment().unwrap().unwrap().head.number, 3);
}

#[test]
fn a_sync_named_world_the_node_does_not_hold_is_still_divergence() {
    let root = TempDir::new().unwrap();
    let node = Live::start(&root.path().join("node"), |log| {
        advance(log, "world-1", b"one\n");
    });
    let desktop = Device::attach(root.path(), "desktop", &node.url, wire_support::OWNER);
    // A local advance that names a sync to a remote version the node never had.
    let mut repo = Repository::open(&desktop.project).unwrap();
    let content = repo.put_blob(b"forged\n").unwrap();
    let tree = repo
        .put_snapshot(
            &Manifest::new([(
                RepoPath::parse("world.txt").unwrap(),
                FileEntry::Regular {
                    content,
                    executable: false,
                },
            )])
            .unwrap(),
        )
        .unwrap();
    let forged = format!("sync-2-{}", ObjectId::for_blob(b"not on the node"));
    let head = repo.head().unwrap().number;
    repo.advance_to_tree(&op(&forged), head, tree).unwrap();
    drop(repo);
    node.change(|log| {
        let two = advance(log, "world-2", b"two\n");
        log.create_layer(&layer("newer"), two).unwrap();
    });
    let (code, refused) = desktop.json(&["layer", "take", "newer", "--repo", desktop.repo()]);
    assert_eq!(code, Some(1), "{refused}");
    assert_eq!(refused["error"]["code"], "local_world_diverged");
    let repo = Repository::open(&desktop.project).unwrap();
    assert_eq!(repo.attachment().unwrap().unwrap().head.number, 1);
}
