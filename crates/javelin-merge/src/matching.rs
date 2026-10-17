/// One exact LCS matching and whether every minimal matching keeps each base row.
pub(crate) fn matches(base: &[&[u8]], other: &[&[u8]]) -> (Vec<Option<usize>>, Vec<bool>) {
    let (n, m) = (base.len(), other.len());
    let width = m + 1;
    let mut prefix = vec![0u32; (n + 1) * width];
    for i in 0..n {
        for j in 0..m {
            prefix[(i + 1) * width + j + 1] = if base[i] == other[j] {
                prefix[i * width + j] + 1
            } else {
                prefix[i * width + j + 1].max(prefix[(i + 1) * width + j])
            };
        }
    }
    let total = prefix[n * width + m];
    let mut matched = vec![None; n];
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        if base[i - 1] == other[j - 1] {
            matched[i - 1] = Some(j - 1);
            i -= 1;
            j -= 1;
        } else if prefix[(i - 1) * width + j] >= prefix[i * width + j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    let mut mandatory = vec![false; n];
    let mut suffix = vec![0u32; width];
    let mut row = vec![0u32; width];
    for i in (0..n).rev() {
        // `suffix[j]` is LCS(base[i+1..], other[j..]). Any matching omitting
        // base[i] splits at some j, so this maximum is exactly its best length.
        let without = (0..=m)
            .map(|j| prefix[i * width + j] + suffix[j])
            .max()
            .unwrap();
        mandatory[i] = without < total;
        for j in (0..m).rev() {
            row[j] = if base[i] == other[j] {
                1 + suffix[j + 1]
            } else {
                suffix[j].max(row[j + 1])
            };
        }
        std::mem::swap(&mut row, &mut suffix);
    }
    (matched, mandatory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mandatory_rows_and_traceback_match_all_subsequence_masks() {
        let mut words: Vec<Vec<&[u8]>> = vec![vec![]];
        let mut level = words.clone();
        for _ in 0..3 {
            level = level
                .iter()
                .flat_map(|word| {
                    [b"a".as_slice(), b"b", b"U"].map(|token| {
                        let mut next = word.clone();
                        next.push(token);
                        next
                    })
                })
                .collect();
            words.extend(level.clone());
        }
        for base in &words {
            for side in &words {
                let (mut best, mut masks) = (0, Vec::new());
                for mask in 0usize..1 << base.len() {
                    let subsequence: Vec<_> = base
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| mask & (1 << i) != 0)
                        .map(|(_, v)| *v)
                        .collect();
                    for other in 0usize..1 << side.len() {
                        let candidate: Vec<_> = side
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| other & (1 << i) != 0)
                            .map(|(_, v)| *v)
                            .collect();
                        if subsequence == candidate && subsequence.len() >= best {
                            if subsequence.len() > best {
                                best = subsequence.len();
                                masks.clear();
                            }
                            masks.push(mask);
                        }
                    }
                }
                let (matched, mandatory) = matches(base, side);
                assert_eq!(matched.iter().flatten().count(), best);
                let mut previous = None;
                for (i, j) in matched.iter().enumerate() {
                    if let Some(j) = *j {
                        assert_eq!(base[i], side[j]);
                        assert!(previous.is_none_or(|p| p < j));
                        previous = Some(j);
                    }
                    assert_eq!(
                        mandatory[i],
                        masks.iter().all(|mask| mask & (1 << i) != 0),
                        "{base:?} {side:?} row={i}"
                    );
                }
            }
        }
    }
}
