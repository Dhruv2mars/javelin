//! Root-down fetch against the reference receiver: full and sparse snapshots,
//! have/want against held chunks, and refusal of inconsistent replies.

mod support;

use std::{collections::BTreeMap, io::pipe, thread};

use javelin::{
    FileEntry, Manifest, ObjectId, OperationId, RepoPath, Repository, VersionHasher, VersionNumber,
    genesis_version_id,
    tree::{MemoryStore, Store},
};
use javelin_protocol::{
    Kind, Message, ProtocolError,
    codec::{read_frame, write_frame},
    fetch,
    fetch::{CHAIN_MISMATCH, Fetched},
    limits::{MAX_CHANGESET_PART, MAX_FETCH_PATHS},
    serve,
};
use support::*;

fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}
fn path(s: &str) -> RepoPath {
    RepoPath::parse(s).unwrap()
}

/// Serves `repo` on one thread and fetches from it on this one.
fn run(
    repo: Repository,
    trees: &MemoryStore,
    blobs: &mut BTreeMap<ObjectId, Vec<u8>>,
    version: Option<VersionNumber>,
    prefix: Option<&RepoPath>,
) -> (Result<Fetched, ProtocolError>, Repository) {
    let (client_in, mut server_out) = pipe().unwrap();
    let (mut server_in, client_out) = pipe().unwrap();
    let server = thread::spawn(move || {
        let mut repo = repo;
        let _ = serve(&mut repo, &mut server_in, &mut server_out);
        repo
    });
    let result = fetch(client_in, client_out, trees, blobs, version, prefix);
    (result, server.join().unwrap())
}

/// A published World 1 of 600 files: 300 in one multi-chunk directory and 300 in
/// nested ones. World 2 changes one nested file.
fn published() -> (tempfile::TempDir, Repository, Manifest, Manifest) {
    let (dir, mut repo) = repo();
    let mut entries = BTreeMap::new();
    for n in 0..600u32 {
        let content = repo.put_blob(format!("file {n}\n").as_bytes()).unwrap();
        let p = match n {
            0..300 => format!("big/f{n:04}.rs"),
            _ => format!("pkg{}/mod{}/f{n}.rs", n % 3, n % 5),
        };
        entries.insert(
            path(&p),
            FileEntry::Regular {
                content,
                executable: n % 7 == 0,
            },
        );
    }
    let one = Manifest::new(entries.clone()).unwrap();
    let id = repo.put_manifest(&one).unwrap();
    repo.compare_and_advance(&OperationId::parse("one").unwrap(), v(0), id)
        .unwrap();
    let changed = repo.put_blob(b"changed\n").unwrap();
    entries.insert(
        path("pkg1/mod1/f301.rs"),
        FileEntry::Regular {
            content: changed,
            executable: false,
        },
    );
    let two = Manifest::new(entries).unwrap();
    let id = repo.put_manifest(&two).unwrap();
    repo.compare_and_advance(&OperationId::parse("two").unwrap(), v(1), id)
        .unwrap();
    (dir, repo, one, two)
}

#[test]
fn fetches_a_full_snapshot_root_down_and_reuses_held_chunks() {
    let (_d, repo, one, two) = published();
    let trees = MemoryStore::default();
    let mut blobs = BTreeMap::new();
    let (fetched, repo) = run(repo, &trees, &mut blobs, Some(v(1)), None);
    let fetched = fetched.unwrap();
    assert_eq!(fetched.number, v(1));
    assert_eq!(fetched.tree, repo.version(v(1)).unwrap().tree);
    assert_eq!(&fetched.files, one.entries());
    for entry in one.entries().values() {
        assert_eq!(
            blobs[&entry.content()],
            repo.read_blob(entry.content()).unwrap()
        );
    }
    let first = (fetched.trees_received, fetched.blobs_received);
    assert_eq!(first.1, 600);
    // The head differs in one file: only the chunks on its path and one blob move.
    let (fetched, _) = run(repo, &trees, &mut blobs, None, None);
    let fetched = fetched.unwrap();
    assert_eq!(fetched.number, v(2));
    assert_eq!(&fetched.files, two.entries());
    assert_eq!(fetched.blobs_received, 1);
    assert!(
        fetched.trees_received <= 4,
        "{} of {} chunks refetched",
        fetched.trees_received,
        first.0
    );
}

