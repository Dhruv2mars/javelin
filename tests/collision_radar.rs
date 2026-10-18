//! Collision radar: reports exactly the overlaps that would make Publish conflict.
use std::collections::BTreeSet;

use javelin::{
    FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, OperationId, PathPair,
    PublishAttribution, PublishOutcome, Radar, RepoPath, Repository, SparseChanges, VersionNumber,
};
use tempfile::TempDir;

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn id(s: &str) -> LayerId {
    LayerId::parse(s).unwrap()
}
fn path(s: &str) -> RepoPath {
    RepoPath::parse(s).unwrap()
}
fn file(repo: &mut Repository, bytes: &[u8]) -> Option<FileEntry> {
    Some(FileEntry::Regular {
        content: repo.put_blob(bytes).unwrap(),
        executable: false,
    })
}
fn delta(entries: Vec<(&str, Option<FileEntry>)>) -> SparseChanges {
    SparseChanges::new(entries.into_iter().map(|(p, e)| (path(p), e))).unwrap()
}
fn active(update: LayerUpdate) -> LayerHead {
    match update {
        LayerUpdate::Applied {
            state: LayerState::Active { head },
        } => head,
        other => panic!("expected applied head: {other:?}"),
    }
}
fn head_number(repo: &Repository) -> u64 {
    repo.head().unwrap().number.get()
}
/// Creates Layer `name` on the current World and captures `changes`.
fn layer(repo: &mut Repository, name: &str, changes: SparseChanges) -> LayerHead {
    let base = repo
        .layer_base(VersionNumber::new(head_number(repo)).unwrap())
        .unwrap();
    let empty = repo.create_layer(&id(name), &base).unwrap();
    active(
        repo.capture_layer(&op(&format!("capture-{name}")), &empty, &changes)
            .unwrap(),
    )
}
fn publish(repo: &mut Repository, operation: &str, head: &LayerHead) -> PublishOutcome {
    repo.publish_layer(&op(operation), head, &PublishAttribution::Human, |_, _| {
        Ok(())
    })
    .unwrap()
    .outcome
}
/// Publishes a new Layer with `changes` straight onto the World.
fn advance(repo: &mut Repository, name: &str, changes: SparseChanges) {
    let head = layer(repo, name, changes);
    assert!(matches!(
        publish(repo, &format!("publish-{name}"), &head),
        PublishOutcome::Published { .. }
    ));
}
fn pairs(radar: &Radar, other: &str) -> Vec<(String, String)> {
    radar
        .layers
        .iter()
        .filter(|c| c.layer.as_str() == other)
        .flat_map(|c| &c.paths)
        .map(|PathPair { ours, theirs }| (ours.as_str().into(), theirs.as_str().into()))
        .collect()
}
fn names(radar: &Radar) -> Vec<&str> {
    radar.layers.iter().map(|c| c.layer.as_str()).collect()
}
fn indexed(root: &TempDir) -> i64 {
    rusqlite::Connection::open(root.path().join(".javelin/repository.sqlite3"))
        .unwrap()
        .query_row("SELECT count(*) FROM layer_paths", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn reports_exact_ancestor_and_descendant_overlaps_and_nothing_else() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let a = file(&mut repo, b"a");
    let changes = delta(vec![
        ("src/a.rs", a.clone()),
        ("src/b.rs", a.clone()),
        ("lib/x/y", a.clone()),
        ("ab", a.clone()),
    ]);
    advance(&mut repo, "seed", changes);
    let (one, two) = (file(&mut repo, b"1"), file(&mut repo, b"2"));

    // `src` becomes a file: an ancestor of every path below it.
    let ours = layer(
        &mut repo,
        "ours",
        delta(vec![
            ("src/a.rs", None),
            ("src/b.rs", None),
            ("src", one.clone()),
            ("lib/x/y", one.clone()),
        ]),
    );
    layer(&mut repo, "exact", delta(vec![("lib/x/y", two.clone())]));
    layer(&mut repo, "below", delta(vec![("src/new.rs", two.clone())]));
    // `lib/x` replaces a directory with a file: an ancestor of our `lib/x/y`.
    layer(
        &mut repo,
        "above",
        delta(vec![("lib/x/y", None), ("lib/x", two.clone())]),
    );
    // Same directory, sibling, name-prefix and unrelated paths never collide.
    layer(&mut repo, "sibling", delta(vec![("lib/q", two.clone())]));
    layer(
        &mut repo,
        "prefix",
        delta(vec![("src2/a.rs", two.clone()), ("lib/xy", two.clone())]),
    );
    layer(&mut repo, "unrelated", delta(vec![("ab", two.clone())]));

    let radar = repo.radar(ours.layer()).unwrap();
    assert!(radar.world.paths.is_empty());
    assert_eq!(names(&radar), ["above", "below", "exact"]);
    assert_eq!(
        pairs(&radar, "exact"),
        [("lib/x/y".to_string(), "lib/x/y".to_string())]
    );
    assert_eq!(
        pairs(&radar, "below"),
        [("src".into(), "src/new.rs".into())]
    );
    assert_eq!(
        pairs(&radar, "above"),
        [
            ("lib/x/y".to_string(), "lib/x".to_string()),
            ("lib/x/y".to_string(), "lib/x/y".to_string())
        ]
    );
    // The relation is symmetric for Layers on the same base.
    let below = repo.radar(&id("below")).unwrap();
    assert_eq!(names(&below), ["ours"]);
    assert_eq!(pairs(&below, "ours"), [("src/new.rs".into(), "src".into())]);
    assert_eq!(
        names(&repo.radar(&id("sibling")).unwrap()),
        Vec::<&str>::new()
    );
    assert!(repo.radar(&id("prefix")).unwrap().clear());
    repo.check().unwrap();
}

#[test]
fn world_collisions_match_publish_and_ignore_reverted_advances() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let (a, b) = (file(&mut repo, b"a"), file(&mut repo, b"b"));
    advance(
        &mut repo,
        "seed",
        delta(vec![("f", a.clone()), ("g", a.clone())]),
    );
    let ours = layer(
        &mut repo,
        "ours",
        delta(vec![("f", b.clone()), ("g", b.clone())]),
    );
    // `g` changes and changes back: end to end it is unchanged.
    advance(&mut repo, "w1", delta(vec![("f", None), ("g", b.clone())]));
    advance(&mut repo, "w2", delta(vec![("g", a.clone())]));
    let radar = repo.radar(ours.layer()).unwrap();
    assert_eq!(radar.base.get(), 1);
    assert_eq!(radar.world.version.get(), 3);
    assert_eq!(radar.world.paths, [path("f")]);
    assert!(radar.layers.is_empty(), "published Layers are not live");
    let PublishOutcome::Conflict { paths } = publish(&mut repo, "ours", &ours) else {
        panic!("expected conflict")
    };
    assert_eq!(paths, radar.world.paths);
}

