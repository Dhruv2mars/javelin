//! Derived Merkle trees: history independence, differential checks against flat
//! snapshots, prolly chunking of large directories, decoding and integrity.
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
};

use javelin::{
    FileEntry, LayerId, LayerState, LayerUpdate, ObjectId, OperationId, PublishAttribution,
    PublishOutcome, RepoPath, Repository, SparseChanges, VersionNumber,
    tree::{Chunk, MemoryStore, Node, Store, Trees},
};
use tempfile::TempDir;

type Snapshot = BTreeMap<RepoPath, FileEntry>;

/// Deterministic xorshift; no extra dependencies.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }
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

fn path(s: &str) -> RepoPath {
    RepoPath::parse(s).unwrap()
}
fn file(n: u64, executable: bool) -> FileEntry {
    FileEntry::Regular {
        content: ObjectId::parse(&format!("{n:064x}")).unwrap(),
        executable,
    }
}
fn entry(rng: &mut Rng) -> FileEntry {
    match rng.below(4) {
        0 => FileEntry::Symlink {
            target: ObjectId::parse(&format!("{:064x}", rng.below(5))).unwrap(),
        },
        n => file(rng.below(5) as u64, n == 1),
    }
}
fn trees() -> Trees<MemoryStore> {
    Trees::new(MemoryStore::default())
}
fn build(trees: &Trees<impl Store>, snapshot: &Snapshot) -> ObjectId {
    trees.build(snapshot.iter()).unwrap()
}

/// Paths over a small alphabet so files, directories, deep chains and
/// prefix-sharing names (`a`, `a.b`, `a/b`) collide often.
fn random_path(rng: &mut Rng) -> RepoPath {
    const NAMES: [&str; 6] = ["a", "b", "a.b", "c-1", "a0", "z"];
    let depth = 1 + rng.below(4);
    path(
        &(0..depth)
            .map(|_| NAMES[rng.below(NAMES.len())])
            .collect::<Vec<_>>()
            .join("/"),
    )
}

fn has_ancestor(snapshot: &Snapshot, p: &RepoPath) -> bool {
    let s = p.as_str();
    s.match_indices('/')
        .any(|(i, _)| snapshot.contains_key(&path(&s[..i])))
}
fn has_descendant(snapshot: &Snapshot, p: &RepoPath) -> bool {
    let (start, end) = (format!("{}/", p.as_str()), format!("{}0", p.as_str()));
    snapshot
        .keys()
        .any(|k| k.as_str() >= start.as_str() && k.as_str() < end.as_str())
}

/// A random valid changeset against `snapshot`: edits, adds, deletes, and files
/// replacing directories (with every file below deleted) and the reverse.
fn random_changes(rng: &mut Rng, snapshot: &Snapshot, size: usize) -> Snapshot {
    let mut next = snapshot.clone();
    for _ in 0..size {
        let existing: Vec<_> = next.keys().cloned().collect();
        let p = if !existing.is_empty() && rng.below(2) == 0 {
            existing[rng.below(existing.len())].clone()
        } else {
            random_path(rng)
        };
        if next.contains_key(&p) && rng.below(3) == 0 {
            next.remove(&p);
            continue;
        }
        // Make room: drop a file ancestor and every descendant.
        let s = p.as_str().to_owned();
        for (i, _) in s.match_indices('/') {
            next.remove(&path(&s[..i]));
        }
        next.retain(|k, _| !k.as_str().starts_with(&format!("{s}/")));
        next.insert(p, entry(rng));
    }
    next
}

/// The flat changeset between two snapshots.
fn flat_diff(from: &Snapshot, to: &Snapshot) -> SparseChanges {
    let paths: BTreeSet<_> = from.keys().chain(to.keys()).collect();
    SparseChanges::new(
        paths
            .into_iter()
            .filter(|p| from.get(*p) != to.get(*p))
            .map(|p| (p.clone(), to.get(p).cloned())),
    )
    .unwrap()
}