#[test]
fn sparse_fetch_reads_only_the_path_to_the_prefix_and_below() {
    let (_d, repo, one, _) = published();
    let trees = MemoryStore::default();
    let mut blobs = BTreeMap::new();
    let prefix = path("pkg2/mod3");
    let (fetched, repo) = run(repo, &trees, &mut blobs, Some(v(1)), Some(&prefix));
    let fetched = fetched.unwrap();
    let expected: BTreeMap<_, _> = one
        .entries()
        .iter()
        .filter(|(p, _)| p.as_str().starts_with("pkg2/mod3/"))
        .map(|(p, e)| (p.clone(), e.clone()))
        .collect();
    assert_eq!(fetched.files, expected);
    assert_eq!(fetched.blobs_received, expected.len());
    assert_eq!(blobs.len(), expected.len());
    // Far fewer chunks than the whole tree.
    let full = chunks(&repo, fetched.tree).len();
    assert!(
        fetched.trees_received * 3 < full,
        "{} of {full} chunks",
        fetched.trees_received
    );
    // A file prefix fetches exactly that file, and in a multi-chunk directory
    // only the chunks on the way to it; a missing prefix fetches nothing.
    let file = path("big/f0150.rs");
    let (fetched, repo) = run(
        repo,
        &MemoryStore::default(),
        &mut BTreeMap::new(),
        Some(v(1)),
        Some(&file),
    );
    let fetched = fetched.unwrap();
    assert_eq!(
        fetched.files,
        BTreeMap::from([(file.clone(), one.entries()[&file].clone())])
    );
    let big = chunks(&repo, fetched.tree).len();
    assert!(
        fetched.trees_received <= 4,
        "{} of {big} chunks",
        fetched.trees_received
    );
    let (fetched, _) = run(
        repo,
        &MemoryStore::default(),
        &mut BTreeMap::new(),
        Some(v(1)),
        Some(&path("nope/x")),
    );
    assert!(fetched.unwrap().files.is_empty());
}

type Reply = Box<dyn Fn(&Message) -> Message + Send>;

/// A fake server answering from `reply`.
fn against(reply: impl Fn(&Message) -> Message + Send + 'static) -> Result<Fetched, ProtocolError> {
    let (client_in, mut server_out) = pipe().unwrap();
    let (mut server_in, client_out) = pipe().unwrap();
    thread::spawn(move || {
        while let Ok(Some(message)) = read_frame(&mut server_in) {
            if write_frame(&mut server_out, &reply(&message)).is_err() {
                break;
            }
        }
    });
    fetch(
        client_in,
        client_out,
        &MemoryStore::default(),
        &mut BTreeMap::new(),
        Some(v(1)),
        None,
    )
}

#[test]
fn refuses_replies_that_do_not_answer_the_request() {
    let (_d, repo, _, _) = published();
    let root = repo.version(v(1)).unwrap().tree;
    let real = repo
        .tree_bytes_bounded(root, javelin_protocol::limits::MAX_TREE)
        .unwrap();
    let id = repo.version(v(1)).unwrap().id;
    // The honest changeset, so the chain holds and the cases below reach their targets.
    let changes: &'static [u8] = Box::leak(
        repo.changeset(v(1))
            .unwrap()
            .canonical_bytes()
            .into_boxed_slice(),
    );
    let base = move |message: &Message| match message {
        Message::Hello { .. } => Message::HelloOk { version: 2 },
        Message::Changeset { offset, .. } => Message::ChangesetPart {
            total: changes.len() as u64,
            bytes: changes[*offset as usize..].to_vec(),
        },
        _ => Message::Resolved {
            number: v(1),
            id,
            tree: root,
        },
    };
    // The wrong version, bytes that do not match the ID, objects not asked for,
    // and an empty reply that would never finish.
    let cases: Vec<Reply> = vec![
        Box::new(move |m| match m {
            Message::Resolve { .. } => Message::Resolved {
                number: v(2),
                id,
                tree: root,
            },
            m => base(m),
        }),
        Box::new(move |m| match m {
            Message::Get(_) => Message::Objects(vec![(
                javelin_protocol::Kind::Tree,
                root,
                javelin::tree::Chunk::decode(&[0]).unwrap().encode(),
            )]),
            m => base(m),
        }),
        Box::new(move |m| match m {
            Message::Get(_) => Message::Objects(vec![(
                javelin_protocol::Kind::Blob,
                ObjectId::for_blob(b"x"),
                b"x".to_vec(),
            )]),
            m => base(m),
        }),
        Box::new(move |m| match m {
            Message::Get(_) => Message::Objects(vec![]),
            m => base(m),
        }),
    ];
    let codes: Vec<_> = cases
        .into_iter()
        .map(|reply| against(reply).unwrap_err().code)
        .collect();
    assert_eq!(
        codes,
        [
            "unexpected_message",
            "id_mismatch",
            "unexpected_message",
            "unexpected_message"
        ]
    );
    // Sanity: the real root bytes are what the client would accept.
    assert_eq!(ObjectId::for_tree(&real), root);
    let store = MemoryStore::default();
    store.save(root, &real).unwrap();
    assert_eq!(store.load(root).unwrap(), real);
}

