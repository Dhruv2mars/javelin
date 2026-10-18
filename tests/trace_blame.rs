//! Trace blame: checkpoint provenance flows through Publish into World history.
use javelin::{
    Actor, FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, LineBlame, Manifest,
    OperationId, Provenance, PublishAttribution, PublishOutcome, RepoPath, Repository,
    SparseChanges, VersionNumber,
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
fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}
fn agent(session: &str) -> Option<Provenance> {
    Some(
        Provenance::new(
            Actor::Agent,
            Some("claude-code".into()),
            Some(session.into()),
            Some(format!("tool-{session}")),
        )
        .unwrap(),
    )
}
fn file(repo: &mut Repository, text: &str) -> Option<FileEntry> {
    Some(FileEntry::Regular {
        content: repo.put_blob(text.as_bytes()).unwrap(),
        executable: false,
    })
}
fn delta(repo: &mut Repository, entries: &[(&str, Option<&str>)]) -> SparseChanges {
    let entries: Vec<_> = entries
        .iter()
        .map(|(p, text)| (path(p), text.and_then(|t| file(repo, t))))
        .collect();
    SparseChanges::new(entries).unwrap()
}
fn active(update: LayerUpdate) -> LayerHead {
    match update {
        LayerUpdate::Applied {
            state: LayerState::Active { head },
        } => head,
        other => panic!("expected applied head: {other:?}"),
    }
}
fn create(repo: &mut Repository, name: &str) -> LayerHead {
    let head = repo.head().unwrap().number;
    let base = repo.layer_base(head).unwrap();
    repo.create_layer(&id(name), &base).unwrap()
}
/// Captures `entries` on top of the Layer's current changes, as `who`.
fn capture(
    repo: &mut Repository,
    head: &LayerHead,
    who: Option<Provenance>,
    entries: &[(&str, Option<&str>)],
) -> LayerHead {
    let mut changes = repo
        .read_checkpoint(head.checkpoint())
        .unwrap()
        .changes()
        .entries()
        .clone();
    changes.extend(delta(repo, entries).entries().clone());
    repo.set_provenance(who);
    let operation = op(&format!(
        "capture-{}-{}",
        head.layer().as_str(),
        head.revision()
    ));
    let next = active(
        repo.capture_layer(&operation, head, &SparseChanges::new(changes).unwrap())
            .unwrap(),
    );
    repo.set_provenance(None);
    next
}
fn publish(repo: &mut Repository, head: &LayerHead) -> PublishOutcome {
    repo.publish_layer(
        &op(&format!("publish-{}", head.layer().as_str())),
        head,
        &PublishAttribution::Human,
        |_, _| Ok(()),
    )
    .unwrap()
    .outcome
}
fn published(repo: &mut Repository, head: &LayerHead) -> u64 {
    match publish(repo, head) {
        PublishOutcome::Published { version } => version.number.get(),
        other => panic!("expected publication: {other:?}"),
    }
}
/// (World version, session) per line.
fn lines(blame: &LineBlame) -> Vec<(u64, Option<&str>, &str)> {
    blame
        .lines
        .iter()
        .map(|line| {
            let origin = &blame.origins[line.origin];
            let session = origin
                .provenance
                .as_ref()
                .and_then(|p| p.session.as_deref());
            (origin.version.number.get(), session, line.text.as_str())
        })
        .collect()
}
fn session(repo: &Repository, p: &str) -> (u64, Option<String>) {
    let blame = repo.blame(&path(p), None).unwrap().unwrap();
    (
        blame.origin.version.number.get(),
        blame.origin.provenance.and_then(|p| p.session),
    )
}

#[test]
fn attribution_follows_each_checkpoint_through_multi_layer_publishes() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let one = create(&mut repo, "one");
    let one = capture(
        &mut repo,
        &one,
        agent("s1"),
        &[("f", Some("a\nb\nc\n")), ("g", Some("x\n"))],
    );
    assert_eq!(published(&mut repo, &one), 1);

    // Two sessions share Layer two: s2 edits f, then s3 edits only g.
    let two = create(&mut repo, "two");
    let first = capture(&mut repo, &two, agent("s2"), &[("f", Some("a\nB\nc\n"))]);
    let second = capture(&mut repo, &first, agent("s3"), &[("g", Some("y\n"))]);
    assert_eq!(published(&mut repo, &second), 2);

    let human = Provenance::new(Actor::Human, None, Some("h".into()), None).unwrap();
    let three = create(&mut repo, "three");
    let three = capture(
        &mut repo,
        &three,
        Some(human.clone()),
        &[("h", Some("1\n"))],
    );
    assert_eq!(published(&mut repo, &three), 3);

    let f = repo.blame(&path("f"), None).unwrap().unwrap();
    assert_eq!(f.origin.version.number.get(), 2);
    assert_eq!(f.origin.layer.as_ref().unwrap().as_str(), "two");
    assert_eq!(f.origin.checkpoint, Some(first.checkpoint()));
    assert_eq!(f.origin.provenance, agent("s2"));
    assert_eq!(session(&repo, "g"), (2, Some("s3".into())));
    let h = repo.blame(&path("h"), None).unwrap().unwrap();
    assert_eq!(h.origin.provenance, Some(human));
    assert_eq!(h.origin.checkpoint, Some(three.checkpoint()));
    // At an earlier World, the earlier change.
    let early = repo.blame(&path("f"), Some(v(1))).unwrap().unwrap();
    assert_eq!(early.origin.provenance, agent("s1"));
    assert!(repo.blame(&path("missing"), None).unwrap().is_none());

    let blame = repo.blame_lines(&path("f"), None).unwrap().unwrap();
    assert_eq!(
        lines(&blame),
        [
            (1, Some("s1"), "a"),
            (2, Some("s2"), "B"),
            (1, Some("s1"), "c")
        ]
    );
    assert_eq!(blame.origins.len(), 2);
    repo.check().unwrap();
}