#[test]
fn another_layers_change_back_to_our_base_is_not_a_collision() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let (a, b, c) = (
        file(&mut repo, b"a"),
        file(&mut repo, b"b"),
        file(&mut repo, b"c"),
    );
    advance(&mut repo, "seed", delta(vec![("f", a.clone())]));
    let ours = layer(&mut repo, "ours", delta(vec![("f", c.clone())]));
    advance(&mut repo, "w", delta(vec![("f", b.clone())]));
    // Their change sets `f` back to what our base had.
    let theirs = layer(&mut repo, "theirs", delta(vec![("f", a.clone())]));
    let radar = repo.radar(ours.layer()).unwrap();
    assert_eq!(radar.world.paths, [path("f")]);
    assert!(radar.layers.is_empty());
    // Our base still sees `b` for them, so they would conflict with us.
    assert_eq!(names(&repo.radar(theirs.layer()).unwrap()), ["ours"]);
}

#[test]
fn index_follows_capture_clone_stack_discard_and_publish() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let (a, b) = (file(&mut repo, b"a"), file(&mut repo, b"b"));
    let first = layer(
        &mut repo,
        "first",
        delta(vec![("x", a.clone()), ("y", a.clone())]),
    );
    assert_eq!(indexed(&root), 2);
    // A later capture moves the Layer's entries: `y` dropped, `z` added.
    let latest = active(
        repo.capture_layer(
            &op("recapture"),
            &first,
            &delta(vec![("x", a.clone()), ("z", b.clone())]),
        )
        .unwrap(),
    );
    assert_eq!(indexed(&root), 2);
    // A stale capture changes nothing.
    assert!(matches!(
        repo.capture_layer(&op("late"), &first, &delta(vec![]))
            .unwrap(),
        LayerUpdate::Stale { .. }
    ));
    assert_eq!(indexed(&root), 2);
    let old = latest;

    let clone = repo.clone_layer(&id("clone"), old.checkpoint()).unwrap();
    assert_eq!(indexed(&root), 4);
    assert_eq!(
        pairs(&repo.radar(clone.layer()).unwrap(), "first"),
        [("x".into(), "x".into()), ("z".into(), "z".into())]
    );
    let target = layer(&mut repo, "target", delta(vec![("w", b.clone())]));
    javelin_stack_like(&mut repo, &target, old.checkpoint());
    assert_eq!(indexed(&root), 7);
    assert_eq!(
        names(&repo.radar(&id("target")).unwrap()),
        ["clone", "first"]
    );
    repo.check().unwrap();

    assert!(matches!(
        publish(&mut repo, "publish-first", &old),
        PublishOutcome::Published { .. }
    ));
    assert_eq!(names(&repo.radar(&id("clone")).unwrap()), ["target"]);
    let clone_head = match repo.layer_state(&id("clone")).unwrap() {
        LayerState::Active { head } => head,
        _ => panic!(),
    };
    repo.discard_layer(&op("discard-clone"), &clone_head)
        .unwrap();
    // target: w, x, z remain; first and clone are gone.
    assert_eq!(indexed(&root), 3);
    assert!(repo.radar(&id("target")).unwrap().layers.is_empty());
    assert!(repo.radar(&id("clone")).is_err());
    repo.check().unwrap();
}

