//! Repeated identical lines must never let a line merge invent or drop an edit.

use javelin::{FileEntry, Manifest, ObjectId, RepoPath};
use javelin_merge::{ConflictKind, MergeOutcome, Merged, merge};

/// Merges raw bytes as one regular file. IDs index the in-memory versions.
fn merge_text(base: &[u8], ours: &[u8], theirs: &[u8]) -> MergeOutcome {
    let versions = [base, ours, theirs];
    let id = |n: u8| ObjectId::parse(&format!("{n:064x}")).unwrap();
    let manifest = |n: u8| {
        let entry = FileEntry::Regular {
            content: id(n),
            executable: false,
        };
        Manifest::new([(RepoPath::parse("f").unwrap(), entry)]).unwrap()
    };
    let ids = [id(0), id(1), id(2)];
    merge(&manifest(0), &manifest(1), &manifest(2), |object| {
        Ok::<_, ()>(versions[ids.iter().position(|i| *i == object).unwrap()].to_vec())
    })
    .unwrap()
}

fn merged_text(outcome: &MergeOutcome) -> Option<Vec<u8>> {
    match outcome.merged.get(&RepoPath::parse("f").unwrap()) {
        Some(Merged::Content { bytes, .. }) => Some(bytes.clone()),
        Some(other) => panic!("unexpected {other:?}"),
        None => None,
    }
}

#[test]
fn replacements_of_neighbouring_repeated_lines_conflict() {
    let outcome = merge_text(b"a\na\na\n", b"a\nx\na\n", b"x\na\na\n");
    assert_eq!(merged_text(&outcome), None, "must not invent an extra line");
    assert_eq!(outcome.conflicts[0].kind, ConflictKind::Content);
}

#[test]
fn a_deletion_in_a_run_of_repeated_lines_is_kept() {
    let outcome = merge_text(b"b\nb\nb\nb\nb\n", b"b\nb\nz\nb\nb\n", b"b\nb\nb\nb\n");
    if let Some(bytes) = merged_text(&outcome) {
        assert_eq!(
            bytes.iter().filter(|b| **b == b'\n').count(),
            4,
            "{bytes:?}"
        );
        assert!(bytes.contains(&b'z'));
    }
}

/// Small deterministic generator; no external crates or randomness sources.
struct Lcg(u64);
impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) % n as u64) as usize
    }
}

fn join(lines: &[&[u8]]) -> Vec<u8> {
    lines.iter().flat_map(|l| [*l, b"\n"].concat()).collect()
}

/// Each side makes one edit with a unique token, so a clean result has an exact
/// line count and contains each token exactly once.
#[test]
fn single_edits_over_repetitive_bases_never_merge_into_wrong_lengths() {
    let mut rng = Lcg(0x006a_6176_656c_696e);
    let alphabet: [&[u8]; 3] = [b"a", b"b", b"}"];
    let mut clean = 0;
    for _ in 0..20_000 {
        let len = 1 + rng.below(7);
        let base: Vec<&[u8]> = (0..len).map(|_| alphabet[rng.below(3)]).collect();
        // 0 replaces a line, 1 inserts before a line, 2 deletes a line.
        let edit = |rng: &mut Lcg, token: &'static [u8]| {
            let mut lines = base.clone();
            let at = rng.below(len);
            let op = rng.below(3);
            match op {
                0 => lines[at] = token,
                1 => lines.insert(at, token),
                _ => {
                    lines.remove(at);
                }
            }
            (lines, op)
        };
        let (ours, o) = edit(&mut rng, b"OURS");
        let (theirs, t) = edit(&mut rng, b"THEIRS");
        let outcome = merge_text(&join(&base), &join(&ours), &join(&theirs));
        let Some(bytes) = merged_text(&outcome) else {
            continue;
        };
        clean += 1;
        let lines: Vec<&[u8]> = bytes
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .collect();
        let delta = |op| match op {
            0 => 0,
            1 => 1,
            _ => -1,
        };
        let expected = len as i64 + delta(o) + delta(t);
        let context = format!("base={base:?} ours={ours:?} theirs={theirs:?} got={lines:?}");
        // Two deletions of identical lines may be one convergent deletion.
        let allowed = if (o, t) == (2, 2) {
            expected..=expected + 1
        } else {
            expected..=expected
        };
        assert!(allowed.contains(&(lines.len() as i64)), "{context}");
        for (op, token) in [(o, &b"OURS"[..]), (t, &b"THEIRS"[..])] {
            let want = usize::from(op != 2);
            assert_eq!(
                lines.iter().filter(|l| **l == token).count(),
                want,
                "{context}"
            );
        }
    }
    assert!(
        clean > 1000,
        "the generator must exercise clean merges: {clean}"
    );
}