#[test]
fn refuses_a_child_filed_under_a_name_its_last_name_does_not_match() {
    // The parent files an empty leaf (last name "") under "a".
    let leaf = javelin::tree::Chunk::decode(&[0]).unwrap().encode();
    let leaf_id = javelin_protocol::Kind::Tree.id(&leaf);
    let mut parent = vec![1, 0, 0, 1, b'a'];
    parent.extend_from_slice(leaf_id.as_bytes());
    let root = javelin_protocol::Kind::Tree.id(&parent);
    // The version ID the chain gives this root with an empty changeset.
    let id = VersionHasher::new(Some(genesis_version_id()), root).finish();
    let result = against(move |message| match message {
        Message::Hello { .. } => Message::HelloOk { version: 2 },
        Message::Resolve { .. } => Message::Resolved {
            number: v(1),
            id,
            tree: root,
        },
        Message::Changeset { .. } => Message::ChangesetPart {
            total: 0,
            bytes: Vec::new(),
        },
        Message::Get(request) => Message::Objects(
            request
                .iter()
                .filter_map(|(kind, id)| {
                    if *id == root {
                        Some((*kind, root, parent.clone()))
                    } else if *id == leaf_id {
                        Some((*kind, leaf_id, leaf.clone()))
                    } else {
                        None
                    }
                })
                .collect(),
        ),
        _ => Message::Objects(Vec::new()),
    });
    assert_eq!(result.unwrap_err().code, "malformed");
}

