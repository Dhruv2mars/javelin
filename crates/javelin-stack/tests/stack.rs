use javelin::{
    Error, FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, Manifest, OperationId,
    PublishAttribution, RepoPath, Repository, SparseChanges, VersionNumber,
};
use javelin_merge::{ConflictKind, MergeError, Resource};
use javelin_stack::{StackError, StackOutcome, stack_layer};
use tempfile::TempDir;

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn path(s: &str) -> RepoPath {
    RepoPath::parse(s).unwrap()
}
fn version(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}
fn file(repo: &mut Repository, bytes: &[u8]) -> FileEntry {
    FileEntry::Regular {
        content: repo.put_blob(bytes).unwrap(),
        executable: false,
    }
}
fn changes(entries: impl IntoIterator<Item = (&'static str, Option<FileEntry>)>) -> SparseChanges {
    SparseChanges::new(entries.into_iter().map(|(p, e)| (path(p), e))).unwrap()
}
fn layer(repo: &mut Repository, name: &str, base: u64) -> LayerHead {
    let base = repo.layer_base(version(base)).unwrap();
    repo.create_layer(&LayerId::parse(name).unwrap(), &base)
        .unwrap()
}
fn applied(update: LayerUpdate) -> LayerHead {
    match update {
        LayerUpdate::Applied {
            state: LayerState::Active { head },
        } => head,
        other => panic!("{other:?}"),
    }
}
fn stacked(outcome: StackOutcome) -> LayerHead {
    match outcome {
        StackOutcome::Updated(update) => applied(update),
        other => panic!("{other:?}"),
    }
}
fn capture(
    repo: &mut Repository,
    name: &str,
    head: &LayerHead,
    delta: &SparseChanges,
) -> LayerHead {
    applied(repo.capture_layer(&op(name), head, delta).unwrap())
}
fn fixture() -> (TempDir, Repository, FileEntry) {
    let root = TempDir::new().unwrap();
    let mut repo = Repository::init(root.path()).unwrap();
    let original = file(&mut repo, b"first\nanchor\nlast\n");
    let world = repo
        .put_manifest(&Manifest::new([(path("file"), original.clone())]).unwrap())
        .unwrap();
    repo.compare_and_advance(&op("world"), version(0), world)
        .unwrap();
    (root, repo, original)
}

#[test]
fn clean_within_file_and_disjoint_changes_enter_one_checkpoint_and_replay_after_reopen() {
    let (root, mut repo, _) = fixture();
    let target = layer(&mut repo, "target", 1);
    let source = layer(&mut repo, "source", 1);
    let ours = file(&mut repo, b"FIRST\nanchor\nlast\n");
    let theirs = file(&mut repo, b"first\nanchor\nLAST\n");
    let private = file(&mut repo, b"private");
    let imported = file(&mut repo, b"imported");
    let target = capture(
        &mut repo,
        "ours",
        &target,
        &changes([("file", Some(ours)), ("private", Some(private.clone()))]),
    );
    let source = capture(
        &mut repo,
        "theirs",
        &source,
        &changes([("file", Some(theirs)), ("imported", Some(imported.clone()))]),
    );
    let original_source = repo.read_checkpoint(source.checkpoint()).unwrap();
    let outcome = stack_layer(&mut repo, &op("stack"), &target, source.checkpoint()).unwrap();
    let head = stacked(outcome.clone());
    let frozen = repo.read_checkpoint(head.checkpoint()).unwrap();
    assert_eq!(
        frozen.base(),
        repo.read_checkpoint(target.checkpoint()).unwrap().base()
    );
    assert_eq!(head.revision(), target.revision() + 1);
    assert_eq!(
        repo.read_blob(frozen.manifest().entries()[&path("file")].content())
            .unwrap(),
        b"FIRST\nanchor\nLAST\n"
    );
    assert_eq!(frozen.manifest().entries()[&path("private")], private);
    assert_eq!(frozen.manifest().entries()[&path("imported")], imported);
    assert_eq!(frozen.stack_sources(), &[source.checkpoint()]);
    assert_eq!(repo.history().unwrap().last().unwrap().number, version(1));
    // Source can advance, then discard; it never changes the imported checkpoint.
    let later = file(&mut repo, b"later");
    let next = capture(
        &mut repo,
        "source-next",
        &source,
        &changes([("file", Some(later))]),
    );
    repo.discard_layer(&op("source-discard"), &next).unwrap();
    drop(repo);
    let mut repo = Repository::open(root.path()).unwrap();
    assert_eq!(
        stack_layer(&mut repo, &op("stack"), &target, source.checkpoint()).unwrap(),
        outcome
    );
    assert_eq!(repo.layer_history(target.layer()).unwrap().len(), 3);
    assert_eq!(
        repo.read_checkpoint(source.checkpoint()).unwrap(),
        original_source
    );
    assert_eq!(repo.read_checkpoint(head.checkpoint()).unwrap(), frozen);
    assert!(matches!(
        stack_layer(&mut repo, &op("stack"), &target, next.checkpoint()),
        Err(StackError::Storage(Error::OperationIdReused(_)))
    ));
    repo.check().unwrap();
}

#[test]
fn conflicts_are_explicit_exact_and_leave_no_partial_checkpoint_or_operation() {
    for kind in [
        ConflictKind::Content,
        ConflictKind::AddAdd,
        ConflictKind::ModifyDelete,
        ConflictKind::EntryType,
        ConflictKind::PathPrefix,
        ConflictKind::Binary,
    ] {
        let (_root, mut repo, _) = fixture();
        let target = layer(&mut repo, "target", 1);
        let source = layer(&mut repo, "source", 1);
        let ours = file(
            &mut repo,
            if kind == ConflictKind::Binary {
                b"ours\0"
            } else {
                b"ours"
            },
        );
        let theirs = file(
            &mut repo,
            if kind == ConflictKind::Binary {
                b"theirs\0"
            } else {
                b"theirs"
            },
        );
        let (mine, other) = match kind {
            ConflictKind::AddAdd => (
                changes([("new", Some(ours))]),
                changes([("new", Some(theirs))]),
            ),
            ConflictKind::ModifyDelete => {
                (changes([("file", None)]), changes([("file", Some(theirs))]))
            }
            ConflictKind::EntryType => {
                let symlink = FileEntry::Symlink {
                    target: repo.put_blob(b"destination").unwrap(),
                };
                (
                    changes([("file", Some(symlink))]),
                    changes([("file", Some(theirs))]),
                )
            }
            ConflictKind::PathPrefix => (
                changes([("new", Some(ours))]),
                changes([("new/child", Some(theirs))]),
            ),
            _ => (
                changes([("file", Some(ours))]),
                changes([("file", Some(theirs))]),
            ),
        };
        let target = capture(&mut repo, "ours", &target, &mine);
        let source = capture(&mut repo, "theirs", &source, &other);
        let before = repo.check().unwrap().objects;
        let outcome = stack_layer(&mut repo, &op("stack"), &target, source.checkpoint()).unwrap();
        let StackOutcome::Conflicts(conflicts) = outcome else {
            panic!("{kind:?}: {outcome:?}")
        };
        assert!(conflicts.iter().any(|c| c.kind == kind), "{conflicts:?}");
        let target_entries = repo.read_checkpoint(target.checkpoint()).unwrap();
        let source_entries = repo.read_checkpoint(source.checkpoint()).unwrap();
        for c in &conflicts {
            assert_eq!(
                c.ours.as_ref(),
                target_entries.manifest().entries().get(&c.path)
            );
            assert_eq!(
                c.theirs.as_ref(),
                source_entries.manifest().entries().get(&c.path)
            );
        }
        assert_eq!(repo.check().unwrap().objects, before);
        assert_eq!(
            repo.layer_state(target.layer()).unwrap(),
            LayerState::Active {
                head: target.clone()
            }
        );
        assert_eq!(
            repo.stack_operation(&op("stack"), &target, source.checkpoint())
                .unwrap(),
            None
        );
    }
}

#[test]
fn different_bases_import_only_source_delta_and_preserve_private_and_accepted_content() {
    for newer_source in [false, true] {
        let (_root, mut repo, original) = fixture();
        let unrelated = file(&mut repo, b"World 2 content");
        let world = repo
            .put_manifest(
                &Manifest::new([
                    (path("file"), original),
                    (path("unrelated"), unrelated.clone()),
                ])
                .unwrap(),
            )
            .unwrap();
        repo.compare_and_advance(&op("world2"), version(1), world)
            .unwrap();
        let target = layer(&mut repo, "target", if newer_source { 1 } else { 2 });
        let source = layer(&mut repo, "source", if newer_source { 2 } else { 1 });
        let private = file(&mut repo, b"private");
        let target = capture(
            &mut repo,
            "ours",
            &target,
            &changes([("private", Some(private.clone()))]),
        );
        let change = file(&mut repo, b"source delta");
        let source = capture(
            &mut repo,
            "theirs",
            &source,
            &changes([("file", Some(change.clone()))]),
        );
        let head =
            stacked(stack_layer(&mut repo, &op("stack"), &target, source.checkpoint()).unwrap());
        let result = repo.read_checkpoint(head.checkpoint()).unwrap();
        assert_eq!(result.manifest().entries()[&path("file")], change);
        assert_eq!(result.manifest().entries()[&path("private")], private);
        assert_eq!(
            result.manifest().entries().get(&path("unrelated")),
            if newer_source { None } else { Some(&unrelated) }
        );
        assert_eq!(
            result.base().version(),
            if newer_source { version(1) } else { version(2) }
        );
        assert_eq!(repo.history().unwrap().last().unwrap().number, version(2));
        repo.check().unwrap();
    }
}

#[test]
fn resource_and_bounded_loader_refusals_write_nothing() {
    for too_large_blob in [false, true] {
        let (_root, mut repo, _) = fixture();
        let target = layer(&mut repo, "target", 1);
        let source = layer(&mut repo, "source", 1);
        let bytes = if too_large_blob {
            vec![b'x'; Resource::BlobBytes.limit() as usize + 1]
        } else {
            b"line\n".repeat(Resource::Lines.limit() as usize + 1)
        };
        let ours = file(&mut repo, &bytes);
        let theirs = file(&mut repo, b"different");
        let target = capture(&mut repo, "ours", &target, &changes([("file", Some(ours))]));
        let source = capture(
            &mut repo,
            "theirs",
            &source,
            &changes([("file", Some(theirs))]),
        );
        let before = repo.check().unwrap().objects;
        let error = stack_layer(&mut repo, &op("stack"), &target, source.checkpoint()).unwrap_err();
        if too_large_blob {
            assert!(
                matches!(
                    error,
                    StackError::Merge(MergeError::Load(Error::ObjectTooLarge { .. }))
                ),
                "{error}"
            );
        } else {
            assert!(
                matches!(error,StackError::Merge(MergeError::Resource(limit)) if limit.resource==Resource::Lines),
                "{error}"
            );
        }
        assert_eq!(repo.check().unwrap().objects, before);
        assert_eq!(
            repo.layer_state(target.layer()).unwrap(),
            LayerState::Active {
                head: target.clone()
            }
        );
        assert_eq!(
            repo.stack_operation(&op("stack"), &target, source.checkpoint())
                .unwrap(),
            None
        );
    }
}

#[test]
fn clean_deletions_types_modes_and_published_source_remain_frozen_transitively() {
    let (_root, mut repo, _) = fixture();
    let target = layer(&mut repo, "target", 1);
    let source = layer(&mut repo, "source", 1);
    let entry = FileEntry::Regular {
        content: repo.put_blob(b"executable").unwrap(),
        executable: true,
    };
    let link = FileEntry::Symlink {
        target: repo.put_blob(b"exe").unwrap(),
    };
    let source = capture(
        &mut repo,
        "theirs",
        &source,
        &changes([
            ("file", None),
            ("exe", Some(entry.clone())),
            ("link", Some(link.clone())),
        ]),
    );
    let frozen = repo.read_checkpoint(source.checkpoint()).unwrap();
    repo.publish_layer(
        &op("publish-source"),
        &source,
        &PublishAttribution::Human,
        |_, _| Ok(()),
    )
    .unwrap();
    let head = stacked(stack_layer(&mut repo, &op("stack"), &target, source.checkpoint()).unwrap());
    let result = repo.read_checkpoint(head.checkpoint()).unwrap();
    assert_eq!(result.manifest(), frozen.manifest());
    assert_eq!(result.manifest().entries()[&path("exe")], entry);
    assert_eq!(result.manifest().entries()[&path("link")], link);
    let third = layer(&mut repo, "third", 1);
    let third =
        stacked(stack_layer(&mut repo, &op("stack-again"), &third, head.checkpoint()).unwrap());
    let third = repo.read_checkpoint(third.checkpoint()).unwrap();
    assert_eq!(third.stack_sources(), &[head.checkpoint()]);
    assert_eq!(
        repo.read_checkpoint(head.checkpoint())
            .unwrap()
            .stack_sources(),
        &[source.checkpoint()]
    );
    assert_eq!(repo.history().unwrap().last().unwrap().number, version(2));
    repo.check().unwrap();
}
