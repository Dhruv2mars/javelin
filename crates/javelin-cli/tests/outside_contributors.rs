//! Outside contributors and `request publish --embargo`, end to end with the real binary
//! against the node's server on loopback, with real tokens. In a public repository `carol`,
//! signed in with no role, attaches at the public frontier and opens a Publish Request from
//! a Layer of their own; `rita` approves it and `mona`, a maintainer, publishes it. A held
//! version is never readable to carol until a maintainer releases it, and an embargo given
//! to `request publish` travels in the Publish itself, through an outage too.

use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::Output,
};

use javelin_collaboration::{Role, Visibility};
use javelin_identity::Scope;
use javelin_protocol::{
    Message,
    codec::{Kind, decode, encode},
};
use serde_json::Value;

mod wire_support;

use wire_support::{Deployment, closed_url, error_code, layer, now, stdout};

const CONTRIBUTOR: &[Scope] = &[Scope::Read, Scope::LayerWrite, Scope::ReviewWrite];
const REVIEWER: &[Scope] = &[Scope::Read, Scope::ReviewWrite, Scope::ReviewApprove];
const MAINTAINER: &[Scope] = &[
    Scope::Read,
    Scope::LayerWrite,
    Scope::Publish,
    Scope::ReviewWrite,
    Scope::ReviewApprove,
];