/// Serves `repo` through a proxy that rewrites each reply with `lie`, which sees the
/// request it answers. The server behind the proxy is honest.
fn run_lying(
    repo: Repository,
    trees: &MemoryStore,
    blobs: &mut BTreeMap<ObjectId, Vec<u8>>,
    lie: impl Fn(&Message, Message) -> Message + Send + 'static,
) -> Result<Fetched, ProtocolError> {
    let (mut proxy_in, client_out) = pipe().unwrap();
    let (client_in, mut proxy_out) = pipe().unwrap();
    let (server_in, mut to_server) = pipe().unwrap();
    let (mut from_server, server_out) = pipe().unwrap();
    thread::spawn(move || {
        let mut repo = repo;
        let _ = serve(&mut repo, server_in, server_out);
    });
    thread::spawn(move || {
        while let Ok(Some(request)) = read_frame(&mut proxy_in) {
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
    fetch(client_in, client_out, trees, blobs, None, None)
}

#[test]
fn a_proxy_that_lies_about_nothing_fetches_the_head() {
    let (_d, repo, _, two) = published();
    let fetched = run_lying(
        repo,
        &MemoryStore::default(),
        &mut BTreeMap::new(),
        |_, r| r,
    );
    assert_eq!(&fetched.unwrap().files, two.entries());
}

#[test]
fn refuses_a_head_that_names_an_older_versions_root() {
    // A rollback: the head's ID and changeset are real, but its root is World 1's.
    let (_d, repo, _, _) = published();
    let older = repo.version(v(1)).unwrap().tree;
    let trees = MemoryStore::default();
    let err = run_lying(
        repo,
        &trees,
        &mut BTreeMap::new(),
        move |request, reply| match (request, reply) {
            (Message::Resolve { version: None }, Message::Resolved { number, id, .. }) => {
                Message::Resolved {
                    number,
                    id,
                    tree: older,
                }
            }
            (_, reply) => reply,
        },
    )
    .unwrap_err();
    assert_eq!(err.code, CHAIN_MISMATCH);
    assert_eq!(trees.len(), 0, "nothing from the lie is stored");
}

#[test]
fn refuses_a_fabricated_root_before_reading_any_chunk() {
    let (_d, repo, _, _) = published();
    let fabricated = ObjectId::for_tree(b"not in any World");
    let trees = MemoryStore::default();
    let err = run_lying(
        repo,
        &trees,
        &mut BTreeMap::new(),
        move |request, reply| match (request, reply) {
            (Message::Resolve { version: None }, Message::Resolved { number, id, .. }) => {
                Message::Resolved {
                    number,
                    id,
                    tree: fabricated,
                }
            }
            (_, reply) => reply,
        },
    )
    .unwrap_err();
    assert_eq!(err.code, CHAIN_MISMATCH);
    assert_eq!(trees.len(), 0, "nothing from the lie is stored");
}

#[test]
fn refuses_a_changeset_that_does_not_hash_to_its_version() {
    let (_d, repo, _, _) = published();
    let err = run_lying(
        repo,
        &MemoryStore::default(),
        &mut BTreeMap::new(),
        |request, reply| match (request, reply) {
            (Message::Changeset { version, .. }, Message::ChangesetPart { total, mut bytes })
                if *version == v(1) && !bytes.is_empty() =>
            {
                bytes[0] ^= 1;
                Message::ChangesetPart { total, bytes }
            }
            (_, reply) => reply,
        },
    )
    .unwrap_err();
    assert_eq!(err.code, CHAIN_MISMATCH);
}

/// An honest server for World 1 over the empty root, whose changeset is `changes`.
/// `flip` corrupts the byte at that offset as it is served.
fn fake_head(changes: Vec<u8>, flip: Option<usize>) -> Result<Fetched, ProtocolError> {
    let root = javelin::tree::empty();
    let mut hash = VersionHasher::new(Some(genesis_version_id()), root);
    hash.update(&changes);
    let id = hash.finish();
    let total = changes.len();
    against(move |message| match message {
        Message::Hello { .. } => Message::HelloOk { version: 2 },
        Message::Resolve { .. } => Message::Resolved {
            number: v(1),
            id,
            tree: root,
        },
        Message::Changeset { offset, .. } => {
            let start = *offset as usize;
            let end = total.min(start + MAX_CHANGESET_PART);
            let mut bytes = changes[start..end].to_vec();
            if let Some(i) = flip.filter(|i| (start..end).contains(i)) {
                bytes[i - start] ^= 1;
            }
            Message::ChangesetPart {
                total: total as u64,
                bytes,
            }
        }
        Message::Get(request) => Message::Objects(
            request
                .iter()
                .map(|(kind, id)| (*kind, *id, vec![0]))
                .collect(),
        ),
        _ => Message::Objects(Vec::new()),
    })
}

#[test]
fn streams_a_changeset_in_parts_and_hashes_all_of_it() {
    let changes: Vec<u8> = (0..MAX_CHANGESET_PART + 1000)
        .map(|i| (i % 251) as u8)
        .collect();
    let fetched = fake_head(changes.clone(), None).unwrap();
    assert!(fetched.files.is_empty());
    // A byte flipped in the second part breaks the chain.
    let err = fake_head(changes, Some(MAX_CHANGESET_PART + 10)).unwrap_err();
    assert_eq!(err.code, CHAIN_MISMATCH);
}

/// Bytes of a level-0 chunk: each item is a tag, its name's length, its name and its ID.
fn leaf_chunk(items: &[(u8, String, ObjectId)]) -> Vec<u8> {
    let mut bytes = vec![0];
    for (tag, name, id) in items {
        bytes.push(*tag);
        bytes.extend_from_slice(&(name.len() as u16).to_be_bytes());
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(id.as_bytes());
    }
    bytes
}

#[test]
fn refuses_a_tree_that_names_more_paths_than_the_fetch_bound() {
    // Each directory names the one below it 256 times, from a few KB of chunks. The
    // chain is honest, so only the bound stops the walk: 256^3 files, past MAX_FETCH_PATHS.
    const FANOUT: usize = 256;
    const FILE: u8 = 1;
    const DIR: u8 = 4;
    assert!(FANOUT.pow(3) > MAX_FETCH_PATHS);
    let blob = ObjectId::for_blob(b"shared");
    let files: Vec<_> = (0..FANOUT)
        .map(|i| (FILE, format!("f{i:03}"), blob))
        .collect();
    let mut chunks = Vec::new();
    let mut dir = Kind::Tree.id(&leaf_chunk(&files));
    chunks.push((dir, leaf_chunk(&files)));
    for _ in 0..2 {
        let subdirs: Vec<_> = (0..FANOUT)
            .map(|i| (DIR, format!("d{i:03}"), dir))
            .collect();
        let bytes = leaf_chunk(&subdirs);
        dir = Kind::Tree.id(&bytes);
        chunks.push((dir, bytes));
    }
    let root = dir;
    let id = VersionHasher::new(Some(genesis_version_id()), root).finish();
    let err = against(move |message| match message {
        Message::Hello { .. } => Message::HelloOk { version: 2 },
        Message::Resolve { .. } => Message::Resolved {
            number: v(1),
            id,
            tree: root,
        },
        Message::Changeset { .. } => Message::ChangesetPart {
            total: 0,
            bytes: Vec::new(),
        },
        Message::Get(request) => Message::Objects(
            request
                .iter()
                .filter_map(|(kind, want)| {
                    let (_, bytes) = chunks.iter().find(|(id, _)| id == want)?;
                    Some((*kind, *want, bytes.clone()))
                })
                .collect(),
        ),
        _ => Message::Objects(Vec::new()),
    })
    .unwrap_err();
    assert_eq!(err.code, "limit_exceeded");
}
