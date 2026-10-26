//! Superposition storage format: hidden entries are committed as their audience and a
//! salted commitment, IDs are the same for every reader, the policy applies from the
//! version that sets it, and a writer never touches a path hidden from it.

use javelin::{
    Audience, FileEntry, LayerId, LayerState, LayerUpdate, ObjectId, Opening, OperationId,
    POLICY_PATH, PublishAttribution, PublishOutcome, Reader, RepoPath, Repository, SealedKind,
    SparseChanges, VersionNumber, WorldVersion,
    tree::{Chunk, MAX_CHUNK, Node},
};
use tempfile::TempDir;

const POLICY: &str = "\
# pattern        audience
.env*            maintainers
config/prod/**   maintainers
internal/**      members
internal/keys/** maintainers
";
const TOKEN: &[u8] = b"TOKEN=hunter2\n";

fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}
fn path(s: &str) -> RepoPath {
    RepoPath::parse(s).unwrap()
}
fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn repo() -> (TempDir, Repository) {
    let dir = TempDir::new().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    (dir, repo)
}

/// Captures `changes` (None deletes) into a new Layer on the head and publishes it.
fn publish(repo: &mut Repository, name: &str, edits: &[(&str, Option<&[u8]>)]) -> WorldVersion {
    let changes = changes(repo, edits);
    let head = layer_on_head(repo, name, &changes).unwrap();
    let receipt = repo
        .publish_layer(
            &op(&format!("publish-{name}")),
            &head,
            &PublishAttribution::Human,
            |_, _| Ok(()),
        )
        .unwrap();
    let PublishOutcome::Published { version } = receipt.outcome else {
        panic!("{name} did not publish: {:?}", receipt.outcome)
    };
    version
}

fn changes(repo: &mut Repository, changes: &[(&str, Option<&[u8]>)]) -> SparseChanges {
    SparseChanges::new(changes.iter().map(|(p, bytes)| {
        let entry = bytes.map(|bytes| FileEntry::Regular {
            content: repo.put_blob(bytes).unwrap(),
            executable: false,
        });
        (path(p), entry)
    }))
    .unwrap()
}

fn layer_on_head(
    repo: &mut Repository,
    name: &str,
    changes: &SparseChanges,
) -> javelin::Result<javelin::LayerHead> {
    let layer = LayerId::parse(name).unwrap();
    let base = repo.layer_base(repo.head()?.number)?;
    let initial = repo.create_layer(&layer, &base)?;
    match repo.capture_layer(&op(&format!("capture-{name}")), &initial, changes)? {
        LayerUpdate::Applied {
            state: LayerState::Active { head },
        } => Ok(head),
        other => panic!("capture of {name}: {other:?}"),
    }
}

/// World 1 of the shared fixture: the policy, hidden files and directories, a nested
/// narrower audience, and public files.
fn fixture() -> (TempDir, Repository, WorldVersion) {
    let (dir, mut repo) = repo();
    let version = publish(
        &mut repo,
        "fixture",
        &[
            (POLICY_PATH, Some(POLICY.as_bytes())),
            (".env", Some(TOKEN)),
            ("config/prod/db.yml", Some(b"password: swordfish\n")),
            ("config/prod/empty", Some(b"")),
            ("config/staging.yml", Some(b"debug: true\n")),
            ("internal/notes", Some(b"launch on friday\n")),
            ("internal/keys/k", Some(b"-----BEGIN KEY-----\n")),
            ("src/main.rs", Some(b"fn main() {}\n")),
            ("src/empty", Some(b"")),
        ],
    );
    (dir, repo, version)
}

fn hidden(manifest: &javelin::Manifest, p: &str) -> (ObjectId, Audience) {
    match &manifest.entries()[&path(p)] {
        FileEntry::Hidden {
            commitment,
            audience,
        } => (*commitment, audience.clone()),
        other => panic!("{p} is not hidden: {other:?}"),
    }
}

/// Every committed chunk of the tree at `root`, without opening hidden entries.
fn chunks(repo: &Repository, root: ObjectId, out: &mut Vec<Vec<u8>>) {
    let bytes = repo.tree_bytes_bounded(root, MAX_CHUNK).unwrap();
    let chunk = Chunk::decode(&bytes).unwrap();
    out.push(bytes);
    for child in chunk.children() {
        chunks(repo, child, out);
    }
    for (_, node) in chunk.entries() {
        if let Node::Dir(dir) = node {
            chunks(repo, dir, out);
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn the_committed_form_hides_names_contents_and_ids() {
    let (_dir, repo, version) = fixture();
    let committed = repo.committed_at(version.number).unwrap();
    let paths: Vec<&str> = committed.entries().keys().map(RepoPath::as_str).collect();
    assert_eq!(
        paths,
        [
            ".env",
            POLICY_PATH,
            "config/prod",
            "config/staging.yml",
            "internal",
            "src/empty",
            "src/main.rs"
        ]
    );
    assert_eq!(hidden(&committed, ".env").1, Audience::Maintainers);
    assert_eq!(hidden(&committed, "config/prod").1, Audience::Maintainers);
    assert_eq!(hidden(&committed, "internal").1, Audience::Members);

    // This store opens everything, so its view is the plain snapshot.
    let view = repo.manifest_at(version.number).unwrap();
    for p in [
        ".env",
        "config/prod/db.yml",
        "internal/notes",
        "internal/keys/k",
    ] {
        assert!(!view.entries()[&path(p)].is_hidden(), "{p}");
    }

    // Neither the changeset that the version ID hashes nor any committed chunk names a
    // hidden file, its plain ID, or a name inside a hidden directory.
    let secret = ObjectId::for_blob(TOKEN);
    let db = ObjectId::for_blob(b"password: swordfish\n");
    let changeset = repo.changeset(version.number).unwrap().canonical_bytes();
    let mut trees = Vec::new();
    chunks(&repo, version.tree, &mut trees);
    for bytes in std::iter::once(&changeset).chain(&trees) {
        for needle in [
            secret.to_string().into_bytes(),
            secret.as_bytes().to_vec(),
            db.to_string().into_bytes(),
            db.as_bytes().to_vec(),
            b"db.yml".to_vec(),
            b"notes".to_vec(),
            b"keys".to_vec(),
        ] {
            assert!(!contains(bytes, &needle));
        }
    }
    repo.check().unwrap();
}

#[test]
fn a_guessed_content_id_does_not_match_the_commitment() {
    let (_dir, repo, version) = fixture();
    let committed = repo.committed_at(version.number).unwrap();
    let (commitment, _) = hidden(&committed, ".env");
    // The attacker guesses the exact content. Without the salt, no guess matches.
    let guess = ObjectId::for_blob(TOKEN);
    assert_ne!(commitment, guess);
    for salt in [[0; 32], [0xff; 32]] {
        let guessed = Opening {
            salt,
            kind: SealedKind::Regular,
            content: guess,
        };
        assert_ne!(guessed.commitment(), commitment);
    }
    // The real opening is the same content under a salt only its audience receives.
    let [(c, opening, audience)] = repo
        .openings(&[commitment], &Reader::maintainer())
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(
        (c, opening.content, audience),
        (commitment, guess, Audience::Maintainers)
    );
    assert_eq!(opening.commitment(), commitment);
    assert!(
        repo.openings(&[commitment], &Reader::outsider())
            .unwrap()
            .is_empty()
    );
    assert!(
        repo.openings(&[commitment], &Reader::member([]))
            .unwrap()
            .is_empty()
    );
    let (internal, _) = hidden(&committed, "internal");
    assert_eq!(
        repo.openings(&[internal], &Reader::member([]))
            .unwrap()
            .len(),
        1
    );
    // Equal content at two hidden paths commits to unrelated values.
    let inner = repo.openings(&[internal], &Reader::member([])).unwrap();
    assert_ne!(inner[0].0, commitment);
}

#[test]
fn publishing_reseals_only_what_changed() {
    let (_dir, mut repo, one) = fixture();
    let before = repo.committed_at(one.number).unwrap();

    // An edit inside a hidden directory changes that one entry and nothing else.
    let two = publish(
        &mut repo,
        "db",
        &[("config/prod/db.yml", Some(b"password: x\n"))],
    );
    let changes = repo.changeset(two.number).unwrap();
    assert_eq!(
        changes
            .entries()
            .keys()
            .map(RepoPath::as_str)
            .collect::<Vec<_>>(),
        ["config/prod"]
    );
    let after = repo.committed_at(two.number).unwrap();
    assert_ne!(
        hidden(&after, "config/prod"),
        hidden(&before, "config/prod")
    );
    assert_eq!(hidden(&after, ".env"), hidden(&before, ".env"));
    assert_eq!(hidden(&after, "internal"), hidden(&before, "internal"));

    // A nested hidden entry reseals the entries that hold it.
    let three = publish(&mut repo, "key", &[("internal/keys/k", Some(b"new key\n"))]);
    assert_eq!(
        repo.changeset(three.number)
            .unwrap()
            .entries()
            .keys()
            .map(RepoPath::as_str)
            .collect::<Vec<_>>(),
        ["internal"]
    );

    // Reverting a hidden file to old content gives a fresh commitment.
    let four = publish(&mut repo, "env", &[(".env", Some(b"other\n"))]);
    let five = publish(&mut repo, "revert", &[(".env", Some(TOKEN))]);
    let reverted = hidden(&repo.committed_at(five.number).unwrap(), ".env");
    assert_ne!(reverted, hidden(&before, ".env"));
    assert_ne!(
        reverted,
        hidden(&repo.committed_at(four.number).unwrap(), ".env")
    );
    assert_eq!(
        repo.manifest_at(five.number).unwrap().entries()[&path(".env")],
        repo.manifest_at(one.number).unwrap().entries()[&path(".env")]
    );

    // A public edit leaves every hidden entry as it was.
    let six = publish(
        &mut repo,
        "public",
        &[("src/main.rs", Some(b"fn main() { }\n"))],
    );
    let changes = repo.changeset(six.number).unwrap();
    assert_eq!(
        changes
            .entries()
            .keys()
            .map(RepoPath::as_str)
            .collect::<Vec<_>>(),
        ["src/main.rs"]
    );
    repo.check().unwrap();
    repo.history().unwrap();
}

#[test]
fn a_policy_change_applies_from_its_version() {
    let (_dir, mut repo) = repo();
    let one = publish(
        &mut repo,
        "one",
        &[("notes/plan.md", Some(b"plan\n")), ("src/a", Some(b"a\n"))],
    );
    let one_committed = repo.committed_at(one.number).unwrap();
    let two = publish(
        &mut repo,
        "two",
        &[(POLICY_PATH, Some(b"notes/** maintainers\n"))],
    );
    // The earlier version is untouched: same ID, same root, plain entry.
    assert_eq!(repo.version(one.number).unwrap(), one);
    assert_eq!(repo.committed_at(one.number).unwrap(), one_committed);
    assert!(!one_committed.entries()[&path("notes/plan.md")].is_hidden());
    // From the policy's own version on, the directory is one hidden entry.
    let committed = repo.committed_at(two.number).unwrap();
    let (sealed, audience) = hidden(&committed, "notes");
    assert_eq!(audience, Audience::Maintainers);
    assert!(!committed.entries().contains_key("notes/plan.md"));
    let changes = repo.changeset(two.number).unwrap();
    assert_eq!(
        changes
            .entries()
            .keys()
            .map(RepoPath::as_str)
            .collect::<Vec<_>>(),
        [POLICY_PATH, "notes", "notes/plan.md"]
    );
    assert_eq!(changes.entries()[&path("notes/plan.md")], None);

    // Removing the rule discloses the directory from that version on. Earlier IDs and
    // the old commitment's opening stay as they were.
    let three = publish(&mut repo, "three", &[(POLICY_PATH, Some(b""))]);
    let committed = repo.committed_at(three.number).unwrap();
    assert!(!committed.entries()[&path("notes/plan.md")].is_hidden());
    assert_eq!(repo.version(one.number).unwrap(), one);
    assert_eq!(repo.version(two.number).unwrap(), two);
    let opened = repo.openings(&[sealed], &Reader::maintainer()).unwrap();
    assert_eq!(opened[0].1.kind, SealedKind::Directory);
    repo.check().unwrap();
}

#[test]
fn a_policy_file_that_does_not_parse_is_refused() {
    let (_dir, mut repo) = repo();
    let changes = changes(&mut repo, &[(POLICY_PATH, Some(b"secret everyone\n"))]);
    let head = layer_on_head(&mut repo, "bad", &changes).unwrap();
    let error = repo
        .publish_layer(&op("bad"), &head, &PublishAttribution::Human, |_, _| Ok(()))
        .unwrap_err();
    assert_eq!(error.code(), "invalid_input");
    assert_eq!(repo.head().unwrap().number, v(0));
}

/// A repository that holds only what an outsider receives: committed trees and
/// public blobs, no openings.
fn outsider_copy(source: &Repository, version: &WorldVersion) -> (TempDir, Repository) {
    fn copy(from: &Repository, to: &mut Repository, root: ObjectId) {
        if to.has_tree(root).unwrap() {
            return;
        }
        let bytes = from.tree_bytes_bounded(root, MAX_CHUNK).unwrap();
        let chunk = Chunk::decode(&bytes).unwrap();
        for child in chunk.children() {
            copy(from, to, child);
        }
        for (_, node) in chunk.entries() {
            match node {
                Node::Dir(dir) => copy(from, to, dir),
                Node::File(entry) => {
                    if let Some(blob) = entry.content() {
                        to.put_blob(&from.read_blob(blob).unwrap()).unwrap();
                    }
                }
            }
        }
        to.put_tree(&bytes).unwrap();
    }
    let (dir, mut repo) = repo();
    copy(source, &mut repo, version.tree);
    repo.advance_to_tree(&op("attach"), v(0), version.tree)
        .unwrap();
    (dir, repo)
}

#[test]
fn an_outsider_gets_the_same_ids_and_never_touches_hidden_paths() {
    let (_dir, mut node, one) = fixture();
    let (_odir, mut outsider) = outsider_copy(&node, &one);
    // The outsider's World 1 has the node's root and ID: both hash the committed form.
    assert_eq!(outsider.head().unwrap().id, one.id);
    assert_eq!(outsider.head().unwrap().tree, one.tree);
    let view = outsider.manifest_at(v(1)).unwrap();
    assert_eq!(view, node.committed_at(one.number).unwrap());
    outsider.check().unwrap();

    // Writing at, below or over a hidden entry is refused.
    for (name, change) in [
        ("write", (".env", Some(b"x".as_slice()))),
        ("below", ("config/prod/new", Some(b"x".as_slice()))),
        ("delete", ("internal", None)),
    ] {
        let changes = changes(&mut outsider, &[change]);
        let error = layer_on_head(&mut outsider, name, &changes).unwrap_err();
        assert_eq!(error.code(), "hidden_path", "{name}");
    }

    // The outsider's materialized base leaves hidden entries out, and capturing the
    // directory does not read their absence as a deletion.
    let layer = LayerId::parse("work").unwrap();
    let base = outsider.layer_base(v(1)).unwrap();
    outsider.create_layer(&layer, &base).unwrap();
    outsider.open_workspace(&layer).unwrap();
    let root = outsider.workspace_root(&layer).unwrap();
    assert!(root.join("src/main.rs").exists());
    for missing in [".env", "config/prod", "internal"] {
        assert!(!root.join(missing).exists(), "{missing}");
    }
    assert!(root.join("config/staging.yml").exists());
    std::fs::write(root.join("src/main.rs"), "fn main() { outsider() }\n").unwrap();
    let capture = outsider.capture_workspace(&layer).unwrap().unwrap();
    assert_eq!(
        capture
            .changes
            .entries()
            .keys()
            .map(RepoPath::as_str)
            .collect::<Vec<_>>(),
        ["src/main.rs"]
    );
    // Composed on the node, the outsider's Layer leaves every hidden path unchanged.
    let before = node.committed_at(one.number).unwrap();
    node.put_blob(b"fn main() { outsider() }\n").unwrap();
    let head = layer_on_head(&mut node, "outsider", &capture.changes).unwrap();
    let PublishOutcome::Published { version } = node
        .publish_layer(
            &op("outsider"),
            &head,
            &PublishAttribution::Human,
            |_, _| Ok(()),
        )
        .unwrap()
        .outcome
    else {
        panic!("outsider's Layer did not publish")
    };
    let after = node.committed_at(version.number).unwrap();
    for p in [".env", "config/prod", "internal"] {
        assert_eq!(hidden(&after, p), hidden(&before, p), "{p}");
    }
    assert_eq!(
        node.manifest_at(version.number).unwrap().entries()[&path(".env")],
        node.manifest_at(one.number).unwrap().entries()[&path(".env")]
    );
    node.check().unwrap();
}

#[test]
fn a_node_refuses_writers_outside_an_audience() {
    let (_dir, node, one) = fixture();
    // The writer check reads paths only, so the entries need no stored blob.
    let write = |p: &str| {
        SparseChanges::new([(
            path(p),
            Some(FileEntry::Regular {
                content: ObjectId::for_blob(b"x"),
                executable: false,
            }),
        )])
        .unwrap()
    };
    let cases = [
        (".env", Reader::outsider(), false),
        ("src/x", Reader::outsider(), true),
        ("internal/notes", Reader::outsider(), false),
        ("internal/notes", Reader::member([]), true),
        ("internal/keys/k", Reader::member([]), false),
        ("config/prod/db.yml", Reader::member([]), false),
        (POLICY_PATH, Reader::member([]), false),
        (POLICY_PATH, Reader::maintainer(), true),
        ("internal/keys/k", Reader::maintainer(), true),
    ];
    for (p, writer, allowed) in cases {
        let result = node.check_writer(&write(p), one.number, &writer);
        assert_eq!(result.is_ok(), allowed, "{p} {writer:?}");
        if let Err(error) = result {
            assert_eq!(error.code(), "hidden_path");
        }
    }
    let reveals = |p: &str, reader: &Reader| node.conceals(&write(p), one.number, reader).unwrap();
    assert!(reveals(".env", &Reader::member([])));
    assert!(!reveals("src/x", &Reader::outsider()));
    assert!(!reveals(".env", &Reader::maintainer()));
}

#[test]
fn objects_are_readable_by_their_audiences_only() {
    let (_dir, repo, one) = fixture();
    let view = repo.manifest_at(one.number).unwrap();
    let blob = |p: &str| view.entries()[&path(p)].content().unwrap();
    let secret = blob(".env");
    let notes = blob("internal/notes");
    let key = blob("internal/keys/k");
    let public = blob("src/main.rs");
    // The empty file is hidden under config/prod and public under src.
    let empty = blob("config/prod/empty");
    assert_eq!(empty, blob("src/empty"));
    let check = |id: ObjectId, reader: Reader| repo.readable(id, &reader).unwrap();
    assert!(check(public, Reader::outsider()));
    assert!(check(empty, Reader::outsider()));
    assert!(!check(secret, Reader::outsider()));
    assert!(!check(notes, Reader::outsider()));
    assert!(check(notes, Reader::member([])));
    assert!(!check(secret, Reader::member([])));
    assert!(!check(key, Reader::member([])));
    assert!(check(key, Reader::maintainer()));
    // A hidden directory's own chunks follow its audience.
    let committed = repo.committed_at(one.number).unwrap();
    let (prod, _) = hidden(&committed, "config/prod");
    let [(_, opening, _)] = repo
        .openings(&[prod], &Reader::maintainer())
        .unwrap()
        .try_into()
        .unwrap();
    assert!(!check(opening.content, Reader::outsider()));
    assert!(!check(opening.content, Reader::member([])));
}

#[test]
fn hidden_items_have_one_encoding() {
    let item = |audience: &[u8]| {
        let mut bytes = vec![0, 5, 0, 1, b'a'];
        bytes.extend([7; 32]);
        bytes.push(audience.len() as u8);
        bytes.extend(audience);
        bytes
    };
    let chunk = Chunk::decode(&item(b"group:ops")).unwrap();
    assert_eq!(chunk.encode(), item(b"group:ops"));
    let entries: Vec<(&str, Node)> = chunk.entries().collect();
    let [(name, Node::File(FileEntry::Hidden { audience, .. }))] = &entries[..] else {
        panic!("not a hidden entry")
    };
    let (name, audience) = (*name, audience.clone());
    assert_eq!((name, audience), ("a", Audience::Group("ops".into())));
    for bad in [&b"public"[..], b"everyone", b"group:", b"members!"] {
        assert!(Chunk::decode(&item(bad)).is_err(), "{bad:?}");
    }
    let mut truncated = item(b"members");
    truncated.pop();
    assert!(Chunk::decode(&truncated).is_err());
    let mut unknown = item(b"members");
    unknown[1] = 6;
    assert!(Chunk::decode(&unknown).is_err());
}

#[test]
fn older_formats_are_refused() {
    let (dir, repo) = repo();
    drop(repo);
    let db = rusqlite::Connection::open(dir.path().join(".javelin/repository.sqlite3")).unwrap();
    db.pragma_update(None, "user_version", 9).unwrap();
    drop(db);
    let error = Repository::open(dir.path()).err().unwrap();
    assert_eq!(error.code(), "unsupported_format");
}