/// The pre-tree algorithm: every exact or ancestor/descendant pair between the
/// changed paths and the files that differ between base and head.
fn flat_intersections(base: &Snapshot, head: &Snapshot, changes: &SparseChanges) -> Vec<String> {
    let related = |p: &str, c: &str| {
        p == c
            || c.strip_prefix(p).is_some_and(|t| t.starts_with('/'))
            || p.strip_prefix(c).is_some_and(|t| t.starts_with('/'))
    };
    let world = flat_diff(base, head);
    let mut hits = BTreeSet::new();
    for p in changes.entries().keys() {
        for c in world.entries().keys() {
            if related(p.as_str(), c.as_str()) {
                hits.insert(p.as_str().to_owned());
                hits.insert(c.as_str().to_owned());
            }
        }
    }
    hits.into_iter().collect()
}

#[test]
fn same_entries_give_the_same_root_whatever_the_edit_order() {
    for seed in 1..=40u64 {
        let mut rng = Rng::new(seed);
        let trees = trees();
        // A random history of snapshots, applied one changeset at a time.
        let mut snapshots = vec![Snapshot::new()];
        let mut root = javelin::tree::empty();
        for _ in 0..25 {
            let n = 1 + rng.below(8);
            let next = random_changes(&mut rng, snapshots.last().unwrap(), n);
            root = trees
                .apply(root, &flat_diff(snapshots.last().unwrap(), &next))
                .unwrap();
            assert_eq!(root, build(&trees, &next), "seed {seed}");
            snapshots.push(next);
        }
        let last = snapshots.last().unwrap();
        // The same final set reached by shuffled single-path edits from a random
        // earlier snapshot, through a fresh store.
        let start = &snapshots[rng.below(snapshots.len())];
        let mut steps: Vec<_> = flat_diff(start, last)
            .entries()
            .clone()
            .into_iter()
            .collect();
        for i in (1..steps.len()).rev() {
            steps.swap(i, rng.below(i + 1));
        }
        let fresh = Trees::new(MemoryStore::default());
        let mut current = start.clone();
        let mut root = build(&fresh, start);
        let mut pending = steps;
        // Single edits can be temporarily invalid (a file under a file), so apply
        // each when it is valid against the current snapshot.
        while !pending.is_empty() {
            let index = pending
                .iter()
                .position(|(p, e)| {
                    e.is_none() || (!has_ancestor(&current, p) && !has_descendant(&current, p))
                })
                .expect("some edit is always applicable");
            let (p, e) = pending.remove(index);
            match &e {
                Some(e) => current.insert(p.clone(), e.clone()),
                None => current.remove(&p),
            };
            root = fresh
                .apply(root, &SparseChanges::new([(p, e)]).unwrap())
                .unwrap();
        }
        assert_eq!(&current, last);
        assert_eq!(root, build(&trees, last), "seed {seed}");
        assert_eq!(&fresh.entries(root).unwrap(), last);
    }
}

#[test]
fn large_directories_keep_history_independence_under_random_batches() {
    // Thousands of siblings make multi-level prolly trees; random inserts,
    // deletes and edits must re-cut to exactly the from-scratch chunks.
    for seed in 1..=6u64 {
        let mut rng = Rng::new(seed);
        let trees = trees();
        let name = |n: usize| path(&format!("big/f{n:05}"));
        let mut snapshot: Snapshot = (0..6000)
            .filter(|_| rng.below(3) != 0)
            .map(|n| (name(n), file(n as u64, false)))
            .collect();
        let mut root = build(&trees, &snapshot);
        for round in 0..30 {
            let mut next = snapshot.clone();
            for _ in 0..1 + rng.below(if round % 5 == 0 { 400 } else { 6 }) {
                let p = name(rng.below(6000));
                if next.contains_key(&p) && rng.below(2) == 0 {
                    next.remove(&p);
                } else {
                    next.insert(p, file(rng.next() % 1000, false));
                }
            }
            root = trees.apply(root, &flat_diff(&snapshot, &next)).unwrap();
            snapshot = next;
            assert_eq!(
                root,
                build(&Trees::new(MemoryStore::default()), &snapshot),
                "seed {seed} round {round}"
            );
        }
        assert_eq!(trees.entries(root).unwrap(), snapshot);
    }
}

