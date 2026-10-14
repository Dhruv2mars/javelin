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
fn identical_lines_inserted_near_a_replacement_are_not_merged_away() {
    for (base, ours, theirs) in [
        (
            &b"a\n\n}\nb\n"[..],
            &b"a\n}\n}\nb\n"[..],
            &b"a\n\n}\n}\nb\n"[..],
        ),
        (
            b"fn f() {\n    g();\n\n}\n}\n",
            b"fn f() {\n    g();\n}\n}\n}\n",
            b"fn f() {\n    g();\n\n}\n}\n}\n",
        ),
    ] {
        let outcome = merge_text(base, ours, theirs);
        if let Some(bytes) = merged_text(&outcome) {
            let braces = |b: &[u8]| b.iter().filter(|c| **c == b'}').count();
            // Ours adds one brace by replacement, theirs adds one by insertion.
            assert_eq!(
                braces(&bytes),
                braces(base) + 2,
                "{:?}",
                String::from_utf8_lossy(&bytes)
            );
        }
    }
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

#[derive(Clone, Copy, PartialEq)]
enum Edit {
    Replace(usize, &'static [u8]),
    Insert(usize, &'static [u8]),
    Delete(usize),
}

impl Edit {
    /// The base range this edit occupies; insertions occupy an empty point.
    fn span(self) -> (usize, usize) {
        match self {
            Edit::Replace(i, _) | Edit::Delete(i) => (i, i + 1),
            Edit::Insert(i, _) => (i, i),
        }
    }
}

fn apply<'a>(base: &[&'a [u8]], edits: &[Edit]) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    for i in 0..=base.len() {
        for edit in edits {
            if let Edit::Insert(at, token) = *edit
                && at == i
            {
                out.push(token);
            }
        }
        if i == base.len() {
            break;
        }
        let mut line = Some(base[i]);
        for edit in edits {
            match *edit {
                Edit::Replace(at, token) if at == i => line = Some(token),
                Edit::Delete(at) if at == i => line = None,
                _ => {}
            }
        }
        out.extend(line);
    }
    out
}

/// Within one side, edits may not touch the same base line.
fn overlap(a: Edit, b: Edit) -> bool {
    let ((as_, ae), (bs, be)) = (a.span(), b.span());
    as_.max(bs) < ae.min(be)
}

/// A line diff as edits on base coordinates: deleted base lines, and the lines
/// inserted before each base point (point `len` is the end of the file).
#[derive(Clone, PartialEq)]
struct Script<'a> {
    deleted: Vec<bool>,
    inserted: Vec<Vec<&'a [u8]>>,
}

/// Every minimal line diff from `base` to `side`: one per longest common subsequence
/// matching, enumerated exhaustively from the LCS table.
fn minimal_scripts<'a>(base: &[&'a [u8]], side: &[&'a [u8]]) -> Vec<Script<'a>> {
    let (n, m) = (base.len(), side.len());
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if base[i] == side[j] {
                1 + lcs[i + 1][j + 1]
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut matchings = Vec::new();
    let mut stack = vec![(0, 0, Vec::new())];
    while let Some((i, j, pairs)) = stack.pop() {
        if lcs[i][j] == 0 {
            matchings.push(pairs);
            continue;
        }
        for a in i..n {
            for b in j..m {
                if base[a] == side[b]
                    && lcs[a][b] == lcs[i][j]
                    && 1 + lcs[a + 1][b + 1] == lcs[i][j]
                {
                    let mut next: Vec<(usize, usize)> = pairs.clone();
                    next.push((a, b));
                    stack.push((a + 1, b + 1, next));
                }
            }
        }
    }
    let mut scripts: Vec<Script> = Vec::new();
    for pairs in matchings {
        let mut script = Script {
            deleted: vec![true; n],
            inserted: vec![Vec::new(); n + 1],
        };
        let mut j = 0;
        for &(a, b) in pairs.iter().chain([(n, m)].iter()) {
            script.inserted[a].extend(&side[j..b]);
            if a < n {
                script.deleted[a] = false;
            }
            j = b + 1;
        }
        if !scripts.contains(&script) {
            scripts.push(script);
        }
    }
    scripts
}

fn apply_scripts<'a>(base: &[&'a [u8]], scripts: &[&Script<'a>]) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    for i in 0..=base.len() {
        for script in scripts {
            out.extend(&script.inserted[i]);
        }
        if i < base.len() && scripts.iter().all(|s| !s.deleted[i]) {
            out.push(base[i]);
        }
    }
    out
}

/// Two sides' scripts are independent when they delete no common base line and
/// insert at no common point.
fn independent(a: &Script, b: &Script) -> bool {
    let lines = a.deleted.iter().zip(&b.deleted).all(|(x, y)| !(*x && *y));
    let points = a
        .inserted
        .iter()
        .zip(&b.inserted)
        .all(|(x, y)| x.is_empty() || y.is_empty());
    lines && points
}

/// Independent oracle: a clean result must equal applying some minimal line diff of
/// ours together with some minimal line diff of theirs that touch no common base line
/// or insertion point. This checks content and order without the engine's algorithm.
#[test]
fn small_edits_drawn_from_the_base_alphabet_match_an_independent_oracle() {
    let mut rng = Lcg(0x006d_6572_6765_0002);
    let alphabet: [&'static [u8]; 4] = [b"a", b"", b"}", b"x"];
    let mut clean = 0;
    for _ in 0..20_000 {
        let len = 1 + rng.below(5);
        let base: Vec<&'static [u8]> = (0..len).map(|_| alphabet[rng.below(3)]).collect();
        let side = |rng: &mut Lcg| {
            let mut script: Vec<Edit> = Vec::new();
            for _ in 0..1 + rng.below(2) {
                let at = rng.below(len);
                let token = alphabet[rng.below(4)];
                let edit = match rng.below(3) {
                    0 => Edit::Replace(at, token),
                    1 => Edit::Insert(at + rng.below(2), token),
                    _ => Edit::Delete(at),
                };
                if script.iter().all(|e| !overlap(*e, edit)) {
                    script.push(edit);
                }
            }
            apply(&base, &script)
        };
        let (ours, theirs) = (side(&mut rng), side(&mut rng));
        let outcome = merge_text(&join(&base), &join(&ours), &join(&theirs));
        let Some(bytes) = merged_text(&outcome) else {
            continue;
        };
        clean += 1;
        let got: Vec<&[u8]> = bytes
            .split_inclusive(|b| *b == b'\n')
            .map(|l| &l[..l.len() - 1])
            .collect();
        let (mine, other) = (
            minimal_scripts(&base, &ours),
            minimal_scripts(&base, &theirs),
        );
        let valid = mine.iter().any(|m| {
            other
                .iter()
                .any(|o| independent(m, o) && apply_scripts(&base, &[m, o]) == got)
        });
        assert!(
            valid,
            "base={base:?} ours={ours:?} theirs={theirs:?} got={got:?}"
        );
    }
    assert!(
        clean > 5000,
        "the generator must exercise clean merges: {clean}"
    );
}
