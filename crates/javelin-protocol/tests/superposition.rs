//! Hidden entries on the wire: every reader fetches and verifies the same version IDs,
//! hidden entries reach an outsider as commitments only, and no read message gives an
//! outsider hidden bytes, salts or plain IDs.

use std::{collections::BTreeMap, io::pipe, thread};

use javelin::{
    Audience, FileEntry, Manifest, ObjectId, OperationId, POLICY_PATH, Reader, RepoPath,
    Repository, VersionNumber, tree::MemoryStore,
};
use javelin_protocol::{
    Kind, Message, ProtocolError,
    codec::{VERSION, read_frame, write_frame},
    fetch::{Fetched, fetch, fetch_opened},
    serve_as,
};
use tempfile::TempDir;

const TOKEN: &[u8] = b"TOKEN=hunter2\n";
const NOTES: &[u8] = b"launch on friday\n";

fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}
fn path(s: &str) -> RepoPath {
    RepoPath::parse(s).unwrap()
}

/// A node at World 1: a policy, hidden files and directories, a nested narrower
/// audience, and an empty file that is hidden at one path and public at another.
fn node() -> (TempDir, Repository) {
    let dir = TempDir::new().unwrap();
    let mut repo = Repository::init(dir.path()).unwrap();
    let files: [(&str, &[u8]); 8] = [
        (
            POLICY_PATH,
            b".env* maintainers\nconfig/prod/** maintainers\ninternal/** members\ninternal/keys/** maintainers\n",
        ),
        (".env", TOKEN),
        ("config/prod/db.yml", b"password: swordfish\n"),
        ("config/prod/empty", b""),
        ("internal/notes", NOTES),
        ("internal/keys/k", b"-----BEGIN KEY-----\n"),
        ("src/main.rs", b"fn main() {}\n"),
        ("src/empty", b""),
    ];
    let mut entries = BTreeMap::new();
    for (p, bytes) in files {
        let content = repo.put_blob(bytes).unwrap();
        entries.insert(
            path(p),
            FileEntry::Regular {
                content,
                executable: false,
            },
        );
    }
    let manifest = repo.put_manifest(&Manifest::new(entries).unwrap()).unwrap();
    repo.compare_and_advance(&OperationId::parse("one").unwrap(), v(0), manifest)
        .unwrap();
    (dir, repo)
}

/// Fetches the head from `repo` served to `reader`, opening hidden entries when `open`.
fn fetch_as(
    repo: Repository,
    reader: Reader,
    open: bool,
) -> (Fetched, BTreeMap<ObjectId, Vec<u8>>, Repository) {
    let (client_in, mut server_out) = pipe().unwrap();
    let (mut server_in, client_out) = pipe().unwrap();
    let server = thread::spawn(move || {
        let mut repo = repo;
        let _ = serve_as(&mut repo, &mut server_in, &mut server_out, &reader);
        repo
    });
    let trees = MemoryStore::default();
    let mut blobs = BTreeMap::new();
    let fetched = if open {
        fetch_opened(client_in, client_out, &trees, &mut blobs, None, None)
    } else {
        fetch(client_in, client_out, &trees, &mut blobs, None, None)
    }
    .unwrap();
    (fetched, blobs, server.join().unwrap())
}

/// Sends raw messages to `repo` served to `reader` and returns each reply.
fn session(
    repo: Repository,
    reader: Reader,
    messages: Vec<Message>,
) -> (Vec<Result<Message, ProtocolError>>, Repository) {
    let (mut client_in, mut server_out) = pipe().unwrap();
    let (mut server_in, mut client_out) = pipe().unwrap();
    let server = thread::spawn(move || {
        let mut repo = repo;
        let _ = serve_as(&mut repo, &mut server_in, &mut server_out, &reader);
        repo
    });
    let mut call = |message: &Message| {
        write_frame(&mut client_out, message).unwrap();
        match read_frame(&mut client_in).unwrap().unwrap() {
            Message::Error { code, detail } => Err(ProtocolError { code, detail }),
            reply => Ok(reply),
        }
    };
    call(&Message::Hello {
        min: VERSION,
        max: VERSION,
    })
    .unwrap();
    let mut replies = Vec::new();
    for message in &messages {
        let reply = call(message);
        let failed = reply.is_err();
        replies.push(reply);
        // A refusal ends the session; the next message needs a new one.
        if failed {
            break;
        }
    }
    drop(client_out);
    (replies, server.join().unwrap())
}