/// Stacks `source` into `target` the way javelin-stack does for disjoint changes.
fn javelin_stack_like(repo: &mut Repository, target: &LayerHead, source: javelin::CheckpointId) {
    let mut merged: Vec<_> = repo
        .read_checkpoint(target.checkpoint())
        .unwrap()
        .changes()
        .entries()
        .clone()
        .into_iter()
        .collect();
    merged.extend(
        repo.read_checkpoint(source)
            .unwrap()
            .changes()
            .entries()
            .clone(),
    );
    let changes = SparseChanges::new(merged).unwrap();
    active(
        repo.commit_stack(&op("stack"), target, source, &changes)
            .unwrap(),
    );
}

#[test]
fn check_detects_a_damaged_index() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let a = file(&mut repo, b"a");
    layer(&mut repo, "l", delta(vec![("x", a)]));
    repo.check().unwrap();
    let raw = rusqlite::Connection::open(root.path().join(".javelin/repository.sqlite3")).unwrap();
    raw.execute("DELETE FROM layer_paths", []).unwrap();
    assert_eq!(repo.check().unwrap_err().code(), "corrupt_repository");
    raw.execute(
        "INSERT INTO layer_paths(path,layer,kind,content,checkpoint) \
         SELECT 'y','ghost',NULL,NULL,checkpoint FROM layer_heads LIMIT 1",
        [],
    )
    .unwrap();
    assert_eq!(repo.check().unwrap_err().code(), "corrupt_repository");
}

