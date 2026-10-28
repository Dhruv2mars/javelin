//! `javelin frontier`, `delay` and `embargo` with the real binary against a serving node: a
//! maintainer sets the delay, embargoes a Publish and releases it, and a signed-out reader sees
//! only the frontier and whether the view is delayed.

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    process::{Command, Output},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use javelin::{FileEntry, LayerId, OperationId, PublishAttribution, RepoPath, SparseChanges};
use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_identity::{Scope, test_keys};
use javelin_node::{FileWorldLog, auth, auth::Gate, bind_loopback, serve};
use javelin_protocol::{
    Message,
    codec::{PublishOutcome, decode, encode},
    receiver::WorldLog,
};
use serde_json::Value;
use tempfile::TempDir;

const REPO: &str = "repo-frontier";

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn owner_token() -> String {
    let t = now();
    let claims = test_keys::claims(
        "owner",
        REPO,
        &[Scope::Read, Scope::LayerWrite, Scope::Publish],
        t,
        t + 600,
        "owner-1",
    );
    test_keys::issuer().issue(&claims).unwrap()
}

/// Appends `path` to a new Layer `name` on the head; returns the Layer and its checkpoint.
fn append(
    log: &mut FileWorldLog,
    name: &str,
    path: &str,
    bytes: &[u8],
) -> (LayerId, javelin::ObjectId) {
    let layer = LayerId::parse(name).unwrap();
    let base = log.head().unwrap().number;
    log.create_layer(&layer, base).unwrap();
    let content = log.repository().put_blob(bytes).unwrap();
    let changes = SparseChanges::new([(
        RepoPath::parse(path).unwrap(),
        Some(FileEntry::Regular {
            content,
            executable: false,
        }),
    )])
    .unwrap();
    let head = log
        .append(
            &OperationId::parse(&format!("write-{name}")).unwrap(),
            &layer,
            &changes,
        )
        .unwrap();
    (layer, head.checkpoint().object())
}

/// A public repository with World 1, and Layer `fix` ready to publish as World 2. The node
/// serves until the test ends.
fn serving(dir: &TempDir) -> (String, javelin::LayerId, javelin::ObjectId) {
    let permissions = AppliedPermissions {
        repository: RepositoryId::parse(REPO).unwrap(),
        epoch: 1,
        visibility: Visibility::Public,
        roles: [(PrincipalId::parse("owner").unwrap(), Role::Maintain)]
            .into_iter()
            .collect(),
        required_approvals: 0,
    };
    auth::write_permissions(dir.path(), &permissions).unwrap();
    auth::write_keys(dir.path(), &test_keys::keys()).unwrap();
    let mut log = FileWorldLog::open(dir.path()).unwrap();
    let (seed, checkpoint) = append(&mut log, "seed", "src/a", b"a\n");
    let outcome = log
        .publish_layer(
            &OperationId::parse("publish-seed").unwrap(),
            &seed,
            checkpoint,
            log.head().unwrap().number,
            &PublishAttribution::Human,
        )
        .unwrap();
    assert!(matches!(outcome, PublishOutcome::Published { .. }));
    let (fix, checkpoint) = append(&mut log, "fix", "src/a", b"patched\n");
    let gate = Gate::load(dir.path(), REPO).unwrap();
    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || serve(&listener, REPO, &mut log, &gate, None));
    (format!("http://{addr}"), fix, checkpoint)
}

fn javelin(home: &TempDir, token: Option<&str>, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_javelin"));
    command
        .env("JAVELIN_HOME", home.path())
        .env_remove("JAVELIN_TOKEN")
        .env_remove("JAVELIN_CONTROL_URL")
        .arg("--json")
        .args(args);
    if let Some(token) = token {
        command.env("JAVELIN_TOKEN", token);
    }
    command.output().unwrap()
}

