//! World versions are changesets: indexed Publish composition, derived snapshots
//! and hash-chain identity.
use std::collections::{BTreeMap, BTreeSet};

use javelin::{
    FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, Manifest, OperationId,
    PublishAttribution, PublishOutcome, RepoPath, Repository, SparseChanges, VersionNumber,
};
use rusqlite::Connection;
use tempfile::TempDir;

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn path(s: &str) -> RepoPath {
    RepoPath::parse(s).unwrap()
}
fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}
fn regular(repo: &mut Repository, bytes: &[u8]) -> FileEntry {
    FileEntry::Regular {
        content: repo.put_blob(bytes).unwrap(),
        executable: false,
    }
}

/// Creates a Layer on `base`, captures `changes`, and publishes it.
fn publish(
    repo: &mut Repository,
    name: &str,
    base: u64,
    changes: &[(&str, Option<FileEntry>)],
) -> PublishOutcome {
    let head = repo
        .create_layer(
            &LayerId::parse(name).unwrap(),
            &repo.layer_base(v(base)).unwrap(),
        )
        .unwrap();
    let changes = SparseChanges::new(changes.iter().map(|(p, e)| (path(p), e.clone()))).unwrap();
    let head = match repo
        .capture_layer(&op(&format!("capture-{name}")), &head, &changes)
        .unwrap()
    {
        LayerUpdate::Applied {
            state: LayerState::Active { head },
        } => head,
        other => panic!("{other:?}"),
    };
    publish_head(repo, name, &head)
}

fn publish_head(repo: &mut Repository, name: &str, head: &LayerHead) -> PublishOutcome {
    repo.publish_layer(&op(name), head, &PublishAttribution::Human, |_, _| Ok(()))
        .unwrap()
        .outcome
}

type Case<'a> = (&'a str, &'a [(&'a str, Option<FileEntry>)], &'a [&'a str]);

fn conflict(outcome: PublishOutcome) -> Vec<String> {
    match outcome {
        PublishOutcome::Conflict { paths } => paths.iter().map(|p| p.as_str().into()).collect(),
        other => panic!("expected conflict: {other:?}"),
    }
}

#[test]
fn indexed_composition_reports_exact_ancestor_and_descendant_intersections() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let a = regular(&mut repo, b"A");
    let b = regular(&mut repo, b"B");
    let c = regular(&mut repo, b"C");
    // World 1: a shared base.
    publish(
        &mut repo,
        "base",
        0,
        &[
            ("dir/file", Some(a.clone())),
            ("file", Some(a.clone())),
            ("gone", Some(a.clone())),
            ("tree/x/y", Some(a.clone())),
            ("other", Some(a.clone())),
        ],
    );
    // World 2 changes, all relative to World 1.
    publish(
        &mut repo,
        "world",
        1,
        &[
            ("dir/file", Some(b.clone())),
            ("gone", None),
            ("tree/x/y", None),
            ("tree/x", Some(b.clone())),
            ("new/deep/leaf", Some(b.clone())),
        ],
    );
    let cases: [Case; 7] = [
        ("exact", &[("dir/file", Some(c.clone()))], &["dir/file"]),
        ("exact-deletion", &[("gone", Some(c.clone()))], &["gone"]),
        // The Layer adds a file where the World now has a directory.
        (
            "file-over-dir",
            &[("new", Some(c.clone()))],
            &["new", "new/deep/leaf"],
        ),
        (
            "file-over-deep-dir",
            &[("new/deep", Some(c.clone()))],
            &["new/deep", "new/deep/leaf"],
        ),
        // The Layer edits below a path the World changed to a file.
        (
            "below-world-file",
            &[("tree/x/z", Some(c.clone()))],
            &["tree/x", "tree/x/z"],
        ),
        // The Layer deletes a file the World replaced with a file at its parent.
        (
            "delete-under",
            &[("tree/x/y", None)],
            &["tree/x", "tree/x/y"],
        ),
        // Siblings and unrelated paths never intersect; only the hit is reported.
        (
            "mixed",
            &[
                ("other", None),
                ("dir/sibling", Some(c.clone())),
                ("dir/file", None),
            ],
            &["dir/file"],
        ),
    ];
    for (name, changes, expected) in cases {
        assert_eq!(
            conflict(publish(&mut repo, name, 1, changes)),
            expected,
            "{name}"
        );
    }
    // Disjoint changes compose without touching the World's own changes.
    let outcome = publish(
        &mut repo,
        "clean",
        1,
        &[("dir/sibling", Some(c.clone())), ("other", None)],
    );
    let PublishOutcome::Published { version } = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(
        repo.changeset(version.number).unwrap().entries(),
        &BTreeMap::from([
            (path("dir/sibling"), Some(c.clone())),
            (path("other"), None)
        ])
    );
    let snapshot = repo.manifest_at(version.number).unwrap();
    assert_eq!(
        snapshot
            .entries()
            .keys()
            .map(RepoPath::as_str)
            .collect::<Vec<_>>(),
        ["dir/file", "dir/sibling", "file", "new/deep/leaf", "tree/x"]
    );
    repo.check().unwrap();
}

