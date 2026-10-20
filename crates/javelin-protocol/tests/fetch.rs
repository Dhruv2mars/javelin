//! Root-down fetch against the reference receiver: full and sparse snapshots,
//! have/want against held chunks, and refusal of inconsistent replies.

mod support;

use std::{collections::BTreeMap, io::pipe, thread};

use javelin::{
    FileEntry, Manifest, ObjectId, OperationId, RepoPath, Repository, VersionNumber,
    tree::{MemoryStore, Store},
};
use javelin_protocol::{
    Message, ProtocolError,
    codec::{read_frame, write_frame},
    fetch,
    fetch::Fetched,
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
    let base = move |message: &Message| match message {
        Message::Hello { .. } => Message::HelloOk { version: 2 },
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