#[test]
fn trees_match_flat_snapshots_diffs_lookups_and_intersections() {
    for seed in 1..=30u64 {
        let mut rng = Rng::new(seed);
        let trees = trees();
        let mut history = vec![(Snapshot::new(), javelin::tree::empty())];
        for _ in 0..20 {
            let (last, root) = history.last().unwrap().clone();
            let next = {
                let n = 1 + rng.below(6);
                random_changes(&mut rng, &last, n)
            };
            let root = trees.apply(root, &flat_diff(&last, &next)).unwrap();
            assert_eq!(trees.entries(root).unwrap(), next);
            history.push((next, root));
        }
        for _ in 0..40 {
            let (a, ra) = &history[rng.below(history.len())];
            let (b, rb) = &history[rng.below(history.len())];
            assert_eq!(trees.diff(*ra, *rb).unwrap(), flat_diff(a, b));
            // Lookups and descendant listings at every path the snapshots name.
            for p in a.keys().chain(b.keys()) {
                let s = p.as_str();
                let probes = std::iter::once(p.clone())
                    .chain(s.match_indices('/').map(|(i, _)| path(&s[..i])));
                for probe in probes {
                    let under: Vec<_> = a
                        .keys()
                        .filter(|k| k.as_str().starts_with(&format!("{}/", probe.as_str())))
                        .cloned()
                        .collect();
                    match trees.lookup(*ra, &probe).unwrap() {
                        Some(Node::File(e)) => assert_eq!(a.get(&probe), Some(&e)),
                        Some(Node::Dir(_)) => assert!(!a.contains_key(&probe) && !under.is_empty()),
                        None => assert!(!a.contains_key(&probe) && under.is_empty()),
                    }
                    let mut listed = trees.files_under(*ra, &probe).unwrap();
                    listed.sort();
                    assert_eq!(listed, under);
                }
            }
            // A Layer changeset against `a` checked against head `b`.
            let layer = flat_diff(a, &{
                let n = 1 + rng.below(4);
                random_changes(&mut rng, a, n)
            });
            let found: Vec<String> = trees
                .intersections(*ra, *rb, &layer)
                .unwrap()
                .into_iter()
                .map(|p| p.as_str().to_owned())
                .collect();
            assert_eq!(found, flat_intersections(a, b, &layer), "seed {seed}");
        }
    }
}

/// Counts chunk reads over a shared store, with a cold decoded cache.
struct Counting<'a>(&'a MemoryStore, Cell<usize>);
impl Store for Counting<'_> {
    fn load(&self, id: ObjectId) -> javelin::Result<Vec<u8>> {
        self.1.set(self.1.get() + 1);
        self.0.load(id)
    }
    fn save(&self, id: ObjectId, bytes: &[u8]) -> javelin::Result<()> {
        self.0.save(id, bytes)
    }
}

#[test]
fn one_edit_in_a_50k_entry_directory_rewrites_a_few_kib() {
    let trees = Trees::new(MemoryStore::default());
    let snapshot: Snapshot = (0..50_000)
        .map(|n| (path(&format!("dir/file-{n:06}.rs")), file(n, false)))
        .collect();
    let root = build(&trees, &snapshot);
    let total = trees.store().bytes();
    for (label, change) in [
        ("edit", Some(file(7, true))),
        ("delete", None),
        ("insert", Some(file(9, false))),
    ] {
        let p = match label {
            "insert" => path("dir/file-025000.rs.bak"),
            _ => path("dir/file-025000.rs"),
        };
        let before = trees.store().bytes();
        let next = trees
            .apply(root, &SparseChanges::new([(p.clone(), change)]).unwrap())
            .unwrap();
        let written = trees.store().bytes() - before;
        assert!(
            written < 16 * 1024,
            "{label} wrote {written} of {total} bytes"
        );
        let mut expected = snapshot.clone();
        match trees.lookup(next, &p).unwrap() {
            Some(Node::File(e)) => expected.insert(p, e),
            _ => expected.remove(&p),
        };
        assert_eq!(next, build(&Trees::new(MemoryStore::default()), &expected));
    }
    assert!(total > 2_000_000, "the listing is {total} bytes");
}

