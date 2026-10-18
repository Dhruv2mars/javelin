//! Trace blame: who and what produced each path change in World history.
//!
//! Each checkpoint names its provenance (actor, agent, session, reference). The
//! radar index carries, per live path, the checkpoint that last set its entry;
//! Publish copies that into `blame`, one row per World change. Path blame is one
//! indexed read; line blame walks only that path's change rows with a line diff.
//! `blame` is derived from Layer history and verified by `check()`.
use std::collections::{BTreeMap, HashMap};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    CheckpointId, Error, FileEntry, LayerId, ObjectId, RepoPath, Repository, Result, SparseChanges,
    VersionNumber, WorldVersion,
    domain::ObjectKind,
    layer::{CheckpointReader, attribution_in},
    repository::{
        decode_entry, decode_id, head_in, insert_object, read_object, validate_format, version_in,
    },
};

pub(crate) const SCHEMA: &str = "
CREATE TABLE blame (
    path TEXT NOT NULL,
    version INTEGER NOT NULL REFERENCES versions(number),
    checkpoint TEXT NOT NULL REFERENCES objects(id),
    provenance TEXT REFERENCES objects(id),
    PRIMARY KEY(path,version)
) STRICT, WITHOUT ROWID;
";

const MAX_FIELD: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    Human,
    Agent,
}

/// Who and what produced a checkpoint, as declared by the harness. Not
/// authenticated identity. Stored as one content-addressed Blob.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub actor: Actor,
    /// Agent or model name, such as `claude-code/claude-opus-5-5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Optional tool-call or prompt reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

impl Provenance {
    /// Each field is 1 to 256 bytes without control characters. A human has no
    /// agent name.
    pub fn new(
        actor: Actor,
        agent: Option<String>,
        session: Option<String>,
        reference: Option<String>,
    ) -> Result<Self> {
        for (name, field) in [
            ("agent", &agent),
            ("session", &session),
            ("reference", &reference),
        ] {
            if let Some(value) = field
                && (value.is_empty()
                    || value.len() > MAX_FIELD
                    || value.chars().any(char::is_control))
            {
                return Err(Error::InvalidInput(format!(
                    "provenance {name} must be 1..{MAX_FIELD} bytes without control characters"
                )));
            }
        }
        if actor == Actor::Human && agent.is_some() {
            return Err(Error::InvalidInput(
                "a human actor has no agent name".into(),
            ));
        }
        Ok(Self {
            actor,
            agent,
            session,
            reference,
        })
    }

    pub(crate) fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("provenance encoding")
    }

    fn decode(id: ObjectId, bytes: &[u8]) -> Result<Self> {
        let value: Self = serde_json::from_slice(bytes).map_err(|_| Error::CorruptObject(id))?;
        let value = Self::new(value.actor, value.agent, value.session, value.reference)
            .map_err(|_| Error::CorruptObject(id))?;
        if value.bytes() != bytes {
            return Err(Error::CorruptObject(id));
        }
        Ok(value)
    }
}

pub(crate) fn store_provenance(conn: &Connection, provenance: &Provenance) -> Result<ObjectId> {
    insert_object(conn, ObjectKind::Blob, &provenance.bytes())
}

pub(crate) fn read_provenance(conn: &Connection, id: ObjectId) -> Result<Provenance> {
    Provenance::decode(id, &read_object(conn, id, ObjectKind::Blob)?)
}

/// The World version that made a change, and the Layer checkpoint and session
/// behind it. Versions advanced without a Layer (`compare_and_advance`) have no
/// checkpoint; unattributed checkpoints have no provenance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Origin {
    pub version: WorldVersion,
    pub layer: Option<LayerId>,
    pub checkpoint: Option<CheckpointId>,
    pub provenance: Option<Provenance>,
}