fn keys(map: &BTreeMap<RepoPath, impl Sized>) -> Vec<&str> {
    map.keys().map(RepoPath::as_str).collect()
}

#[test]
fn every_reader_verifies_the_same_ids_and_sees_its_audiences() {
    let (_dir, repo) = node();
    let head = repo.head().unwrap();
    let (outsider, blobs, repo) = fetch_as(repo, Reader::outsider(), true);
    let (member, _, repo) = fetch_as(repo, Reader::member([]), true);
    let (maintainer, maintainer_blobs, _repo) = fetch_as(repo, Reader::maintainer(), true);
    for fetched in [&outsider, &member, &maintainer] {
        assert_eq!(
            (fetched.number, fetched.id, fetched.tree),
            (head.number, head.id, head.tree)
        );
    }

    // The outsider gets public files only; each hidden entry stays a commitment and
    // its materialized base leaves it out.
    assert_eq!(
        keys(&outsider.files),
        [POLICY_PATH, "src/empty", "src/main.rs"]
    );
    assert_eq!(keys(&outsider.hidden), [".env", "config/prod", "internal"]);
    assert_eq!(outsider.hidden[&path(".env")].1, Audience::Maintainers);
    assert!(outsider.openings.is_empty());
    assert!(!blobs.contains_key(&ObjectId::for_blob(TOKEN)));

    // A member opens `internal` but not the maintainers' entries, nested or not.
    assert_eq!(
        keys(&member.files),
        [POLICY_PATH, "internal/notes", "src/empty", "src/main.rs"]
    );
    assert_eq!(
        keys(&member.hidden),
        [".env", "config/prod", "internal/keys"]
    );

    // A maintainer opens everything, and each opening matched its commitment.
    assert!(maintainer.hidden.is_empty());
    assert_eq!(
        keys(&maintainer.files),
        [
            ".env",
            POLICY_PATH,
            "config/prod/db.yml",
            "config/prod/empty",
            "internal/keys/k",
            "internal/notes",
            "src/empty",
            "src/main.rs"
        ]
    );
    assert_eq!(maintainer_blobs[&ObjectId::for_blob(TOKEN)], TOKEN);
    for (commitment, (opening, _)) in &maintainer.openings {
        assert_eq!(opening.commitment(), *commitment);
    }

    // Without asking to open, even a maintainer's fetch stays committed.
    let (_dir, repo) = node();
    let (closed, _, _) = fetch_as(repo, Reader::maintainer(), false);
    assert_eq!(keys(&closed.hidden), [".env", "config/prod", "internal"]);
}