#[test]
fn conflict_checks_skip_subtrees_with_equal_hashes() {
    // A wide tree where the World and the Layer change unrelated directories.
    let trees = trees();
    let snapshot: Snapshot = (0..20_000)
        .map(|n| {
            (
                path(&format!("d{}/s{}/f{n}", n % 50, n % 7)),
                file(n, false),
            )
        })
        .collect();
    let base = build(&trees, &snapshot);
    let world = SparseChanges::new([(path("d1/s1/f1"), Some(file(1, true)))]).unwrap();
    let head = trees.apply(base, &world).unwrap();
    let probe = || Trees::new(Counting(trees.store(), Cell::new(0)));
    let reads = |t: &Trees<Counting<'_>>| t.store().1.get();
    // 200 new files under `d2`: both roots are read, then `d2` is equal by hash
    // on both sides and skipped with everything below it.
    let layer = SparseChanges::new(
        (0..200).map(|n| (path(&format!("d2/s{}/new{n}", n % 7)), Some(file(n, false)))),
    )
    .unwrap();
    let t = probe();
    assert!(t.intersections(base, head, &layer).unwrap().is_empty());
    assert!(reads(&t) <= 4, "{} chunk reads", reads(&t));
    // A Layer touching the World's change reads only along that path.
    let touching = SparseChanges::new([(path("d1/s1/f1"), Some(file(2, false)))]).unwrap();
    let t = probe();
    let found = t.intersections(base, head, &touching).unwrap();
    assert_eq!(found, BTreeSet::from([path("d1/s1/f1")]));
    assert!(reads(&t) <= 12, "{} chunk reads", reads(&t));
    // A Layer replacing the whole directory reports every changed file below it.
    let replace = SparseChanges::new([(path("d1"), Some(file(3, false)))]).unwrap();
    let found = probe().intersections(base, head, &replace).unwrap();
    assert_eq!(found, BTreeSet::from([path("d1"), path("d1/s1/f1")]));
}

#[test]
fn deep_paths_up_to_the_native_limit_work() {
    let trees = trees();
    let deep = vec!["dir"; javelin::MAX_DEPTH - 1].join("/") + "/f";
    let snapshot = Snapshot::from([(path(&deep), file(1, false))]);
    let root = build(&trees, &snapshot);
    assert_eq!(trees.entries(root).unwrap(), snapshot);
    let gone = trees
        .apply(root, &SparseChanges::new([(path(&deep), None)]).unwrap())
        .unwrap();
    assert_eq!(gone, javelin::tree::empty());
    assert!(RepoPath::parse(&(deep + "/g")).is_err());
    assert!(RepoPath::parse(&"d".repeat(javelin::MAX_PATH + 1)).is_err());
}

#[test]
fn ambiguous_or_colliding_changes_are_refused() {
    let trees = trees();
    let root = build(
        &trees,
        &Snapshot::from([(path("f"), file(1, false)), (path("d/x"), file(2, false))]),
    );
    let refused = |changes: &[(&str, Option<FileEntry>)]| {
        let changes = SparseChanges::new(changes.iter().map(|(p, e)| (path(p), e.clone())));
        trees.apply(root, &changes.unwrap()).is_err()
    };
    assert!(refused(&[("f/x", Some(file(3, false)))]));
    assert!(refused(&[("d", Some(file(3, false)))]));
    assert!(refused(&[("d", None)]));
    assert!(refused(&[("n", None), ("n/x", Some(file(3, false)))]));
    // Valid replacements: a file becomes a directory and the reverse.
    assert!(!refused(&[("f", None), ("f/x", Some(file(3, false)))]));
    assert!(!refused(&[("d", Some(file(3, false))), ("d/x", None)]));
    // Deleting a missing file is a no-op.
    assert!(!refused(&[("missing", None)]));
}

#[test]
fn decoder_accepts_only_canonical_chunks() {
    let id = [7u8; 32];
    let item = |tag: u8, name: &str| {
        let mut out = vec![tag];
        out.extend((name.len() as u16).to_be_bytes());
        out.extend(name.as_bytes());
        out.extend(id);
        out
    };
    let chunk = |level: u8, items: &[Vec<u8>]| {
        let mut out = vec![level];
        for i in items {
            out.extend(i);
        }
        out
    };
    let good = chunk(0, &[item(1, "a"), item(4, "b"), item(3, "c")]);
    assert_eq!(Chunk::decode(&good).unwrap().encode(), good);
    assert!(Chunk::decode(&chunk(1, &[item(0, "a")])).is_ok());
    for bad in [
        vec![],
        chunk(0, &[item(1, "b"), item(1, "a")]),
        chunk(0, &[item(1, "a"), item(1, "a")]),
        chunk(0, &[item(0, "a")]),
        chunk(1, &[item(1, "a")]),
        chunk(0, &[item(5, "a")]),
        chunk(0, &[item(1, "")]),
        chunk(0, &[item(1, "a/b")]),
        chunk(0, &[item(1, "..")]),
        chunk(0, &[item(1, ".JAVELIN")]),
        chunk(0, &[item(1, "a:b")]),
        chunk(1, &[]),
        chunk(33, &[item(0, "a")]),
        [good.clone(), vec![0]].concat(),
        good[..good.len() - 1].to_vec(),
        chunk(
            0,
            &[[vec![1, 0, 2], vec![0xff, 0xfe], id.to_vec()].concat()],
        ),
        vec![0; javelin::tree::MAX_CHUNK + 1],
    ] {
        assert!(Chunk::decode(&bad).is_err(), "{bad:?}");
    }
}

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}

