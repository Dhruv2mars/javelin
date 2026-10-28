//! Conflict text for an owner to resolve by editing: the merge of one file with every region
//! both sides changed set between markers, so the file can be written into a Layer directory.

use crate::{Resource, lines, matching::matches};

/// What each side of a marked region is called on its marker line.
#[derive(Clone, Copy, Debug)]
pub struct Labels<'a> {
    pub ours: &'a str,
    pub base: &'a str,
    pub theirs: &'a str,
}

/// The first marker line of a region, without its label.
const OURS: &[u8] = b"<<<<<<<";
const BASE: &[u8] = b"|||||||";
const SPLIT: &[u8] = b"=======";
const THEIRS: &[u8] = b">>>>>>>";

/// `ours` and `theirs` merged relative to `base`, line by line. A region only one side
/// changed takes that side; a region both sides changed differently is written whole as
///
/// ```text
/// <<<<<<< ours label
/// ...ours...
/// ||||||| base label
/// ...base...
/// =======
/// ...theirs...
/// >>>>>>> theirs label
/// ```
///
/// Regions are separated only by an unchanged base line that every minimal diff of both
/// sides keeps and that occurs once in each text, as [`crate::merge`] separates them, so no
/// edit is dropped, duplicated or reordered outside a marked region. Text over the merge's
/// line limits is one marked region.
pub fn conflict_text(base: &[u8], ours: &[u8], theirs: &[u8], labels: Labels<'_>) -> Vec<u8> {
    let (b, o, t) = (lines(base), lines(ours), lines(theirs));
    let cells = |n: usize, m: usize| (n as u64 + 1) * (m as u64 + 1);
    let fits = [b.len(), o.len(), t.len()]
        .iter()
        .all(|n| Resource::Lines.check(*n as u64).is_ok())
        && Resource::TableCells.check(cells(b.len(), o.len())).is_ok()
        && Resource::TableCells.check(cells(b.len(), t.len())).is_ok();
    let mut out = Vec::new();
    if !fits {
        marked(&mut out, &o, &b, &t, labels);
        return out;
    }
    let (in_ours, ours_kept) = matches(&b, &o);
    let (in_theirs, theirs_kept) = matches(&b, &t);
    let unique = |line: &[u8]| {
        [&b, &o, &t]
            .iter()
            .all(|lines| lines.iter().filter(|l| **l == line).count() == 1)
    };
    let mut group = Group::default();
    let (mut bi, mut oi, mut ti) = (0, 0, 0);
    loop {
        let anchor = (bi..b.len()).find_map(|i| Some((i, in_ours[i]?, in_theirs[i]?)));
        let (nb, no, nt) = anchor.unwrap_or((b.len(), o.len(), t.len()));
        group.add(&b[bi..nb], &o[oi..no], &t[ti..nt]);
        let Some((nb, no, nt)) = anchor else {
            group.flush(&mut out, labels);
            break;
        };
        let line = [b[nb]];
        if unique(b[nb]) && ours_kept[nb] && theirs_kept[nb] {
            group.flush(&mut out, labels);
            out.extend_from_slice(b[nb]);
        } else {
            group.add(&line, &line, &line);
        }
        (bi, oi, ti) = (nb + 1, no + 1, nt + 1);
    }
    out
}

/// Whether `bytes` still holds a marker line [`conflict_text`] writes around a region.
pub fn has_markers(bytes: &[u8]) -> bool {
    lines(bytes).iter().any(|line| {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        [OURS, BASE, THEIRS]
            .iter()
            .any(|marker| line == *marker || line.starts_with(&[marker, &b" "[..]].concat()))
            || line == SPLIT
    })
}

/// Lines between two separating anchors, per side.
#[derive(Default)]
struct Group<'a> {
    base: Vec<&'a [u8]>,
    ours: Vec<&'a [u8]>,
    theirs: Vec<&'a [u8]>,
}

impl<'a> Group<'a> {
    fn add(&mut self, base: &[&'a [u8]], ours: &[&'a [u8]], theirs: &[&'a [u8]]) {
        self.base.extend_from_slice(base);
        self.ours.extend_from_slice(ours);
        self.theirs.extend_from_slice(theirs);
    }

    fn flush(&mut self, out: &mut Vec<u8>, labels: Labels<'_>) {
        let group = std::mem::take(self);
        let taken = if group.ours == group.base || group.ours == group.theirs {
            Some(&group.theirs)
        } else if group.theirs == group.base {
            Some(&group.ours)
        } else {
            None
        };
        match taken {
            Some(lines) => out.extend(lines.iter().copied().flatten()),
            None => marked(out, &group.ours, &group.base, &group.theirs, labels),
        }
    }
}

fn marked(out: &mut Vec<u8>, ours: &[&[u8]], base: &[&[u8]], theirs: &[&[u8]], labels: Labels<'_>) {
    let mut side = |marker: &[u8], label: &str, lines: Option<&[&[u8]]>| {
        out.extend_from_slice(marker);
        if !label.is_empty() {
            out.push(b' ');
            out.extend_from_slice(label.as_bytes());
        }
        out.push(b'\n');
        for line in lines.into_iter().flatten() {
            out.extend_from_slice(line);
        }
        if !out.ends_with(b"\n") {
            out.push(b'\n');
        }
    };
    side(OURS, labels.ours, Some(ours));
    side(BASE, labels.base, Some(base));
    side(SPLIT, "", Some(theirs));
    side(THEIRS, labels.theirs, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    const LABELS: Labels = Labels {
        ours: "ours",
        base: "base",
        theirs: "theirs",
    };

    #[test]
    fn a_region_both_sides_changed_is_marked_and_the_rest_merges() {
        let base = b"head\none\ntwo\nmiddle\nthree\ntail\n";
        let ours = b"HEAD\none\nTWO\nmiddle\nthree\ntail\n";
        let theirs = b"head\none\nzwei\nmiddle\nthree\nTAIL\n";
        let text = conflict_text(base, ours, theirs, LABELS);
        assert_eq!(
            String::from_utf8(text.clone()).unwrap(),
            "HEAD\none\n<<<<<<< ours\nTWO\n||||||| base\ntwo\n=======\nzwei\n>>>>>>> theirs\n\
             middle\nthree\nTAIL\n"
        );
        assert!(has_markers(&text));
    }

    #[test]
    fn a_clean_merge_has_no_markers() {
        let base = b"a\nb\nc\nd\ne\n";
        let ours = b"A\nb\nc\nd\ne\n";
        let theirs = b"a\nb\nc\nd\nE\n";
        let text = conflict_text(base, ours, theirs, LABELS);
        assert_eq!(text, b"A\nb\nc\nd\nE\n");
        assert!(!has_markers(&text));
    }

    #[test]
    fn an_add_add_and_a_missing_final_newline_are_one_whole_region() {
        let text = conflict_text(b"", b"mine", b"yours\n", LABELS);
        assert_eq!(
            String::from_utf8(text).unwrap(),
            "<<<<<<< ours\nmine\n||||||| base\n=======\nyours\n>>>>>>> theirs\n"
        );
    }

    #[test]
    fn markers_are_whole_lines() {
        assert!(!has_markers(b"a <<<<<<< b\n=======x\n"));
        assert!(has_markers(b"text\n=======\n"));
        assert!(has_markers(b">>>>>>> World 3\r\n"));
    }
}
