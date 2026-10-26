//! Trust anchors: a fetch given a version the caller verified earlier checks only the
//! versions between that anchor and its target, still refuses a server that lies about
//! any of them or about the anchor itself, and costs the same however long the history
//! before the anchor is.

mod support;

use std::{
    collections::BTreeMap,
    io::pipe,
    sync::{Arc, Mutex},
    thread,
    time::Instant,
};

use javelin::{
    FileEntry, Manifest, ObjectId, OperationId, RepoPath, Repository, VersionHasher, VersionNumber,
    genesis_version_id,
    tree::{MemoryStore, empty},
};
use javelin_protocol::{
    Anchor, Kind, Message, ProtocolError,
    codec::{read_frame, write_frame},
    fetch,
    fetch::{CHAIN_MISMATCH, Fetched},
    serve,
};
use support::*;

fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}

/// A repository with Worlds 1 to `n`, each adding the file `fK` holding `tag K`.
fn worlds(n: u64, tag: &str) -> (tempfile::TempDir, Repository) {
    let (dir, mut repo) = repo();
    let mut entries = BTreeMap::new();
    for k in 1..=n {
        let content = repo.put_blob(format!("{tag} {k}\n").as_bytes()).unwrap();
        entries.insert(
            RepoPath::parse(&format!("f{k}")).unwrap(),
            FileEntry::Regular {
                content,
                executable: false,
            },
        );
        let id = repo
            .put_manifest(&Manifest::new(entries.clone()).unwrap())
            .unwrap();
        repo.compare_and_advance(&OperationId::parse(&format!("w{k}")).unwrap(), v(k - 1), id)
            .unwrap();
    }
    (dir, repo)
}

fn anchor_of(repo: &Repository, n: u64) -> Anchor {
    let version = repo.version(v(n)).unwrap();
    Anchor {
        number: version.number,
        id: version.id,
        tree: version.tree,
    }
}

/// What a fetch asked for: each request, in order.
type Log = Arc<Mutex<Vec<Message>>>;

/// Serves `repo` through a proxy that logs each request and rewrites each reply with
/// `lie`. The server behind the proxy is honest.
fn through(
    repo: Repository,
    trees: &MemoryStore,
    anchor: Option<&Anchor>,
    version: Option<VersionNumber>,
    lie: impl Fn(&Message, Message) -> Message + Send + 'static,
) -> (Result<Fetched, ProtocolError>, Vec<Message>) {
    let (mut proxy_in, client_out) = pipe().unwrap();
    let (client_in, mut proxy_out) = pipe().unwrap();
    let (server_in, mut to_server) = pipe().unwrap();
    let (mut from_server, server_out) = pipe().unwrap();
    let log = Log::default();
    let requests = log.clone();
    thread::spawn(move || {
        let mut repo = repo;
        let _ = serve(&mut repo, server_in, server_out);
    });
    thread::spawn(move || {
        while let Ok(Some(request)) = read_frame(&mut proxy_in) {
            requests.lock().unwrap().push(request.clone());
            if write_frame(&mut to_server, &request).is_err() {
                break;
            }
            let Ok(Some(reply)) = read_frame(&mut from_server) else {
                break;
            };
            if write_frame(&mut proxy_out, &lie(&request, reply)).is_err() {
                break;
            }
        }
    });
    let result = fetch(
        client_in,
        client_out,
        trees,
        &mut BTreeMap::new(),
        anchor,
        version,
        None,
    );
    let log = log.lock().unwrap().clone();
    (result, log)
}

fn honest(_: &Message, reply: Message) -> Message {
    reply
}

/// The versions whose changesets a fetch hashed.
fn linked(log: &[Message]) -> Vec<u64> {
    let mut versions: Vec<u64> = log
        .iter()
        .filter_map(|m| match m {
            Message::Changeset { version, .. } => Some(version.get()),
            _ => None,
        })
        .collect();
    versions.dedup();
    versions
}

/// Replaces the root a `Resolved` reply names for `version` (the head's reply when None).
fn root_lie(
    version: Option<VersionNumber>,
    tree: ObjectId,
) -> impl Fn(&Message, Message) -> Message + Send + 'static {
    move |request, reply| match (request, reply) {
        (Message::Resolve { version: asked }, Message::Resolved { number, id, .. })
            if *asked == version =>
        {
            Message::Resolved { number, id, tree }
        }
        (_, reply) => reply,
    }
}

#[test]
fn a_fetch_after_the_anchor_hashes_only_the_new_versions() {
    let (_d, repo) = worlds(4, "a");
    let expected = repo.manifest_at(v(4)).unwrap();
    // The client verifies World 2 from World 0 and keeps it as its anchor.
    let (first, log) = through(repo, &MemoryStore::default(), None, Some(v(2)), honest);
    let anchor = first.unwrap().anchor();
    assert_eq!(linked(&log), [1, 2]);
    let (_d, repo) = worlds(4, "a");
    assert_eq!(anchor, anchor_of(&repo, 2));

    let (fetched, log) = through(repo, &MemoryStore::default(), Some(&anchor), None, honest);
    let fetched = fetched.unwrap();
    assert_eq!(fetched.number, v(4));
    assert_eq!(&fetched.files, expected.entries());
    assert_eq!(linked(&log), [3, 4], "only the versions after the anchor");

    // At the anchor itself, nothing is hashed.
    let (_d, repo) = worlds(4, "a");
    let (fetched, log) = through(repo, &MemoryStore::default(), Some(&anchor), Some(v(2)), honest);
    assert_eq!(fetched.unwrap().anchor(), anchor);
    assert!(linked(&log).is_empty());
}