/// Captures `changes` on a new Layer at `base` and publishes it.
fn publish(repo: &mut Repository, name: &str, base: u64, changes: SparseChanges) -> PublishOutcome {
    let head = repo
        .create_layer(
            &LayerId::parse(name).unwrap(),
            &repo.layer_base(v(base)).unwrap(),
        )
        .unwrap();
    let head = match repo
        .capture_layer(&op(&format!("capture-{name}")), &head, &changes)
        .unwrap()
    {
        LayerUpdate::Applied {
            state: LayerState::Active { head },
        } => head,
        other => panic!("{other:?}"),
    };
    repo.publish_layer(&op(name), &head, &PublishAttribution::Human, |_, _| Ok(()))
        .unwrap()
        .outcome
}

#[test]
fn every_world_version_records_its_derived_root_and_check_recomputes_it() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    assert_eq!(repo.head().unwrap().tree, javelin::tree::empty());
    let mut rng = Rng::new(9);
    let blobs: Vec<_> = (0..5u8)
        .map(|i| repo.put_blob(&[b'a' + i]).unwrap())
        .collect();
    let mut snapshot = Snapshot::new();
    for step in 0..12 {
        let next = {
            let n = 1 + rng.below(5);
            random_changes(&mut rng, &snapshot, n)
        };
        // Real blobs, so capture's content validation passes.
        let next: Snapshot = next
            .into_iter()
            .map(|(p, e)| {
                let content = blobs[(e.content().as_bytes()[31] % 5) as usize];
                let e = match e {
                    FileEntry::Regular { executable, .. } => FileEntry::Regular {
                        content,
                        executable,
                    },
                    FileEntry::Symlink { .. } => FileEntry::Symlink { target: content },
                };
                (p, e)
            })
            .collect();
        let changes = flat_diff(&snapshot, &next);
        if changes.entries().is_empty() {
            continue;
        }
        let head = repo.head().unwrap().number.get();
        let PublishOutcome::Published { version } =
            publish(&mut repo, &format!("p{step}"), head, changes)
        else {
            panic!("compatible publish")
        };
        snapshot = next;
        assert_eq!(version.tree, build(&trees(), &snapshot));
        assert_eq!(
            repo.manifest_at(version.number).unwrap().entries(),
            &snapshot
        );
    }
    repo.check().unwrap();

    // Damage: a stored root that disagrees with its derivation, and a missing chunk.
    let head = repo.head().unwrap();
    drop(repo);
    let db = rusqlite::Connection::open(root.path().join(".javelin/repository.sqlite3")).unwrap();
    db.execute_batch(
        "PRAGMA foreign_keys=OFF; DROP TRIGGER versions_no_UPDATE; DROP TRIGGER objects_no_DELETE;",
    )
    .unwrap();
    db.execute(
        "UPDATE versions SET tree=(SELECT tree FROM versions WHERE number=0) WHERE number=?1",
        [head.number.get() as i64],
    )
    .unwrap();
    let repo = Repository::open(root.path()).unwrap();
    assert!(repo.check().is_err());
    assert!(repo.history().is_err());
    db.execute(
        "UPDATE versions SET tree=?1 WHERE number=?2",
        rusqlite::params![head.tree.to_string(), head.number.get() as i64],
    )
    .unwrap();
    repo.check().unwrap();
    db.execute("DELETE FROM objects WHERE id=?1", [head.tree.to_string()])
        .unwrap();
    assert!(repo.check().is_err());
    assert!(repo.history().is_err());
}
