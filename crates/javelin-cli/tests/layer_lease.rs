//! A Layer follows its owner across devices, end to end with the real binary and signed
//! tokens: the laptop's first append takes the Layer's writer lease, `javelin layer take`
//! on the desktop moves it and materializes the Layer, the laptop's queued append replays
//! into `layer_stale` and stays in its outbox, and `javelin layer clone` keeps its work as a
//! new Layer. Nothing is lost and nothing is forced (`docs/direction.md`, Layers follow
//! their owner).

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
};

use javelin::{
    CheckpointId, FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, ObjectId, OperationId,
    RepoPath, Repository, SparseChanges, VersionNumber,
    outbox::{Kind, Outbox},
};
use javelin_collaboration::{PrincipalId, Role, Visibility};
use javelin_node::{FileWorldLog, bind_loopback, serve};
use javelin_protocol::{
    Message,
    codec::{Kind as ObjectKind, LayerOutcome, decode, encode},
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