#[test]
fn refuses_a_server_that_lies_about_a_version_after_the_anchor() {
    let (_d, repo) = worlds(4, "a");
    let anchor = anchor_of(&repo, 2);
    let older = repo.version(v(1)).unwrap().tree;
    let flip = |request: &Message, reply| match (request, reply) {
        (Message::Changeset { version, .. }, Message::ChangesetPart { total, mut bytes })
            if version.get() == 3 && !bytes.is_empty() =>
        {
            bytes[0] ^= 1;
            Message::ChangesetPart { total, bytes }
        }
        (_, reply) => reply,
    };
    let lies: Vec<Box<dyn Fn(&Message, Message) -> Message + Send>> = vec![
        Box::new(flip),
        Box::new(root_lie(None, older)),
        Box::new(root_lie(Some(v(3)), older)),
    ];
    for lie in lies {
        let (_d, repo) = worlds(4, "a");
        let trees = MemoryStore::default();
        let (result, _) = through(repo, &trees, Some(&anchor), None, lie);
        assert_eq!(result.unwrap_err().code, CHAIN_MISMATCH);
        assert_eq!(trees.len(), 0, "nothing from the lie is stored");
    }
}

#[test]
fn refuses_a_server_whose_chain_does_not_pass_through_the_anchor() {
    // The anchor is World 2 of another history with the same shape.
    let (_d, other) = worlds(4, "b");
    let foreign = anchor_of(&other, 2);
    for version in [None, Some(v(3)), Some(v(2)), Some(v(1))] {
        let (_d, repo) = worlds(4, "a");
        let (result, _) = through(repo, &MemoryStore::default(), Some(&foreign), version, honest);
        assert_eq!(result.unwrap_err().code, CHAIN_MISMATCH, "{version:?}");
    }
    // The server names another root, or another ID, for the anchor itself.
    let (_d, repo) = worlds(4, "a");
    let anchor = anchor_of(&repo, 2);
    let older = repo.version(v(1)).unwrap().tree;
    let (result, _) = through(
        repo,
        &MemoryStore::default(),
        Some(&anchor),
        Some(v(2)),
        root_lie(Some(v(2)), older),
    );
    assert_eq!(result.unwrap_err().code, CHAIN_MISMATCH);
    let (_d, repo) = worlds(4, "a");
    let other_id = repo.version(v(1)).unwrap().id;
    let (result, _) = through(
        repo,
        &MemoryStore::default(),
        Some(&anchor),
        Some(v(2)),
        move |_, reply| match reply {
            Message::Resolved { number, tree, .. } => Message::Resolved {
                number,
                id: other_id,
                tree,
            },
            reply => reply,
        },
    );
    assert_eq!(result.unwrap_err().code, CHAIN_MISMATCH);
}

#[test]
fn refuses_a_head_behind_the_anchor() {
    // A rollback: the server's real World 2 offered as the head to a client anchored at 4.
    let (_d, repo) = worlds(4, "a");
    let anchor = anchor_of(&repo, 4);
    let two = repo.version(v(2)).unwrap();
    let (result, log) = through(
        repo,
        &MemoryStore::default(),
        Some(&anchor),
        None,
        move |request, reply| match request {
            Message::Resolve { version: None } => Message::Resolved {
                number: two.number,
                id: two.id,
                tree: two.tree,
            },
            _ => reply,
        },
    );
    assert_eq!(result.unwrap_err().code, CHAIN_MISMATCH);
    assert!(linked(&log).is_empty());
}

#[test]
fn verifies_an_older_version_down_from_the_anchor() {
    let (_d, repo) = worlds(4, "a");
    let anchor = anchor_of(&repo, 4);
    let expected = repo.manifest_at(v(2)).unwrap();
    let (fetched, log) = through(repo, &MemoryStore::default(), Some(&anchor), Some(v(2)), honest);
    assert_eq!(&fetched.unwrap().files, expected.entries());
    // Down from the anchor, plus one link for World 2's own root; World 1 is not hashed.
    assert_eq!(linked(&log), [4, 3, 2]);
    let (_d, repo) = worlds(4, "a");
    let (fetched, log) = through(repo, &MemoryStore::default(), Some(&anchor), Some(v(0)), honest);
    let fetched = fetched.unwrap();
    assert!(fetched.files.is_empty());
    assert_eq!(fetched.tree, empty());
    assert_eq!(linked(&log), [4, 3, 2, 1]);

    // A lie about the target's root, or about a root between it and the anchor.
    for lied in [2, 3] {
        let (_d, repo) = worlds(4, "a");
        let older = repo.version(v(1)).unwrap().tree;
        let trees = MemoryStore::default();
        let (result, _) = through(
            repo,
            &trees,
            Some(&anchor),
            Some(v(2)),
            root_lie(Some(v(lied)), older),
        );
        assert_eq!(result.unwrap_err().code, CHAIN_MISMATCH, "World {lied}");
        assert_eq!(trees.len(), 0);
    }
}