#[test]
fn a_rename_is_a_delete_plus_an_add() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let one = create(&mut repo, "one");
    let one = capture(&mut repo, &one, agent("s1"), &[("old", Some("k\nl\n"))]);
    published(&mut repo, &one);
    let two = create(&mut repo, "two");
    let two = capture(
        &mut repo,
        &two,
        agent("s2"),
        &[("old", None), ("new", Some("k\nl\n"))],
    );
    published(&mut repo, &two);
    // The renamed file's lines start at the renaming version; history does not
    // follow content across paths.
    let new = repo.blame_lines(&path("new"), None).unwrap().unwrap();
    assert_eq!(lines(&new), [(2, Some("s2"), "k"), (2, Some("s2"), "l")]);
    let old = repo.blame(&path("old"), None).unwrap().unwrap();
    assert!(old.entry.is_none(), "deleted");
    assert_eq!(old.origin.provenance, agent("s2"));
    assert!(repo.blame_lines(&path("old"), None).unwrap().is_none());
    // Recreating the old path starts its line history again.
    let three = create(&mut repo, "three");
    let three = capture(&mut repo, &three, agent("s3"), &[("old", Some("k\n"))]);
    published(&mut repo, &three);
    let old = repo.blame_lines(&path("old"), None).unwrap().unwrap();
    assert_eq!(lines(&old), [(3, Some("s3"), "k")]);
    repo.check().unwrap();
}

#[test]
fn blame_is_correct_after_a_conflict_is_resolved() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let base = create(&mut repo, "base");
    let base = capture(&mut repo, &base, agent("s0"), &[("f", Some("1\n2\n3\n"))]);
    published(&mut repo, &base);
    let left = create(&mut repo, "left");
    let right = create(&mut repo, "right");
    let left = capture(&mut repo, &left, agent("s1"), &[("f", Some("1\nA\n3\n"))]);
    let right = capture(&mut repo, &right, agent("s2"), &[("f", Some("1\n2\nB\n"))]);
    assert_eq!(published(&mut repo, &left), 2);
    let PublishOutcome::Conflict { paths } = publish(&mut repo, &right) else {
        panic!("expected conflict")
    };
    assert_eq!(paths, [path("f")]);
    // A conflict records nothing; s2's work is not in the World.
    assert_eq!(session(&repo, "f"), (2, Some("s1".into())));
    // The owner resolves in a fresh Layer on the new head.
    let resolved = create(&mut repo, "resolved");
    let resolved = capture(
        &mut repo,
        &resolved,
        agent("s3"),
        &[("f", Some("1\nA\nB\n"))],
    );
    assert_eq!(published(&mut repo, &resolved), 3);
    let blame = repo.blame_lines(&path("f"), None).unwrap().unwrap();
    assert_eq!(
        lines(&blame),
        [
            (1, Some("s0"), "1"),
            (2, Some("s1"), "A"),
            (3, Some("s3"), "B")
        ]
    );
    let stale = match repo.layer_state(&id("right")).unwrap() {
        LayerState::Active { head } => head,
        other => panic!("{other:?}"),
    };
    repo.discard_layer(&op("drop-right"), &stale).unwrap();
    repo.check().unwrap();
}