/// Model check: for every pair of live Layers, the radar predicts exactly whether
/// publishing the other first makes ours conflict, and the World part equals the
/// paths Publish reports now. Each prediction runs Publish on a copy of the store.
#[test]
fn radar_predicts_publish_outcomes() {
    const PATHS: [&str; 7] = ["a", "a/b", "a/b/c", "a/c", "ab", "b", "b/a"];
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let (mut checked, mut collided) = (0, 0);
    for round in 0..12 {
        let root = TempDir::new().unwrap();
        let mut repo = Repository::init(root.path()).unwrap();
        let contents: Vec<_> = (0..3)
            .map(|i| file(&mut repo, format!("c{i}").as_bytes()))
            .collect();
        let random_changes = |next: &mut dyn FnMut() -> u64| {
            let mut entries = Vec::new();
            for p in PATHS {
                if next().is_multiple_of(3) {
                    let entry = match next() % 4 {
                        0 => None,
                        n => contents[n as usize - 1].clone(),
                    };
                    entries.push((p, entry));
                }
            }
            delta(entries)
        };
        let mut live = Vec::new();
        for n in 0..10 {
            let changes = random_changes(&mut next);
            let name = format!("l{n}");
            let base = repo
                .layer_base(VersionNumber::new(head_number(&repo)).unwrap())
                .unwrap();
            let empty = repo.create_layer(&id(&name), &base).unwrap();
            // Invalid random deltas (file/descendant collisions) are skipped.
            let Ok(update) = repo.capture_layer(&op(&format!("c-{name}")), &empty, &changes) else {
                continue;
            };
            let head = active(update);
            if next().is_multiple_of(4)
                && let PublishOutcome::Published { .. } =
                    publish(&mut repo, &format!("p-{name}"), &head)
            {
                continue;
            }
            live.push(head);
        }
        repo.check().unwrap();
        for ours in &live {
            let radar = repo.radar(ours.layer()).unwrap();
            let now = in_copy(&root, |copy| publish(copy, "now", ours));
            match now {
                PublishOutcome::Conflict { paths } => assert_eq!(paths, radar.world.paths),
                _ => assert!(radar.world.paths.is_empty(), "round {round}"),
            }
            // With no World collision now, any later conflict comes from theirs.
            if !radar.world.paths.is_empty() {
                continue;
            }
            let reported: BTreeSet<_> = names(&radar).into_iter().collect();
            for theirs in live.iter().filter(|t| t.layer() != ours.layer()) {
                let outcome = in_copy(&root, |copy| match publish(copy, "theirs", theirs) {
                    PublishOutcome::Published { .. } => Some(publish(copy, "ours", ours)),
                    _ => None,
                });
                let Some(outcome) = outcome else { continue };
                assert_eq!(
                    matches!(outcome, PublishOutcome::Conflict { .. }),
                    reported.contains(theirs.layer().as_str()),
                    "round {round}: {} then {}",
                    theirs.layer().as_str(),
                    ours.layer().as_str()
                );
                checked += 1;
                collided += usize::from(reported.contains(theirs.layer().as_str()));
            }
        }
    }
    println!("{checked} predictions, {collided} collisions");
    assert!(
        collided >= 30 && checked - collided >= 30,
        "{checked} checked, {collided} collided"
    );
}

/// Runs `f` on a private copy of the store, leaving the original untouched.
fn in_copy<T>(root: &TempDir, f: impl FnOnce(&mut Repository) -> T) -> T {
    let copy = TempDir::new().unwrap();
    std::fs::create_dir(copy.path().join(".javelin")).unwrap();
    std::fs::copy(
        root.path().join(".javelin/repository.sqlite3"),
        copy.path().join(".javelin/repository.sqlite3"),
    )
    .unwrap();
    f(&mut Repository::open(copy.path()).unwrap())
}