/// An honest in-memory chain of `length` versions after World 0. Every version after
/// World 0 has the same one-file root and a small changeset of its own.
struct Chain {
    versions: Vec<(ObjectId, ObjectId, Vec<u8>)>,
    objects: BTreeMap<ObjectId, (Kind, Vec<u8>)>,
}

impl Chain {
    fn new(length: u64) -> Self {
        let blob = b"hello\n".to_vec();
        let blob_id = Kind::Blob.id(&blob);
        let mut leaf = vec![0, 1, 0, 1, b'f'];
        leaf.extend_from_slice(blob_id.as_bytes());
        let root = Kind::Tree.id(&leaf);
        let mut versions = vec![(genesis_version_id(), empty(), b"{}".to_vec())];
        for n in 1..=length {
            let changes = format!("{{\"f\":{n}}}").into_bytes();
            let mut hash = VersionHasher::new(Some(versions.last().unwrap().0), root);
            hash.update(&changes);
            versions.push((hash.finish(), root, changes));
        }
        let objects = BTreeMap::from([(blob_id, (Kind::Blob, blob)), (root, (Kind::Tree, leaf))]);
        Self { versions, objects }
    }

    fn anchor(&self, n: u64) -> Anchor {
        let (id, tree, _) = self.versions[n as usize];
        Anchor {
            number: v(n),
            id,
            tree,
        }
    }

    fn reply(&self, message: &Message) -> Message {
        let head = self.versions.len() as u64 - 1;
        match message {
            Message::Hello { .. } => Message::HelloOk { version: 2 },
            Message::Resolve { version } => {
                let number = version.map_or(head, |v| v.get());
                let (id, tree, _) = self.versions[number as usize];
                Message::Resolved {
                    number: v(number),
                    id,
                    tree,
                }
            }
            Message::Changeset { version, offset } => {
                let changes = &self.versions[version.get() as usize].2;
                Message::ChangesetPart {
                    total: changes.len() as u64,
                    bytes: changes[*offset as usize..].to_vec(),
                }
            }
            Message::Get(wanted) => Message::Objects(
                wanted
                    .iter()
                    .map(|(_, id)| {
                        let (kind, bytes) = &self.objects[id];
                        (*kind, *id, bytes.clone())
                    })
                    .collect(),
            ),
            _ => Message::Objects(Vec::new()),
        }
    }
}

/// Fetches from `chain` and counts the requests the fetch made.
fn count(
    chain: &Arc<Chain>,
    anchor: Option<&Anchor>,
    version: Option<VersionNumber>,
) -> (Fetched, usize) {
    let (client_in, mut server_out) = pipe().unwrap();
    let (mut server_in, client_out) = pipe().unwrap();
    let served = chain.clone();
    let server = thread::spawn(move || {
        let mut requests = 0;
        while let Ok(Some(message)) = read_frame(&mut server_in) {
            requests += 1;
            if write_frame(&mut server_out, &served.reply(&message)).is_err() {
                break;
            }
        }
        requests
    });
    let fetched = fetch(
        client_in,
        client_out,
        &MemoryStore::default(),
        &mut BTreeMap::new(),
        anchor,
        version,
        None,
    )
    .unwrap();
    (fetched, server.join().unwrap())
}

#[test]
fn a_fetch_after_the_anchor_costs_the_same_at_any_history_length() {
    let mut anchored = Vec::new();
    let mut unanchored = Vec::new();
    for length in [10u64, 1_000, 10_000] {
        let chain = Arc::new(Chain::new(length));
        // The client verified the version before the head earlier; one version is new.
        let anchor = chain.anchor(length - 1);
        let start = Instant::now();
        let (fetched, requests) = count(&chain, Some(&anchor), None);
        let elapsed = start.elapsed();
        assert_eq!(fetched.anchor(), chain.anchor(length));
        assert_eq!(fetched.files.len(), 1);
        let start = Instant::now();
        let (_, full) = count(&chain, None, None);
        eprintln!(
            "history {length:>6}: after the anchor {requests} requests in {elapsed:?}; \
             from World 0 {full} requests in {:?}",
            start.elapsed()
        );
        anchored.push(requests);
        unanchored.push(full);
    }
    // Hello, Resolve, one Changeset, the root chunk and the blob, at every length.
    assert_eq!(anchored, [5, 5, 5]);
    // From World 0 every version costs a Resolve and a Changeset.
    assert_eq!(unanchored, [5 + 2 * 9, 5 + 2 * 999, 5 + 2 * 9_999]);
}