#[test]
fn no_read_message_gives_an_outsider_hidden_bytes_salts_or_ids() {
    let (_dir, repo) = node();
    let committed = repo.committed_at(v(1)).unwrap();
    let commitment = |p: &str| match &committed.entries()[&path(p)] {
        FileEntry::Hidden { commitment, .. } => *commitment,
        other => panic!("{other:?}"),
    };
    let (env, prod, internal) = (
        commitment(".env"),
        commitment("config/prod"),
        commitment("internal"),
    );
    let all = repo
        .openings(&[env, prod, internal], &Reader::maintainer())
        .unwrap();
    let secret = ObjectId::for_blob(TOKEN);
    let notes = ObjectId::for_blob(NOTES);
    let prod_root = all[1].1.content;
    let view = repo.manifest_at(v(1)).unwrap();
    assert_eq!(view.entries()[&path(".env")].content(), Some(secret));
    // The view's own root chunk names plain IDs; it is never served.
    let view_root = javelin::tree::Trees::new(MemoryStore::default())
        .build(view.entries())
        .unwrap();

    // `Open` returns nothing to an outsider.
    let (replies, repo) = session(
        repo,
        Reader::outsider(),
        vec![Message::Open(vec![env, prod, internal])],
    );
    assert_eq!(replies[0], Ok(Message::Opened(Vec::new())));

    // `Want` reports hidden objects missing, as if never stored, and public ones held.
    let probe = vec![
        (Kind::Blob, secret),
        (Kind::Blob, notes),
        (Kind::Tree, prod_root),
        (Kind::Blob, ObjectId::for_blob(b"")),
    ];
    let (replies, repo) = session(repo, Reader::outsider(), vec![Message::Want(probe.clone())]);
    assert_eq!(replies[0], Ok(Message::Missing(probe[..3].to_vec())));

    // `Get` refuses each hidden object exactly as it refuses one that does not exist.
    let absent = ObjectId::for_blob(b"never stored");
    let (replies, mut repo) = session(
        repo,
        Reader::outsider(),
        vec![Message::Get(vec![(Kind::Blob, absent)])],
    );
    let not_stored = replies[0].clone().unwrap_err();
    for (kind, id) in [
        (Kind::Blob, secret),
        (Kind::Blob, notes),
        (Kind::Tree, prod_root),
        (Kind::Tree, view_root),
    ] {
        let (replies, back) = session(
            repo,
            Reader::outsider(),
            vec![Message::Get(vec![(kind, id)])],
        );
        repo = back;
        let error = replies[0].clone().unwrap_err();
        assert_eq!(error.code, not_stored.code);
        assert_eq!(
            error.detail,
            not_stored
                .detail
                .replace(&absent.to_string(), &id.to_string())
        );
    }

    // `Resolve` and `Changeset` carry the committed form: no plain ID or hidden name.
    let (replies, repo) = session(
        repo,
        Reader::outsider(),
        vec![
            Message::Resolve { version: None },
            Message::Changeset {
                version: v(1),
                offset: 0,
            },
        ],
    );
    let Ok(Message::ChangesetPart { bytes, .. }) = &replies[1] else {
        panic!("{replies:?}")
    };
    for needle in [
        secret.to_string(),
        notes.to_string(),
        "db.yml".into(),
        "notes".into(),
    ] {
        assert!(
            !String::from_utf8_lossy(bytes).contains(&needle),
            "{needle}"
        );
    }
    let Ok(Message::Resolved { tree, .. }) = replies[0] else {
        panic!("{replies:?}")
    };
    assert_ne!(tree, view_root);

    // The same requests succeed for the audience, so the IDs above were the real ones.
    let (replies, repo) = session(
        repo,
        Reader::maintainer(),
        vec![
            Message::Open(vec![env]),
            Message::Get(vec![(Kind::Blob, secret), (Kind::Tree, prod_root)]),
        ],
    );
    assert!(
        matches!(&replies[0], Ok(Message::Opened(o)) if o.len() == 1 && o[0].1.content == secret)
    );
    assert!(matches!(&replies[1], Ok(Message::Objects(o)) if o.len() == 2));
    let (replies, _) = session(
        repo,
        Reader::member([]),
        vec![Message::Open(vec![env, internal])],
    );
    assert!(matches!(&replies[0], Ok(Message::Opened(o)) if o.len() == 1 && o[0].0 == internal));
}

#[test]
fn an_opening_that_does_not_match_its_commitment_is_refused() {
    let (_dir, repo) = node();
    let committed = repo.committed_at(v(1)).unwrap();
    let FileEntry::Hidden { commitment, .. } = committed.entries()[&path(".env")] else {
        panic!()
    };
    let [(_, mut opening, _)] = repo
        .openings(&[commitment], &Reader::maintainer())
        .unwrap()
        .try_into()
        .unwrap();
    opening.salt[0] ^= 1;
    let bytes =
        javelin_protocol::codec::encode(&Message::Opened(vec![(commitment, opening)])).unwrap();
    let error = javelin_protocol::codec::decode(&bytes).unwrap_err();
    assert_eq!(error.code, "id_mismatch");
}