fn json(output: &Output) -> Value {
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Publishes Layer `fix` over the wire as the owner.
fn publish(node: &str, layer: &LayerId, checkpoint: javelin::ObjectId) {
    let body = encode(&Message::PublishLayer {
        operation: OperationId::parse("publish-fix").unwrap(),
        layer: layer.clone(),
        expected: checkpoint,
        base: javelin::VersionNumber::new(1).unwrap(),
        attribution: PublishAttribution::Human,
    })
    .unwrap();
    let addr: SocketAddr = node.trim_start_matches("http://").parse().unwrap();
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "POST /v2/r/{REPO} HTTP/1.1\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n",
        owner_token(),
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let reply = decode(&reply[split + 4..]).unwrap();
    assert!(
        matches!(
            reply,
            Message::PublishOutcome(PublishOutcome::Published { .. })
        ),
        "{reply:?}"
    );
}

#[test]
fn a_maintainer_delays_embargoes_and_releases_and_outsiders_see_the_frontier() {
    let dir = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let (node, fix, checkpoint) = serving(&dir);
    let owner = owner_token();
    let target = ["--node", node.as_str(), "--repository", REPO];
    let with = |args: &[&'static str]| -> Vec<&str> {
        let mut all = args.to_vec();
        all.extend_from_slice(&target);
        all
    };

    let signed_out = json(&javelin(&home, None, &with(&["frontier"])));
    assert_eq!(signed_out["frontier"], 1);
    assert_eq!(signed_out["delayed"], false);
    assert_eq!(signed_out["delay"], Value::Null);

    // A week's delay holds later versions; World 1 was already public and stays so.
    let set = json(&javelin(&home, Some(&owner), &with(&["delay", "7d"])));
    assert_eq!(set["delay"], 604_800);
    assert_eq!(set["frontier"], 1);
    let outsider = json(&javelin(&home, None, &with(&["delay"])));
    assert_eq!(outsider["delayed"], true);
    assert_eq!(
        outsider["delay"],
        Value::Null,
        "an outsider is not told the delay"
    );
    let plain = Command::new(env!("CARGO_BIN_EXE_javelin"))
        .env("JAVELIN_HOME", home.path())
        .env_remove("JAVELIN_TOKEN")
        .args(with(&["frontier"]))
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&plain.stdout),
        "Public frontier: World 1.\nThe public view is delayed.\n"
    );
    let off = json(&javelin(&home, Some(&owner), &with(&["delay", "0"])));
    assert_eq!(off["frontier"], 1);

    // Signed out, nothing changes and nothing is sent.
    let refused = javelin(&home, None, &with(&["delay", "1d"]));
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");

    // An embargo, then its Publish: World 2 stays above the frontier until released.
    json(&javelin(
        &home,
        Some(&owner),
        &with(&["embargo", "--operation", "publish-fix"]),
    ));
    publish(&node, &fix, checkpoint);
    assert_eq!(
        json(&javelin(&home, None, &with(&["frontier"])))["frontier"],
        1
    );
    // Signed out, attach brings the frontier, World 1, not the embargoed head.
    let clone = TempDir::new().unwrap();
    let url = format!("{node}/{REPO}");
    let attached = json(&javelin(
        &home,
        None,
        &["attach", url.as_str(), clone.path().to_str().unwrap()],
    ));
    assert_eq!(attached["head"]["number"], 1);
    let repo = clone.path().to_str().unwrap();
    let synced = json(&javelin(&home, None, &["sync", "--repo", repo]));
    assert_eq!(synced["head"]["number"], 1);
    assert_eq!(synced["advanced"], false);
    // Request list and show are Layer work: a caller without a role is refused.
    let listed = javelin(&home, None, &["request", "list", "--repo", repo]);
    assert_eq!(listed.status.code(), Some(1), "{listed:?}");
    assert!(
        String::from_utf8_lossy(&listed.stderr).contains("\"forbidden\""),
        "{listed:?}"
    );
    let member = json(&javelin(&home, Some(&owner), &with(&["frontier"])));
    assert_eq!(member["embargoes"][0]["version"], 2);
    assert_eq!(member["embargoes"][0]["until"], Value::Null);

    let released = json(&javelin(
        &home,
        Some(&owner),
        &with(&["embargo", "release", "2"]),
    ));
    assert_eq!(released["frontier"], 2);
    assert_eq!(
        json(&javelin(&home, None, &with(&["frontier"])))["frontier"],
        2
    );
    // Released, the signed-out clone syncs to World 2 from its attached head.
    let synced = json(&javelin(&home, None, &["sync", "--repo", repo]));
    assert_eq!(synced["from"]["number"], 1);
    assert_eq!(synced["head"]["number"], 2);
}