/// The last change to a path at or before a version. `entry` is None when that
/// change deleted it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PathBlame {
    pub path: RepoPath,
    pub entry: Option<FileEntry>,
    pub origin: Origin,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LineBlame {
    pub path: RepoPath,
    pub version: VersionNumber,
    /// Distinct origins, in World order; lines refer to them by index.
    pub origins: Vec<Origin>,
    pub lines: Vec<BlameLine>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BlameLine {
    /// 1-based line number.
    pub number: usize,
    pub origin: usize,
    /// The line without its newline, decoded lossily as UTF-8.
    pub text: String,
}

impl Repository {
    /// The last change to `path` at or before `at` (default: the head): one
    /// indexed read, O(log), independent of history length.
    pub fn blame(&self, path: &RepoPath, at: Option<VersionNumber>) -> Result<Option<PathBlame>> {
        let tx = self.conn.unchecked_transaction()?;
        validate_format(&tx)?;
        let at = match at {
            Some(at) => version_in(&tx, at)?.number,
            None => head_in(&tx)?.number,
        };
        let row = tx
            .prepare_cached(
                "SELECT version,kind,content FROM changes WHERE path=?1 AND version<=?2 \
                 ORDER BY version DESC LIMIT 1",
            )?
            .query_row(params![path.as_str(), at.sql()], |r| {
                Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?;
        let blame = row
            .map(|(version, kind, content)| {
                Ok::<_, Error>(PathBlame {
                    path: path.clone(),
                    entry: decode_entry(kind, content)?,
                    origin: origin_in(&tx, path, VersionNumber::decode(version)?)?,
                })
            })
            .transpose()?;
        tx.commit()?;
        Ok(blame)
    }

    /// Per-line origins of the file at `path` at `at` (default: the head). Walks
    /// only this path's change rows since it was last created, diffing each
    /// version's lines against the previous one. A rename is a delete plus an
    /// add, so a renamed file's lines start at the renaming version.
    pub fn blame_lines(
        &self,
        path: &RepoPath,
        at: Option<VersionNumber>,
    ) -> Result<Option<LineBlame>> {
        let tx = self.conn.unchecked_transaction()?;
        validate_format(&tx)?;
        let at = match at {
            Some(at) => version_in(&tx, at)?.number,
            None => head_in(&tx)?.number,
        };
        let rows = tx
            .prepare_cached(
                "SELECT version,kind,content FROM changes WHERE path=?1 AND version<=?2 \
                 ORDER BY version",
            )?
            .query_map(params![path.as_str(), at.sql()], |r| {
                Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut history = Vec::with_capacity(rows.len());
        for (version, kind, content) in rows {
            history.push((
                VersionNumber::decode(version)?,
                decode_entry(kind, content)?,
            ));
        }
        let Some((_, Some(_))) = history.last() else {
            return Ok(None);
        };
        let start = history
            .iter()
            .rposition(|(_, entry)| entry.is_none())
            .map_or(0, |i| i + 1);
        // Each line's owner is an index into `history`; lines are re-split from
        // the previous version's bytes rather than copied.
        let (mut previous, mut owners) = (Vec::new(), Vec::<usize>::new());
        for (index, (_, entry)) in history.iter().enumerate().skip(start) {
            let content = entry.as_ref().expect("live after last deletion").content();
            let bytes = read_object(&tx, content, ObjectKind::Blob)?;
            let old: Vec<&[u8]> = split(&previous).collect();
            let next: Vec<&[u8]> = split(&bytes).collect();
            owners = matching(&old, &next)
                .into_iter()
                .map(|from| from.map_or(index, |i| owners[i]))
                .collect();
            previous = bytes;
        }
        // Resolve only origins that still own a line, renumbered in World order.
        let mut remap = BTreeMap::new();
        for owner in &owners {
            remap.insert(*owner, 0);
        }
        let mut origins = Vec::with_capacity(remap.len());
        for (owner, slot) in remap.iter_mut() {
            *slot = origins.len();
            origins.push(origin_in(&tx, path, history[*owner].0)?);
        }
        tx.commit()?;
        Ok(Some(LineBlame {
            path: path.clone(),
            version: at,
            origins,
            lines: split(&previous)
                .zip(owners)
                .enumerate()
                .map(|(i, (line, owner))| BlameLine {
                    number: i + 1,
                    origin: remap[&owner],
                    text: String::from_utf8_lossy(line.strip_suffix(b"\n").unwrap_or(line))
                        .into_owned(),
                })
                .collect(),
        }))
    }
}

fn split(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split_inclusive(|&b| b == b'\n')
}

fn origin_in(conn: &Connection, path: &RepoPath, version: VersionNumber) -> Result<Origin> {
    let version = version_in(conn, version)?;
    let row: Option<(String, Option<String>)> = conn
        .prepare_cached("SELECT checkpoint,provenance FROM blame WHERE path=?1 AND version=?2")?
        .query_row(params![path.as_str(), version.number.sql()], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let layer: Option<String> = match &version.operation {
        Some(operation) => conn
            .prepare_cached("SELECT layer FROM publications WHERE id=?1 AND outcome=1")?
            .query_row([operation.as_str()], |r| r.get(0))
            .optional()?,
        None => None,
    };
    let (checkpoint, provenance) = match row {
        Some((checkpoint, provenance)) => (
            Some(CheckpointId::parse(&checkpoint).map_err(|_| corrupt("invalid blame row"))?),
            provenance
                .map(|id| read_provenance(conn, decode_id(&id)?))
                .transpose()?,
        ),
        None => (None, None),
    };
    Ok(Origin {
        version,
        layer: layer
            .map(|l| LayerId::parse(&l).map_err(|_| corrupt("invalid publication Layer")))
            .transpose()?,
        checkpoint,
        provenance,
    })
}

fn corrupt(message: &str) -> Error {
    Error::CorruptRepository(message.into())
}

/// Records the published Layer's per-path attribution for World `version`, from
/// the radar index rows, inside Publish's write transaction. O(change).
pub(crate) fn record_in(
    conn: &Connection,
    layer: &LayerId,
    version: VersionNumber,
    changes: &SparseChanges,
) -> Result<()> {
    let tracked = crate::radar::attributed_in(conn, layer)?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO blame(path,version,checkpoint,provenance) VALUES(?1,?2,?3,?4)",
    )?;
    for path in changes.entries().keys() {
        let (checkpoint, provenance) = tracked
            .get(path.as_str())
            .ok_or_else(|| corrupt("published path missing from the Layer index"))?;
        insert.execute(params![
            path.as_str(),
            version.sql(),
            checkpoint.object().to_string(),
            provenance.map(|p| p.to_string())
        ])?;
    }
    Ok(())
}

/// Rebuilds every successful publication's attribution from its Layer history
/// and compares it with the stored `blame` rows.
pub(crate) fn check_blame(checkpoints: &mut CheckpointReader<'_>) -> Result<()> {
    let conn = checkpoints.conn;
    let published = conn
        .prepare("SELECT layer,expected,result FROM publications WHERE outcome=1 ORDER BY result")?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut expected_rows = 0;
    for (layer, revision, version) in published {
        let layer = LayerId::parse(&layer).map_err(|_| corrupt("invalid publication Layer"))?;
        let revision = u64::try_from(revision).map_err(|_| corrupt("invalid revision"))?;
        let derived = attribution_in(checkpoints, &layer, revision)?;
        let stored: BTreeMap<String, (String, Option<String>)> = conn
            .prepare_cached("SELECT path,checkpoint,provenance FROM blame WHERE version=?1")?
            .query_map([version], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
            .collect::<std::result::Result<_, _>>()?;
        let changes = crate::repository::changeset_in(conn, VersionNumber::decode(version)?)?;
        expected_rows += changes.entries().len();
        for path in changes.entries().keys() {
            let (_, checkpoint, provenance) = derived
                .get(path)
                .ok_or_else(|| corrupt("published path has no Layer attribution"))?;
            let row = (
                checkpoint.object().to_string(),
                provenance.map(|p| p.to_string()),
            );
            if stored.get(path.as_str()) != Some(&row) {
                return Err(corrupt("blame disagrees with Layer history"));
            }
        }
    }
    let rows: i64 = conn.query_row("SELECT count(*) FROM blame", [], |r| r.get(0))?;
    if rows != expected_rows as i64 {
        return Err(corrupt("blame names an unpublished change"));
    }
    Ok(())
}

/// Longest edit-free correspondence between two line lists (Myers' O(ND) diff):
/// for each line of `new`, the index of the unchanged `old` line it continues.
/// Beyond `MAX_EDITS` edits in the middle, the middle is treated as rewritten.
pub(crate) fn matching<'a>(old: &[&'a [u8]], new: &[&'a [u8]]) -> Vec<Option<usize>> {
    const MAX_EDITS: usize = 2048;
    let mut result = vec![None; new.len()];
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    for (i, slot) in result.iter_mut().enumerate().take(prefix) {
        *slot = Some(i);
    }
    for k in 0..suffix {
        result[new.len() - 1 - k] = Some(old.len() - 1 - k);
    }
    let (a, b) = (
        &old[prefix..old.len() - suffix],
        &new[prefix..new.len() - suffix],
    );
    // Compare interned line numbers rather than bytes.
    let mut ids: HashMap<&'a [u8], u32> = HashMap::new();
    let mut intern = |line: &'a [u8]| {
        let next = ids.len() as u32;
        *ids.entry(line).or_insert(next)
    };
    let a: Vec<u32> = a.iter().map(|line| intern(line)).collect();
    let b: Vec<u32> = b.iter().map(|line| intern(line)).collect();
    for (i, j) in myers(&a, &b, MAX_EDITS) {
        result[prefix + j] = Some(prefix + i);
    }
    result
}

/// Matched index pairs of a shortest edit script, or none past `limit` edits.
fn myers(a: &[u32], b: &[u32], limit: usize) -> Vec<(usize, usize)> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (a.len() + b.len()).min(limit) as isize;
    let offset = max + 1;
    let mut v = vec![0isize; 2 * offset as usize + 1];
    // trace[d] holds V after step d for k in -d..=d.
    let mut trace: Vec<Vec<isize>> = Vec::new();
    for d in 0..=max {
        for k in (-d..=d).step_by(2) {
            let down =
                k == -d || (k != d && v[(offset + k - 1) as usize] < v[(offset + k + 1) as usize]);
            let mut x = if down {
                v[(offset + k + 1) as usize]
            } else {
                v[(offset + k - 1) as usize] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[(offset + k) as usize] = x;
            if x >= n && y >= m {
                trace.push(v[(offset - d) as usize..=(offset + d) as usize].to_vec());
                return backtrack(&trace, n, m);
            }
        }
        trace.push(v[(offset - d) as usize..=(offset + d) as usize].to_vec());
    }
    Vec::new()
}

fn backtrack(trace: &[Vec<isize>], n: isize, m: isize) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..trace.len() as isize).rev() {
        let k = x - y;
        let (prev_x, prev_y) = if d == 0 {
            (0, 0)
        } else {
            let prev = &trace[d as usize - 1];
            let at = |k: isize| prev[(k + d - 1) as usize];
            let down = k == -d || (k != d && at(k - 1) < at(k + 1));
            let prev_k = if down { k + 1 } else { k - 1 };
            (at(prev_k), at(prev_k) - prev_k)
        };
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            pairs.push((x as usize, y as usize));
        }
        (x, y) = (prev_x, prev_y);
    }
    pairs.reverse();
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcs(a: &[u32], b: &[u32]) -> usize {
        let mut t = vec![vec![0; b.len() + 1]; a.len() + 1];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                t[i][j] = if a[i] == b[j] {
                    t[i + 1][j + 1] + 1
                } else {
                    t[i + 1][j].max(t[i][j + 1])
                };
            }
        }
        t[0][0]
    }

    #[test]
    fn myers_finds_a_longest_common_subsequence() {
        let mut seed = 0x853c_49e6_748f_ea9bu64;
        let mut next = move |bound: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % bound
        };
        for _ in 0..2000 {
            let a: Vec<u32> = (0..next(12)).map(|_| next(4) as u32).collect();
            let b: Vec<u32> = (0..next(12)).map(|_| next(4) as u32).collect();
            let pairs = myers(&a, &b, usize::MAX);
            assert_eq!(pairs.len(), lcs(&a, &b), "{a:?} {b:?}");
            assert!(pairs.iter().all(|&(i, j)| a[i] == b[j]));
            assert!(pairs.windows(2).all(|w| w[0].0 < w[1].0 && w[0].1 < w[1].1));
        }
    }

    #[test]
    fn matching_keeps_unchanged_lines_and_caps_edits() {
        let old: Vec<&[u8]> = vec![b"a\n", b"b\n", b"c\n", b"d\n"];
        let new: Vec<&[u8]> = vec![b"a\n", b"x\n", b"c\n", b"d\n", b"e"];
        assert_eq!(
            matching(&old, &new),
            [Some(0), None, Some(2), Some(3), None]
        );
        assert_eq!(matching(&[], &new), [None; 5]);
        assert_eq!(matching(&old, &[]), Vec::<Option<usize>>::new());
        // Past the edit cap the middle counts as rewritten; the ends still match.
        let lines: Vec<Vec<u8>> = (0..5000).map(|i| format!("{i}\n").into_bytes()).collect();
        let old: Vec<&[u8]> = lines.iter().map(Vec::as_slice).collect();
        let mut new = old.clone();
        new.reverse();
        new.insert(0, old[0]);
        let kept = matching(&old, &new);
        assert_eq!(kept[0], Some(0));
        assert_eq!(kept.iter().flatten().count(), 1);
    }
}