#[test]
fn net_world_revert_is_not_an_intersection() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let a = regular(&mut repo, b"A");
    let b = regular(&mut repo, b"B");
    publish(
        &mut repo,
        "base",
        0,
        &[("f", Some(a.clone())), ("d/x", Some(a.clone()))],
    );
    publish(&mut repo, "w1", 1, &[("f", Some(b.clone())), ("d/x", None)]);
    publish(
        &mut repo,
        "w2",
        2,
        &[("f", Some(a.clone())), ("d/x", Some(a.clone()))],
    );
    let outcome = publish(&mut repo, "layer", 1, &[("f", None), ("d/y", Some(b))]);
    assert!(
        matches!(outcome, PublishOutcome::Published { .. }),
        "{outcome:?}"
    );
    repo.check().unwrap();
}

/// Deterministic xorshift; no extra dependencies.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const PATHS: [&str; 12] = [
    "a", "a/b", "a/c", "a/b/c", "a/b/d", "b", "b/a", "c", "c/d/e", "c/d", "d", "d/e/f",
];

fn live_ancestor(snapshot: &BTreeMap<RepoPath, FileEntry>, path: &RepoPath) -> bool {
    let s = path.as_str();
    s.match_indices('/')
        .any(|(i, _)| snapshot.contains_key(&RepoPath::parse(&s[..i]).unwrap()))
}

/// The pre-changeset algorithm: full snapshots, an end-to-end diff over every path
/// and the nested exact/ancestor/descendant intersection loop.
fn reference_compose(
    base: &BTreeMap<RepoPath, FileEntry>,
    current: &BTreeMap<RepoPath, FileEntry>,
    changes: &BTreeMap<RepoPath, Option<FileEntry>>,
) -> Result<BTreeMap<RepoPath, FileEntry>, Vec<String>> {
    let ancestor = |p: &RepoPath, c: &RepoPath| {
        c.as_str()
            .strip_prefix(p.as_str())
            .is_some_and(|t| t.starts_with('/'))
    };
    let world: BTreeSet<_> = base
        .keys()
        .chain(current.keys())
        .filter(|p| base.get(*p) != current.get(*p))
        .collect();
    let mut hits = BTreeSet::new();
    for p in changes.keys() {
        for c in &world {
            if p == *c || ancestor(p, c) || ancestor(c, p) {
                hits.insert(p.as_str().to_string());
                hits.insert(c.as_str().to_string());
            }
        }
    }
    if !hits.is_empty() {
        return Err(hits.into_iter().collect());
    }
    let mut entries = current.clone();
    for (p, e) in changes {
        match e {
            Some(e) => entries.insert(p.clone(), e.clone()),
            None => entries.remove(p),
        };
    }
    Ok(entries)
}

#[test]
fn derived_snapshots_and_outcomes_match_full_manifest_composition_over_random_publishes() {
    for seed in 1..=6u64 {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let blobs: Vec<_> = (0..4u8).map(|i| regular(&mut repo, &[i])).collect();
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        // snapshots[n] is the reference snapshot of World n.
        let mut snapshots = vec![BTreeMap::new()];
        let (mut published, mut conflicts) = (0, 0);
        for step in 0..60 {
            let base = rng.below(snapshots.len());
            let base_snapshot = snapshots[base].clone();
            // A valid Layer delta against its own base: edit, add or delete.
            let mut changes = BTreeMap::new();
            let mut layer = base_snapshot.clone();
            for _ in 0..1 + rng.below(3) {
                let p = path(PATHS[rng.below(PATHS.len())]);
                if changes.contains_key(&p) {
                    continue;
                }
                let after = if layer.contains_key(&p) && rng.below(3) == 0 {
                    None
                } else {
                    Some(blobs[rng.below(blobs.len())].clone())
                };
                let mut next = layer.clone();
                match &after {
                    Some(e) => next.insert(p.clone(), e.clone()),
                    None => next.remove(&p),
                };
                let (start, end) = (format!("{}/", p.as_str()), format!("{}0", p.as_str()));
                let collides = after.is_some()
                    && (live_ancestor(&next, &p)
                        || next
                            .keys()
                            .any(|k| k.as_str() >= start.as_str() && k.as_str() < end.as_str()));
                if collides || base_snapshot.get(&p) == after.as_ref() {
                    continue;
                }
                layer = next;
                changes.insert(p, after);
            }
            let name = format!("s{seed}-{step}");
            let entries: Vec<(&str, Option<FileEntry>)> = changes
                .iter()
                .map(|(p, e)| (p.as_str(), e.clone()))
                .collect();
            let head = snapshots.len() as u64 - 1;
            let expected = reference_compose(&base_snapshot, snapshots.last().unwrap(), &changes);
            match (publish(&mut repo, &name, base as u64, &entries), expected) {
                (PublishOutcome::Published { version }, Ok(snapshot)) => {
                    assert_eq!(version.number, v(head + 1), "{name}");
                    assert_eq!(
                        repo.manifest_at(version.number).unwrap().entries(),
                        &snapshot
                    );
                    snapshots.push(snapshot);
                    published += 1;
                }
                (PublishOutcome::Conflict { paths }, Err(hits)) => {
                    let paths: Vec<String> = paths.iter().map(|p| p.as_str().into()).collect();
                    assert_eq!(paths, hits, "{name}");
                    conflicts += 1;
                }
                (actual, expected) => panic!("{name}: {actual:?} vs {expected:?}"),
            }
        }
        assert!(
            published > 10 && conflicts > 5,
            "seed {seed}: {published}/{conflicts}"
        );
        for (n, snapshot) in snapshots.iter().enumerate() {
            assert_eq!(repo.manifest_at(v(n as u64)).unwrap().entries(), snapshot);
        }
        repo.check().unwrap();
    }
}