fn describe(output: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The JSON of a command that must succeed.
fn ok(output: Output) -> Value {
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    stdout(&output)
}

/// The error code of a command that must fail with exit 1.
fn refused(output: Output) -> String {
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    error_code(&output)
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// Points the clone at `node`: a closed URL is an outage, as the CLI sees one.
fn set_node(dir: &Path, node: &str) {
    let path = dir.join(".javelin").join("attach.json");
    let mut attachment: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    attachment["node"] = node.into();
    fs::write(&path, serde_json::to_vec(&attachment).unwrap()).unwrap();
}

struct Team {
    deployment: Deployment,
    carol: String,
    rita: String,
    mona: String,
}

impl Team {
    /// A public repository: `rita` reviews and `mona` maintains; `carol` has no role. One
    /// approval is required.
    fn start() -> Self {
        let deployment = Deployment::start_as(
            Visibility::Public,
            &[("rita", Role::Review), ("mona", Role::Maintain)],
            1,
            |_| Vec::new(),
        );
        Self {
            carol: deployment.token("carol", CONTRIBUTOR),
            rita: deployment.token("rita", REVIEWER),
            mona: deployment.token("mona", MAINTAINER),
            deployment,
        }
    }

    fn run(&self, token: &str, args: &[&str]) -> Output {
        self.deployment.javelin_as(token, args)
    }

    /// Attaches a clone named `name` as the holder of `token`.
    fn attach(&self, name: &str, token: &str) -> (PathBuf, Value) {
        let dir = self.deployment.temp.path().join(name);
        let url = self.deployment.attach_url();
        let attached = ok(self.run(token, &["attach", url.as_str(), s(&dir)]));
        (dir, attached)
    }

    /// `javelin frontier`, as the holder of `token`.
    fn frontier(&self, token: &str) -> Value {
        let node = &self.deployment.node_url;
        let repository = &self.deployment.repository;
        ok(self.run(
            token,
            &["frontier", "--node", node, "--repository", repository],
        ))
    }

    /// One wire message to the node as the holder of `token`.
    fn ask(&self, token: &str, message: &Message) -> Message {
        let body = encode(message).unwrap();
        let addr: SocketAddr = self
            .deployment
            .node_url
            .trim_start_matches("http://")
            .parse()
            .unwrap();
        let mut stream = TcpStream::connect(addr).unwrap();
        write!(
            stream,
            "POST /v2/r/{} HTTP/1.1\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n",
            self.deployment.repository,
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).unwrap();
        let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        decode(&reply[split + 4..]).unwrap()
    }

    /// Opens request `id` from a new Layer `name` in `clone`, holding `files`.
    fn propose(&self, token: &str, clone: &Path, name: &str, id: &str, files: &[(&str, &str)]) {
        ok(self.run(token, &["workspace", "create", name, "--repo", s(clone)]));
        let dir = clone.join(".javelin").join("layers").join(name);
        for (path, text) in files {
            fs::write(dir.join(path), text).unwrap();
        }
        let operation = format!("open-{id}");
        let opened = ok(self.run(
            token,
            &[
                "request",
                "open",
                id,
                "--layer",
                name,
                "--operation",
                &operation,
                "--repo",
                s(clone),
            ],
        ));
        assert_eq!(opened["revision"], 1, "{opened}");
    }

    /// `rita` approves revision 1 of `id`, reading through `clone`.
    fn approve(&self, clone: &Path, id: &str) {
        let operation = format!("approve-{id}");
        ok(self.run(
            &self.rita,
            &[
                "request",
                "approve",
                id,
                "--revision",
                "1",
                "--operation",
                &operation,
                "--repo",
                s(clone),
            ],
        ));
    }
}

fn error_of(reply: &Message) -> &str {
    match reply {
        Message::Error { code, .. } => code,
        other => panic!("expected an error, got {other:?}"),
    }
}

#[test]
fn an_outside_contributor_proposes_from_their_own_layer_and_never_reads_a_held_version() {
    let team = Team::start();
    let (maintainer, _) = team.attach("maintainer", &team.mona);

    // A maintainer's fix goes out embargoed, with the embargo in the Publish itself.
    team.propose(
        &team.mona,
        &maintainer,
        "fix",
        "fix-1",
        &[("fix.txt", "patched\n")],
    );
    team.approve(&maintainer, "fix-1");
    let published = ok(team.run(
        &team.mona,
        &[
            "request",
            "publish",
            "fix-1",
            "--revision",
            "1",
            "--operation",
            "publish-fix",
            "--embargo",
            "release",
            "--repo",
            s(&maintainer),
        ],
    ));
    assert_eq!(published["published"]["number"], 1, "{published}");
    assert_eq!(published["embargo"]["until"], Value::Null);
    let disclosure = team.frontier(&team.mona);
    assert_eq!(disclosure["frontier"], 0, "{disclosure}");
    assert_eq!(disclosure["embargoes"][0]["version"], 1);

    // Carol attaches at the frontier and proposes from a Layer of their own.
    let (contributor, attached) = team.attach("contributor", &team.carol);
    assert_eq!(attached["head"]["number"], 0, "{attached}");
    team.propose(
        &team.carol,
        &contributor,
        "work",
        "pr-1",
        &[("hello.txt", "hello\n")],
    );
    let carol = |args: &[&str]| {
        let mut all = args.to_vec();
        all.extend(["--repo", s(&contributor)]);
        team.run(&team.carol, &all)
    };
    // Carol sees only their own request, under their own IDs, and none of anyone else's.
    let listed = ok(carol(&["request", "list"]));
    let requests = listed["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 1, "{listed}");
    assert_eq!(requests[0]["request"], "pr-1");
    assert_eq!(requests[0]["layer"], "work");
    assert_eq!(requests[0]["author"], "carol");
    assert_eq!(refused(carol(&["request", "show", "fix-1"])), "forbidden");
    let shown = ok(carol(&["request", "show", "pr-1"]));
    assert_eq!(shown["state"], "open");
    assert_eq!(shown["changes"]["paths"][0]["path"], "hello.txt", "{shown}");
    // Carol cannot approve or publish, even their own request.
    let approve = [
        "request",
        "approve",
        "pr-1",
        "--revision",
        "1",
        "--operation",
        "carol-approve",
    ];
    assert_eq!(refused(carol(&approve)), "forbidden");

    // Members see carol's request in carol's namespace; one approves, one publishes.
    let listed = ok(team.run(&team.mona, &["request", "list", "--repo", s(&maintainer)]));
    let theirs = listed["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["author"] == "carol")
        .unwrap_or_else(|| panic!("no request by carol: {listed}"));
    let id = theirs["request"].as_str().unwrap().to_string();
    assert!(id.starts_with("outside-") && id.ends_with(".pr-1"), "{id}");
    let shown = ok(team.run(
        &team.mona,
        &["request", "show", &id, "--repo", s(&maintainer)],
    ));
    assert_eq!(shown["changes"]["paths"][0]["path"], "hello.txt", "{shown}");
    team.approve(&maintainer, &id);
    let published = ok(team.run(
        &team.mona,
        &[
            "request",
            "publish",
            &id,
            "--revision",
            "1",
            "--operation",
            "publish-carol",
            "--repo",
            s(&maintainer),
        ],
    ));
    assert_eq!(published["published"]["number"], 2, "{published}");
    assert_eq!(team.deployment.node_head(), 2);

    // Carol learns the request was published, and stays at the frontier.
    assert_eq!(
        ok(carol(&["request", "show", "pr-1"]))["state"],
        "published"
    );
    let synced = ok(carol(&["sync"]));
    assert_eq!(synced["head"]["number"], 0, "{synced}");
    assert_eq!(synced["advanced"], false);
    // On the wire, too: neither held version, nor the fix's content, nor its Layer.
    let head = team.ask(&team.carol, &Message::Resolve { version: None });
    assert!(
        matches!(head, Message::Resolved { number, .. } if number.get() == 0),
        "{head:?}"
    );
    for n in [1, 2] {
        let version = javelin::VersionNumber::new(n).unwrap();
        let reply = team.ask(
            &team.carol,
            &Message::Resolve {
                version: Some(version),
            },
        );
        assert_eq!(error_of(&reply), "missing_version");
        let reply = team.ask(&team.carol, &Message::Changeset { version, offset: 0 });
        assert_eq!(error_of(&reply), "missing_version");
    }
    let fix = javelin::ObjectId::for_blob(b"patched\n");
    let reply = team.ask(&team.carol, &Message::Get(vec![(Kind::Blob, fix)]));
    assert_eq!(error_of(&reply), "missing_object");
    let reply = team.ask(
        &team.carol,
        &Message::LayerHead {
            layer: layer("fix"),
        },
    );
    assert_eq!(error_of(&reply), "forbidden");

    // Released, both versions reach carol, and their own change with them.
    let node = team.deployment.node_url.clone();
    let repository = team.deployment.repository.clone();
    let released = ok(team.run(
        &team.mona,
        &[
            "embargo",
            "release",
            "1",
            "--node",
            &node,
            "--repository",
            &repository,
        ],
    ));
    assert_eq!(released["frontier"], 2, "{released}");
    let synced = ok(carol(&["sync"]));
    assert_eq!(synced["head"]["number"], 2, "{synced}");
    ok(carol(&["workspace", "create", "next"]));
    let next = contributor.join(".javelin/layers/next");
    assert_eq!(
        fs::read_to_string(next.join("hello.txt")).unwrap(),
        "hello\n"
    );
    assert_eq!(
        fs::read_to_string(next.join("fix.txt")).unwrap(),
        "patched\n"
    );
}

#[test]
fn request_publish_with_an_embargo_queues_it_with_the_publish_and_holds_the_version() {
    let team = Team::start();
    let (maintainer, _) = team.attach("maintainer", &team.mona);
    team.propose(
        &team.mona,
        &maintainer,
        "fix",
        "fix-1",
        &[("fix.txt", "patched\n")],
    );
    team.approve(&maintainer, "fix-1");
    let until = (now() + 30 * 86_400).to_string();
    let publish = |embargo: &str| {
        team.run(
            &team.mona,
            &[
                "request",
                "publish",
                "fix-1",
                "--revision",
                "1",
                "--operation",
                "publish-fix",
                "--embargo",
                embargo,
                "--repo",
                s(&maintainer),
            ],
        )
    };
    let invalid = publish("soon");
    assert_eq!(invalid.status.code(), Some(2), "{}", describe(&invalid));

    // The node is down: the Publish queues with its embargo.
    set_node(&maintainer, &closed_url());
    let queued = ok(publish(&until));
    assert_eq!(queued["queued"], true, "{queued}");
    assert_eq!(queued["embargo"]["until"], until.parse::<u64>().unwrap());
    // A retry with another embargo would deliver what was queued, so it is refused.
    assert_eq!(refused(publish("release")), "operation_id_reused");

    // Delivered by sync, the version is held from the first moment it exists.
    set_node(&maintainer, &team.deployment.node_url);
    ok(team.run(&team.mona, &["sync", "--repo", s(&maintainer)]));
    assert_eq!(team.deployment.node_head(), 1);
    let disclosure = team.frontier(&team.mona);
    assert_eq!(disclosure["frontier"], 0, "{disclosure}");
    assert_eq!(disclosure["embargoes"][0]["version"], 1);
    assert_eq!(
        disclosure["embargoes"][0]["until"],
        until.parse::<u64>().unwrap()
    );
    assert_eq!(team.frontier(&team.carol)["frontier"], 0);
    let replayed = ok(publish(&until));
    assert_eq!(replayed["published"]["number"], 1, "{replayed}");
}
