//! Every case merges real World and Layer checkpoint manifests from a native repository.

use std::cell::RefCell;

use javelin::{
    FileEntry, LayerId, LayerState, LayerUpdate, Manifest, ObjectId, OperationId, RepoPath,
    Repository, SparseChanges, VersionNumber,
};
use javelin_merge::{Conflict, ConflictKind, MergeOutcome, Merged, merge};
use tempfile::TempDir;

#[derive(Clone, Copy)]
enum Spec<'a> {
    File(&'a [u8]),
    Exec(&'a [u8]),
    Link(&'a [u8]),
}
use Spec::{Exec, File, Link};

fn path(s: &str) -> RepoPath {
    RepoPath::parse(s).unwrap()
}

struct Fixture {
    _dir: TempDir,
    repo: Repository,
    base: Manifest,
    layers: usize,
}

impl Fixture {
    /// Publishes `files` as World 1, the base every Layer in the case starts from.
    fn new(files: &[(&str, Spec)]) -> Self {
        let dir = TempDir::new().unwrap();
        let mut repo = Repository::init(dir.path()).unwrap();
        let entries: Vec<_> = files
            .iter()
            .map(|(p, s)| (path(p), entry(&mut repo, *s)))
            .collect();
        let id = repo.put_manifest(&Manifest::new(entries).unwrap()).unwrap();
        let op = OperationId::parse("world-1").unwrap();
        repo.compare_and_advance(&op, VersionNumber::new(0).unwrap(), id)
            .unwrap();
        let base = repo.read_manifest(id).unwrap();
        Self {
            _dir: dir,
            repo,
            base,
            layers: 0,
        }
    }

    /// Captures `changes` in a new Layer over World 1 and returns its checkpoint manifest.
    fn layer(&mut self, changes: &[(&str, Option<Spec>)]) -> Manifest {
        self.layers += 1;
        let repo = &mut self.repo;
        let base = repo.layer_base(VersionNumber::new(1).unwrap()).unwrap();
        let id = LayerId::parse(&format!("layer-{}", self.layers)).unwrap();
        let head = repo.create_layer(&id, &base).unwrap();
        let changes: Vec<_> = changes
            .iter()
            .map(|(p, s)| (path(p), s.map(|s| entry(repo, s))))
            .collect();
        let op = OperationId::parse(&format!("capture-{}", self.layers)).unwrap();
        let update = repo
            .capture_layer(&op, &head, &SparseChanges::new(changes).unwrap())
            .unwrap();
        let LayerUpdate::Applied {
            state: LayerState::Active { head },
        } = update
        else {
            panic!("capture was not applied: {update:?}");
        };
        repo.read_checkpoint(head.checkpoint())
            .unwrap()
            .manifest()
            .clone()
    }

    fn merge(&self, ours: &Manifest, theirs: &Manifest) -> MergeOutcome {
        merge(&self.base, ours, theirs, |id| self.repo.read_blob(id)).unwrap()
    }

    /// Records every loaded object to prove which paths needed content.
    fn merge_counting(&self, ours: &Manifest, theirs: &Manifest) -> (MergeOutcome, Vec<ObjectId>) {
        let loads = RefCell::new(Vec::new());
        let outcome = merge(&self.base, ours, theirs, |id| {
            loads.borrow_mut().push(id);
            self.repo.read_blob(id)
        })
        .unwrap();
        (outcome, loads.into_inner())
    }

    /// The caller's job: store new bytes, build the manifest, and read it back.
    fn persist(&mut self, outcome: &MergeOutcome) -> Manifest {
        assert!(outcome.is_clean(), "{:?}", outcome.conflicts);
        let mut entries = Vec::new();
        for (path, merged) in &outcome.merged {
            let entry = match merged {
                Merged::Retained(entry) => entry.clone(),
                Merged::Content { bytes, executable } => FileEntry::Regular {
                    content: self.repo.put_blob(bytes).unwrap(),
                    executable: *executable,
                },
            };
            entries.push((path.clone(), entry));
        }
        let id = self
            .repo
            .put_manifest(&Manifest::new(entries).unwrap())
            .unwrap();
        self.repo.read_manifest(id).unwrap()
    }

    fn bytes(&self, manifest: &Manifest, p: &str) -> Vec<u8> {
        let entry = &manifest.entries()[&path(p)];
        self.repo.read_blob(entry.content()).unwrap()
    }

    fn entry_of(&self, manifest: &Manifest, p: &str) -> Option<FileEntry> {
        manifest.entries().get(&path(p)).cloned()
    }
}

fn entry(repo: &mut Repository, spec: Spec) -> FileEntry {
    match spec {
        File(bytes) => FileEntry::Regular {
            content: repo.put_blob(bytes).unwrap(),
            executable: false,
        },
        Exec(bytes) => FileEntry::Regular {
            content: repo.put_blob(bytes).unwrap(),
            executable: true,
        },
        Link(target) => FileEntry::Symlink {
            target: repo.put_blob(target).unwrap(),
        },
    }
}

fn kinds(outcome: &MergeOutcome) -> Vec<(&str, ConflictKind)> {
    outcome
        .conflicts
        .iter()
        .map(|c| (c.path.as_str(), c.kind))
        .collect()
}

const TEN: &[u8] = b"1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";

#[test]
fn disjoint_path_changes_retain_exact_entries_without_loading_content() {
    let mut fx = Fixture::new(&[
        ("a", File(b"a\n")),
        ("b", File(b"b\n")),
        ("gone", File(b"x\n")),
        ("same", File(b"s\n")),
    ]);
    let ours = fx.layer(&[("a", Some(File(b"A\n"))), ("gone", None)]);
    let theirs = fx.layer(&[("b", Some(File(b"B\n"))), ("dir/new", Some(File(b"n\n")))]);
    let (outcome, loads) = fx.merge_counting(&ours, &theirs);
    assert!(loads.is_empty(), "no path needed content: {loads:?}");
    assert!(outcome.is_clean());
    let result = fx.persist(&outcome);
    assert_eq!(fx.entry_of(&result, "a"), fx.entry_of(&ours, "a"));
    assert_eq!(fx.entry_of(&result, "b"), fx.entry_of(&theirs, "b"));
    assert_eq!(
        fx.entry_of(&result, "dir/new"),
        fx.entry_of(&theirs, "dir/new")
    );
    assert_eq!(fx.entry_of(&result, "same"), fx.entry_of(&fx.base, "same"));
    assert_eq!(fx.entry_of(&result, "gone"), None);
    assert_eq!(result.entries().len(), 4);
}

#[test]
fn disjoint_line_edits_in_one_file_combine_into_new_bytes() {
    let mut fx = Fixture::new(&[("f", File(TEN))]);
    let ours = fx.layer(&[("f", Some(File(b"1\nTWO\n3\n4\n5\n6\n7\n8\n9\n10\n")))]);
    let theirs = fx.layer(&[("f", Some(File(b"1\n2\n3\n4\n5\n6\n7\n8\nNINE\n10\n11\n")))]);
    let outcome = fx.merge(&ours, &theirs);
    let result = fx.persist(&outcome);
    assert_eq!(
        fx.bytes(&result, "f"),
        b"1\nTWO\n3\n4\n5\n6\n7\n8\nNINE\n10\n11\n"
    );
    assert!(matches!(
        outcome.merged[&path("f")],
        Merged::Content {
            executable: false,
            ..
        }
    ));
}

#[test]
fn overlapping_and_adjacent_line_edits_are_unresolved_without_markers() {
    let mut fx = Fixture::new(&[("same", File(TEN)), ("adjacent", File(TEN))]);
    let ours = fx.layer(&[
        ("same", Some(File(b"1\n2\nOURS\n4\n5\n6\n7\n8\n9\n10\n"))),
        (
            "adjacent",
            Some(File(b"1\n2\nOURS\n4\n5\n6\n7\n8\n9\n10\n")),
        ),
    ]);
    let theirs = fx.layer(&[
        ("same", Some(File(b"1\n2\nTHEIRS\n4\n5\n6\n7\n8\n9\n10\n"))),
        (
            "adjacent",
            Some(File(b"1\n2\n3\nTHEIRS\n5\n6\n7\n8\n9\n10\n")),
        ),
    ]);
    let outcome = fx.merge(&ours, &theirs);
    assert_eq!(
        kinds(&outcome),
        [
            ("adjacent", ConflictKind::Content),
            ("same", ConflictKind::Content)
        ]
    );
    assert!(outcome.merged.is_empty(), "no conflicted bytes are offered");
    let same = &outcome.conflicts[1];
    assert_eq!(same.base, fx.entry_of(&fx.base, "same"));
    assert_eq!(same.ours, fx.entry_of(&ours, "same"));
    assert_eq!(same.theirs, fx.entry_of(&theirs, "same"));
    assert_eq!(same.with, None);
}

#[test]
fn different_insertions_at_one_point_conflict_and_identical_ones_converge() {
    let mut fx = Fixture::new(&[("diff", File(TEN)), ("same", File(TEN))]);
    let ours = fx.layer(&[
        ("diff", Some(File(b"1\n2\n3\n4\n5\nX\n6\n7\n8\n9\n10\n"))),
        ("same", Some(File(b"1\n2\n3\n4\n5\nX\n6\n7\n8\n9\n10\n"))),
    ]);
    let theirs = fx.layer(&[
        ("diff", Some(File(b"1\n2\n3\n4\n5\nY\n6\n7\n8\n9\n10\n"))),
        ("same", Some(File(b"1\n2\n3\n4\n5\nX\n6\n7\n8\n9\n10\n"))),
    ]);
    let (outcome, loads) = fx.merge_counting(&ours, &theirs);
    assert_eq!(kinds(&outcome), [("diff", ConflictKind::Content)]);
    assert_eq!(loads.len(), 3, "identical entries never load content");
    assert_eq!(
        outcome.merged[&path("same")],
        Merged::Retained(fx.entry_of(&ours, "same").unwrap())
    );
}

#[test]
fn partial_convergence_inside_a_file_is_left_to_the_owner() {
    // A shared edit plus a one-sided edit could be a matching artifact with
    // repeated lines, so only whole-file identical content converges.
    let mut fx = Fixture::new(&[("f", File(TEN))]);
    let ours = fx.layer(&[("f", Some(File(b"1\nZ\n3\n4\n5\n6\n7\n8\n9\nEND\n")))]);
    let theirs = fx.layer(&[("f", Some(File(b"1\nZ\n3\n4\n5\n6\n7\n8\n9\n10\n")))]);
    let outcome = fx.merge(&ours, &theirs);
    assert_eq!(kinds(&outcome), [("f", ConflictKind::Content)]);
}

#[test]
fn add_delete_and_modify_combinations() {
    let mut fx = Fixture::new(&[
        ("both-deleted", File(b"x\n")),
        ("ours-deleted", File(b"x\n")),
        ("theirs-deleted", File(b"x\n")),
    ]);
    let ours = fx.layer(&[
        ("both-deleted", None),
        ("ours-deleted", None),
        ("theirs-deleted", Some(File(b"changed\n"))),
        ("added-same", Some(File(b"s\n"))),
        ("added-diff", Some(File(b"o\n"))),
    ]);
    let theirs = fx.layer(&[
        ("both-deleted", None),
        ("ours-deleted", Some(File(b"changed\n"))),
        ("theirs-deleted", None),
        ("added-same", Some(File(b"s\n"))),
        ("added-diff", Some(File(b"t\n"))),
    ]);
    let outcome = fx.merge(&ours, &theirs);
    assert_eq!(
        kinds(&outcome),
        [
            ("added-diff", ConflictKind::AddAdd),
            ("ours-deleted", ConflictKind::ModifyDelete),
            ("theirs-deleted", ConflictKind::ModifyDelete),
        ]
    );
    let add = &outcome.conflicts[0];
    assert_eq!(
        (add.base.clone(), add.ours.is_some(), add.theirs.is_some()),
        (None, true, true)
    );
    let deleted = &outcome.conflicts[1];
    assert_eq!(deleted.ours, None);
    assert_eq!(deleted.theirs, fx.entry_of(&theirs, "ours-deleted"));
    assert_eq!(
        outcome
            .merged
            .keys()
            .map(RepoPath::as_str)
            .collect::<Vec<_>>(),
        ["added-same"]
    );
}

#[test]
fn crlf_lines_are_preserved_and_line_ending_rewrites_conflict() {
    let crlf: &[u8] = b"a\r\nb\r\nc\r\nd\r\ne\r\n";
    let mut fx = Fixture::new(&[("keep", File(crlf)), ("rewrite", File(crlf))]);
    let ours = fx.layer(&[
        ("keep", Some(File(b"A\r\nb\r\nc\r\nd\r\ne\r\n"))),
        ("rewrite", Some(File(b"a\nb\nc\nd\ne\n"))),
    ]);
    let theirs = fx.layer(&[
        ("keep", Some(File(b"a\r\nb\r\nc\r\nd\r\nE\r\n"))),
        ("rewrite", Some(File(b"a\r\nb\r\nC\r\nd\r\ne\r\n"))),
    ]);
    let outcome = fx.merge(&ours, &theirs);
    assert_eq!(kinds(&outcome), [("rewrite", ConflictKind::Content)]);
    assert_eq!(
        outcome.merged[&path("keep")],
        Merged::Content {
            bytes: b"A\r\nb\r\nc\r\nd\r\nE\r\n".to_vec(),
            executable: false
        }
    );
}

#[test]
fn missing_final_newline_is_an_exact_line_difference() {
    let base: &[u8] = b"a\nb\nc\nd\nlast";
    let mut fx = Fixture::new(&[
        ("append", File(base)),
        ("terminate", File(base)),
        ("both", File(base)),
    ]);
    let ours = fx.layer(&[
        ("append", Some(File(b"a\nb\nc\nd\nlast\nmore"))),
        ("terminate", Some(File(b"a\nb\nc\nd\nlast\n"))),
        ("both", Some(File(b"a\nb\nc\nd\nlast\n"))),
    ]);
    let theirs = fx.layer(&[
        ("append", Some(File(b"A\nb\nc\nd\nlast"))),
        ("terminate", Some(File(b"A\nb\nc\nd\nlast"))),
        ("both", Some(File(b"a\nb\nc\nd\nLAST"))),
    ]);
    let outcome = fx.merge(&ours, &theirs);
    assert_eq!(kinds(&outcome), [("both", ConflictKind::Content)]);
    let bytes = |p: &str| match &outcome.merged[&path(p)] {
        Merged::Content { bytes, .. } => bytes.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(bytes("append"), b"A\nb\nc\nd\nlast\nmore");
    assert_eq!(bytes("terminate"), b"A\nb\nc\nd\nlast\n");
}

#[test]
fn binary_content_is_refused_only_when_both_sides_change_it() {
    let bin: &[u8] = b"head\n\0\x01\x02\nmid\n\xff\xfe\ntail\n";
    let mut fx = Fixture::new(&[("both", File(bin)), ("one", File(bin))]);
    let ours = fx.layer(&[
        (
            "both",
            Some(File(b"HEAD\n\0\x01\x02\nmid\n\xff\xfe\ntail\n")),
        ),
        ("one", Some(File(b"changed\0"))),
    ]);
    let theirs = fx.layer(&[(
        "both",
        Some(File(b"head\n\0\x01\x02\nmid\n\xff\xfe\nTAIL\n")),
    )]);
    let outcome = fx.merge(&ours, &theirs);
    // A line merge would combine these disjoint edits; binary content must not.
    assert_eq!(kinds(&outcome), [("both", ConflictKind::Binary)]);
    assert_eq!(
        outcome.merged[&path("one")],
        Merged::Retained(fx.entry_of(&ours, "one").unwrap())
    );
}

#[test]
fn non_utf8_text_without_nul_still_merges_by_line() {
    let mut fx = Fixture::new(&[("latin1", File(b"caf\xe9\nb\nc\nd\ne\n"))]);
    let ours = fx.layer(&[("latin1", Some(File(b"CAF\xc9\nb\nc\nd\ne\n")))]);
    let theirs = fx.layer(&[("latin1", Some(File(b"caf\xe9\nb\nc\nd\n\xe9\n")))]);
    let result = {
        let outcome = fx.merge(&ours, &theirs);
        fx.persist(&outcome)
    };
    assert_eq!(fx.bytes(&result, "latin1"), b"CAF\xc9\nb\nc\nd\n\xe9\n");
}

#[test]
fn executable_bit_merges_independently_of_content() {
    let mut fx = Fixture::new(&[
        ("mode-only", File(TEN)),
        ("mode-and-text", File(TEN)),
        ("drop", Exec(TEN)),
        ("theirs-mode", File(TEN)),
        ("same-text", File(TEN)),
    ]);
    let ours = fx.layer(&[
        ("mode-only", Some(Exec(TEN))),
        (
            "mode-and-text",
            Some(Exec(b"ONE\n2\n3\n4\n5\n6\n7\n8\n9\n10\n")),
        ),
        ("drop", Some(File(TEN))),
        (
            "theirs-mode",
            Some(File(b"ONE\n2\n3\n4\n5\n6\n7\n8\n9\n10\n")),
        ),
        (
            "same-text",
            Some(Exec(b"ONE\n2\n3\n4\n5\n6\n7\n8\n9\n10\n")),
        ),
    ]);
    let theirs = fx.layer(&[
        ("mode-only", Some(File(b"1\n2\n3\n4\n5\n6\n7\n8\n9\nTEN\n"))),
        (
            "mode-and-text",
            Some(File(b"1\n2\n3\n4\n5\n6\n7\n8\n9\nTEN\n")),
        ),
        ("drop", Some(Exec(b"1\n2\n3\n4\n5\n6\n7\n8\n9\nTEN\n"))),
        ("theirs-mode", Some(Exec(TEN))),
        (
            "same-text",
            Some(File(b"ONE\n2\n3\n4\n5\n6\n7\n8\n9\n10\n")),
        ),
    ]);
    let (outcome, loads) = fx.merge_counting(&ours, &theirs);
    assert!(outcome.is_clean(), "{:?}", outcome.conflicts);
    assert_eq!(
        loads.len(),
        3,
        "only mode-and-text changed content on both sides"
    );
    let result = fx.persist(&outcome);
    let ten = b"1\n2\n3\n4\n5\n6\n7\n8\n9\nTEN\n";
    assert_eq!(
        fx.entry_of(&result, "mode-only"),
        Some(FileEntry::Regular {
            content: fx.entry_of(&theirs, "mode-only").unwrap().content(),
            executable: true
        })
    );
    assert_eq!(
        fx.entry_of(&result, "drop"),
        Some(FileEntry::Regular {
            content: fx.entry_of(&theirs, "drop").unwrap().content(),
            executable: false
        })
    );
    assert_eq!(
        fx.bytes(&result, "mode-and-text"),
        b"ONE\n2\n3\n4\n5\n6\n7\n8\n9\nTEN\n"
    );
    assert!(matches!(
        fx.entry_of(&result, "mode-and-text"),
        Some(FileEntry::Regular {
            executable: true,
            ..
        })
    ));
    assert_eq!(fx.bytes(&result, "drop"), ten);
    // Content-only on one side and mode-only on the other retains the edited object.
    for p in ["theirs-mode", "same-text"] {
        assert_eq!(
            outcome.merged[&path(p)],
            Merged::Retained(FileEntry::Regular {
                content: fx.entry_of(&ours, p).unwrap().content(),
                executable: true
            }),
            "{p}"
        );
    }
}

#[test]
fn symlink_targets_and_entry_types_are_never_line_merged() {
    let mut fx = Fixture::new(&[
        ("one-retarget", Link(b"a")),
        ("both-retarget", Link(b"target/a\nx")),
        ("type-vs-edit", File(TEN)),
        ("link-vs-delete", Link(b"a")),
        ("same-type-change", File(TEN)),
    ]);
    let ours = fx.layer(&[
        ("one-retarget", Some(Link(b"b"))),
        ("both-retarget", Some(Link(b"target/b\nx"))),
        ("type-vs-edit", Some(Link(b"elsewhere"))),
        ("link-vs-delete", Some(Link(b"b"))),
        ("same-type-change", Some(Link(b"t"))),
    ]);
    let theirs = fx.layer(&[
        ("both-retarget", Some(Link(b"target/a\ny"))),
        (
            "type-vs-edit",
            Some(File(b"1\n2\n3\n4\n5\n6\n7\n8\n9\nTEN\n")),
        ),
        ("link-vs-delete", None),
        ("same-type-change", Some(Link(b"t"))),
    ]);
    let (outcome, loads) = fx.merge_counting(&ours, &theirs);
    assert!(loads.is_empty(), "symlink targets are compared by ID only");
    assert_eq!(
        kinds(&outcome),
        [
            ("both-retarget", ConflictKind::EntryType),
            ("link-vs-delete", ConflictKind::ModifyDelete),
            ("type-vs-edit", ConflictKind::EntryType),
        ]
    );
    assert_eq!(
        outcome.merged[&path("one-retarget")],
        Merged::Retained(fx.entry_of(&ours, "one-retarget").unwrap())
    );
    assert_eq!(
        outcome.merged[&path("same-type-change")],
        Merged::Retained(fx.entry_of(&ours, "same-type-change").unwrap())
    );
}

#[test]
fn file_and_directory_prefix_collisions_are_structural_conflicts() {
    let mut fx = Fixture::new(&[("dir/x", File(b"x\n")), ("keep", File(b"k\n"))]);
    // Ours replaces directory `dir` with a file; theirs adds a file under it.
    let ours = fx.layer(&[("dir/x", None), ("dir", Some(File(b"now a file\n")))]);
    let theirs = fx.layer(&[("dir/y", Some(File(b"y\n")))]);
    let outcome = fx.merge(&ours, &theirs);
    let found: Vec<_> = outcome
        .conflicts
        .iter()
        .map(|c| {
            (
                c.path.as_str(),
                c.kind,
                c.with.as_ref().map(RepoPath::as_str),
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            ("dir", ConflictKind::PathPrefix, Some("dir/y")),
            ("dir/y", ConflictKind::PathPrefix, Some("dir")),
        ]
    );
    assert_eq!(
        outcome
            .merged
            .keys()
            .map(RepoPath::as_str)
            .collect::<Vec<_>>(),
        ["keep"]
    );
}

#[test]
fn resolved_paths_colliding_with_an_unresolved_path_are_held_back() {
    let mut fx = Fixture::new(&[("a", File(b"a\n"))]);
    // Theirs turns file `a` into directory `a/`; ours edits file `a`.
    let ours = fx.layer(&[("a", Some(File(b"edited\n")))]);
    let theirs = fx.layer(&[("a", None), ("a/b", Some(File(b"b\n")))]);
    let outcome = fx.merge(&ours, &theirs);
    assert_eq!(
        kinds(&outcome),
        [
            ("a", ConflictKind::ModifyDelete),
            ("a/b", ConflictKind::PathPrefix)
        ]
    );
    assert_eq!(outcome.conflicts[0].with, Some(path("a/b")));
    assert_eq!(outcome.conflicts[1].with, Some(path("a")));
    assert!(outcome.merged.is_empty());
}

#[test]
fn merge_is_deterministic_and_symmetric_in_reported_paths() {
    let mut fx = Fixture::new(&[("f", File(TEN)), ("g", File(TEN)), ("dir/x", File(b"x\n"))]);
    let ours = fx.layer(&[
        ("f", Some(File(b"OURS\n2\n3\n4\n5\n6\n7\n8\n9\n10\n"))),
        ("g", Some(File(b"1\n2\nG\n4\n5\n6\n7\n8\n9\n10\n"))),
        ("dir/x", None),
        ("dir", Some(File(b"d\n"))),
    ]);
    let theirs = fx.layer(&[
        ("f", Some(File(b"THEIRS\n2\n3\n4\n5\n6\n7\n8\n9\n10\n"))),
        ("g", Some(File(b"1\n2\n3\n4\n5\n6\n7\n8\nG\n10\n"))),
        ("dir/y", Some(File(b"y\n"))),
    ]);
    let (first, first_loads) = fx.merge_counting(&ours, &theirs);
    let (second, second_loads) = fx.merge_counting(&ours, &theirs);
    assert_eq!(first, second);
    assert_eq!(first_loads, second_loads);
    let (swapped, _) = fx.merge_counting(&theirs, &ours);
    assert_eq!(kinds(&first), kinds(&swapped));
    assert_eq!(first.merged, swapped.merged);
    let swapped_f: &Conflict = &swapped.conflicts[2];
    assert_eq!(
        (swapped_f.ours.clone(), swapped_f.theirs.clone()),
        (
            first.conflicts[2].theirs.clone(),
            first.conflicts[2].ours.clone()
        )
    );
}

#[test]
fn loader_errors_propagate_unchanged() {
    let mut fx = Fixture::new(&[("f", File(TEN))]);
    let ours = fx.layer(&[("f", Some(File(b"x\n")))]);
    let theirs = fx.layer(&[("f", Some(File(b"y\n")))]);
    let result = merge(&fx.base, &ours, &theirs, Err::<Vec<u8>, _>);
    assert_eq!(
        result.unwrap_err(),
        fx.entry_of(&fx.base, "f").unwrap().content()
    );
}