fn raw(root: &TempDir) -> Connection {
    Connection::open(root.path().join(".javelin/repository.sqlite3")).unwrap()
}

#[test]
fn version_identity_is_a_stable_hash_chain_verified_by_check() {
    let build = || {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let a = regular(&mut repo, b"A");
        let b = regular(&mut repo, b"B");
        publish(
            &mut repo,
            "one",
            0,
            &[("x", Some(a.clone())), ("y/z", Some(a))],
        );
        publish(&mut repo, "two", 1, &[("x", Some(b)), ("y/z", None)]);
        let ids: Vec<_> = repo.history().unwrap().iter().map(|w| w.id).collect();
        (root, ids)
    };
    let (root, ids) = build();
    let (_, again) = build();
    // Identity depends only on changesets, not on the repository instance.
    assert_eq!(ids, again);
    assert_eq!(ids.iter().collect::<BTreeSet<_>>().len(), 3);
    let reopened = Repository::open(root.path()).unwrap();
    assert_eq!(reopened.version(v(2)).unwrap().id, ids[2]);
    assert_eq!(reopened.head().unwrap().id, ids[2]);
    reopened.check().unwrap();
    // A Layer base binds the exact version identity.
    assert_eq!(reopened.layer_base(v(1)).unwrap().world(), ids[1]);
    for damage in ["id", "content", "path", "extra", "delete"] {
        let (root, _) = build();
        let db = raw(&root);
        db.execute_batch("PRAGMA foreign_keys=OFF; DROP TRIGGER versions_no_UPDATE; DROP TRIGGER changes_no_UPDATE; DROP TRIGGER changes_no_DELETE;").unwrap();
        let sql = match damage {
            "id" => format!("UPDATE versions SET id='{}' WHERE number=1", "0".repeat(64)),
            "content" => "UPDATE changes SET content=(SELECT content FROM changes WHERE version=2 AND path='x') WHERE version=1 AND path='x'".into(),
            "path" => "UPDATE changes SET path='w' WHERE version=1 AND path='x'".into(),
            "extra" => "INSERT INTO changes SELECT 'q',2,kind,content FROM changes WHERE version=1 AND path='x'".into(),
            "delete" => "DELETE FROM changes WHERE version=2 AND path='y/z'".into(),
            _ => unreachable!(),
        };
        db.execute(&sql, []).unwrap();
        let repo = Repository::open(root.path()).unwrap();
        assert!(repo.check().is_err(), "{damage}");
        assert!(repo.history().is_err(), "{damage}");
    }
}

#[test]
fn full_manifest_advance_records_the_exact_changeset() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let a = regular(&mut repo, b"A");
    let b = regular(&mut repo, b"B");
    publish(
        &mut repo,
        "one",
        0,
        &[("keep", Some(a.clone())), ("drop", Some(a.clone()))],
    );
    let manifest = Manifest::new([(path("keep"), a), (path("add"), b.clone())]).unwrap();
    let id = repo.put_manifest(&manifest).unwrap();
    repo.compare_and_advance(&op("raw"), v(1), id).unwrap();
    assert_eq!(
        repo.changeset(v(2)).unwrap().entries(),
        &BTreeMap::from([(path("add"), Some(b)), (path("drop"), None)])
    );
    assert_eq!(repo.manifest_at(v(2)).unwrap(), manifest);
    repo.check().unwrap();
}