#[test]
fn clone_keeps_the_source_attribution_and_stack_attributes_to_its_session() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let source = create(&mut repo, "source");
    let source = capture(&mut repo, &source, agent("s1"), &[("a", Some("a\n"))]);
    let clone = repo.clone_layer(&id("clone"), source.checkpoint()).unwrap();
    let clone = capture(&mut repo, &clone, agent("s2"), &[("b", Some("b\n"))]);
    assert_eq!(published(&mut repo, &clone), 1);
    assert_eq!(session(&repo, "a"), (1, Some("s1".into())));
    assert_eq!(session(&repo, "b"), (1, Some("s2".into())));

    // Stack imports a source checkpoint's changes into the target as one new
    // checkpoint, attributed to whoever ran Stack.
    let target = create(&mut repo, "target");
    let donor = create(&mut repo, "donor");
    let donor = capture(&mut repo, &donor, agent("s3"), &[("c", Some("c\n"))]);
    let changes = repo
        .read_checkpoint(donor.checkpoint())
        .unwrap()
        .changes()
        .clone();
    repo.set_provenance(agent("stacker"));
    let target = active(
        repo.commit_stack(&op("stack"), &target, donor.checkpoint(), &changes)
            .unwrap(),
    );
    repo.set_provenance(None);
    assert_eq!(published(&mut repo, &target), 2);
    assert_eq!(session(&repo, "c"), (2, Some("stacker".into())));
    repo.check().unwrap();
}

#[test]
fn unattributed_work_and_layerless_versions_report_honestly() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let plain = create(&mut repo, "plain");
    let plain = capture(&mut repo, &plain, None, &[("a", Some("a\n"))]);
    assert_eq!(
        repo.read_checkpoint(plain.checkpoint())
            .unwrap()
            .provenance(),
        None
    );
    published(&mut repo, &plain);
    let a = repo.blame(&path("a"), None).unwrap().unwrap();
    assert_eq!(a.origin.checkpoint, Some(plain.checkpoint()));
    assert!(a.origin.provenance.is_none());

    let content = repo.put_blob(b"z\n").unwrap();
    let mut entries = repo.manifest_at(v(1)).unwrap().entries().clone();
    entries.insert(
        path("z"),
        FileEntry::Regular {
            content,
            executable: false,
        },
    );
    let manifest = repo.put_manifest(&Manifest::new(entries).unwrap()).unwrap();
    repo.compare_and_advance(&op("raw"), v(1), manifest)
        .unwrap();
    let z = repo.blame(&path("z"), None).unwrap().unwrap();
    assert_eq!(z.origin.version.number.get(), 2);
    assert!(z.origin.layer.is_none() && z.origin.checkpoint.is_none());
    repo.check().unwrap();
}

#[test]
fn provenance_is_validated_and_bound_into_checkpoints() {
    assert!(Provenance::new(Actor::Human, Some("bot".into()), None, None).is_err());
    assert!(Provenance::new(Actor::Agent, Some(String::new()), None, None).is_err());
    assert!(Provenance::new(Actor::Agent, None, Some("a\nb".into()), None).is_err());
    assert!(Provenance::new(Actor::Agent, None, Some("x".repeat(257)), None).is_err());
    assert!(Provenance::new(Actor::Agent, None, Some("x".repeat(256)), None).is_ok());

    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let layer = create(&mut repo, "l");
    let changes = delta(&mut repo, &[("a", Some("a"))]);
    // The same changes for two sessions are different checkpoints.
    repo.set_provenance(agent("s1"));
    let first = repo.capture_layer(&op("c1"), &layer, &changes).unwrap();
    repo.set_provenance(agent("s2"));
    let LayerUpdate::Stale { .. } = repo.capture_layer(&op("c2"), &layer, &changes).unwrap() else {
        panic!("the head moved")
    };
    let head = active(first);
    let frozen = repo.read_checkpoint(head.checkpoint()).unwrap();
    let blob = frozen.provenance().unwrap();
    assert_eq!(
        serde_json::from_slice::<Provenance>(&repo.read_blob(blob).unwrap()).unwrap(),
        agent("s1").unwrap()
    );
}

#[test]
fn check_rejects_tampered_attribution() {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let one = create(&mut repo, "one");
    let one = capture(&mut repo, &one, agent("s1"), &[("a", Some("a\n"))]);
    published(&mut repo, &one);
    let two = create(&mut repo, "two");
    capture(&mut repo, &two, agent("s2"), &[("b", Some("b\n"))]);
    repo.check().unwrap();
    let raw = rusqlite::Connection::open(root.path().join(".javelin/repository.sqlite3")).unwrap();
    // Live attribution: point b at another checkpoint.
    raw.execute(
        "UPDATE layer_paths SET checkpoint=(SELECT checkpoint FROM blame LIMIT 1)",
        [],
    )
    .unwrap();
    assert_eq!(repo.check().unwrap_err().code(), "corrupt_repository");
    // World attribution is immutable, and an extra row is refused by check.
    assert!(raw.execute("DELETE FROM blame", []).is_err());
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let one = create(&mut repo, "one");
    let one = capture(&mut repo, &one, agent("s1"), &[("a", Some("a\n"))]);
    published(&mut repo, &one);
    let raw = rusqlite::Connection::open(root.path().join(".javelin/repository.sqlite3")).unwrap();
    raw.execute(
        "INSERT INTO blame(path,version,checkpoint,provenance) \
         SELECT 'b',version,checkpoint,provenance FROM blame",
        [],
    )
    .unwrap();
    assert_eq!(repo.check().unwrap_err().code(), "corrupt_repository");
}
