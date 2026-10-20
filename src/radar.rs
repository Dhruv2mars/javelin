//! Collision radar: which paths would make a Layer's Publish conflict, found before
//! Publish. `layer_paths` indexes every live Layer's changed paths by path. It is
//! derived data, maintained in the same transaction as each Layer head change and
//! cleared by Publish and discard; `check()` rebuilds it and compares. Each row
//! also names the checkpoint, and its provenance, that last set the entry, which
//! Publish copies into trace blame.
use std::collections::BTreeMap;

use rusqlite::{Connection, params};
use serde::Serialize;

use crate::{
    CheckpointId, Error, FileEntry, LayerHead, LayerId, LayerState, ObjectId, RepoPath, Repository,
    Result, VersionNumber,
    domain::{LiveIndex, LivePath, collisions},
    layer::{Candidate, CheckpointReader, attribution_in, checkpoint_in},
    publish::layer_state_in,
    repository::{
        Index, decode_entry, decode_id, decode_path, encode_entry, head_in, validate_format,
    },
};

pub(crate) const SCHEMA: &str = "
CREATE TABLE layer_paths (
    path TEXT NOT NULL,
    layer TEXT NOT NULL,
    kind INTEGER CHECK(kind IN (1,2,3)),
    content TEXT REFERENCES objects(id),
    checkpoint TEXT NOT NULL REFERENCES objects(id),
    provenance TEXT REFERENCES objects(id),
    PRIMARY KEY(path,layer),
    CHECK((kind IS NULL)=(content IS NULL))
) STRICT, WITHOUT ROWID;
CREATE INDEX layer_paths_by_layer ON layer_paths(layer);
";

/// What would make this Layer's Publish conflict: World advances since its base
/// now, and each other live Layer if that Layer published first.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Radar {
    pub head: LayerHead,
    pub base: VersionNumber,
    pub world: WorldCollisions,
    pub layers: Vec<LayerCollision>,
}

impl Radar {
    pub fn clear(&self) -> bool {
        self.world.paths.is_empty() && self.layers.is_empty()
    }
}

/// The exact paths Publish would report as a conflict against World `version`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorldCollisions {
    pub version: VersionNumber,
    pub paths: Vec<RepoPath>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LayerCollision {
    pub layer: LayerId,
    pub paths: Vec<PathPair>,
}

/// `ours` is this Layer's changed path; `theirs` is the other Layer's, equal to it,
/// an ancestor or a descendant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PathPair {
    pub ours: RepoPath,
    pub theirs: RepoPath,
}

impl Repository {
    /// Reports the active Layer's would-conflict paths in one read snapshot. Work is
    /// O(the Layer's changed paths x depth) indexed lookups plus matches; it does not
    /// grow with repository size, history or the number of live Layers.
    pub fn radar(&self, layer: &LayerId) -> Result<Radar> {
        let tx = self.conn.unchecked_transaction()?;
        validate_format(&tx)?;
        let LayerState::Active { head } = layer_state_in(&tx, layer)? else {
            return Err(Error::InvalidInput("Layer is not active".into()));
        };
        let checkpoint = checkpoint_in(&tx, head.checkpoint().object())?;
        let base = checkpoint.base().version();
        let version = head_in(&tx)?.number;
        let index = Index::new(&tx);
        let paths = index.trees.intersections(
            index.root(base)?,
            index.root(version)?,
            checkpoint.changes(),
        )?;
        let layers = collisions(&index, &Live(&tx), layer, checkpoint.changes(), base)?;
        tx.commit()?;
        Ok(Radar {
            head,
            base,
            world: WorldCollisions {
                version,
                paths: paths.into_iter().collect(),
            },
            layers: layers
                .into_iter()
                .map(|(layer, pairs)| LayerCollision {
                    layer,
                    paths: pairs
                        .into_iter()
                        .map(|(ours, theirs)| PathPair { ours, theirs })
                        .collect(),
                })
                .collect(),
        })
    }
}

struct Live<'a>(&'a Connection);

impl LiveIndex for Live<'_> {
    fn live(&self, path: &RepoPath, below: bool) -> Result<Vec<LivePath>> {
        let row = |r: &rusqlite::Row<'_>| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get(2)?,
                r.get(3)?,
            ))
        };
        let rows = if below {
            let (start, end) = path.descendant_range();
            self.0
                .prepare_cached(
                    "SELECT layer,path,kind,content FROM layer_paths WHERE path>=?1 AND path<?2",
                )?
                .query_map(params![start, end], row)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            self.0
                .prepare_cached("SELECT layer,path,kind,content FROM layer_paths WHERE path=?1")?
                .query_map([path.as_str()], row)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        rows.into_iter()
            .map(|(layer, path, kind, content)| {
                Ok(LivePath {
                    layer: stored_layer(&layer)?,
                    path: decode_path(&path)?,
                    entry: decode_entry(kind, content)?,
                })
            })
            .collect()
    }
}

fn stored_layer(layer: &str) -> Result<LayerId> {
    LayerId::parse(layer).map_err(|_| Error::CorruptRepository("invalid indexed Layer".into()))
}

type Tracked = (Option<FileEntry>, CheckpointId, Option<ObjectId>);

/// This Layer's indexed changes and attribution, read through `layer_paths_by_layer`.
fn tracked_in(conn: &Connection, layer: &LayerId) -> Result<BTreeMap<String, Tracked>> {
    let rows = conn
        .prepare_cached(
            "SELECT path,kind,content,checkpoint,provenance FROM layer_paths WHERE layer=?1",
        )?
        .query_map([layer.as_str()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(path, kind, content, checkpoint, provenance)| {
            let checkpoint = CheckpointId::parse(&checkpoint)
                .map_err(|_| Error::CorruptRepository("invalid indexed checkpoint".into()))?;
            let provenance = provenance.as_deref().map(decode_id).transpose()?;
            Ok((path, (decode_entry(kind, content)?, checkpoint, provenance)))
        })
        .collect()
}

/// Each path's attributing checkpoint and provenance, for Publish to record.
pub(crate) fn attributed_in(
    conn: &Connection,
    layer: &LayerId,
) -> Result<BTreeMap<String, (CheckpointId, Option<ObjectId>)>> {
    Ok(tracked_in(conn, layer)?
        .into_iter()
        .map(|(path, (_, checkpoint, provenance))| (path, (checkpoint, provenance)))
        .collect())
}

/// Sets the Layer's indexed changes to its new head's, writing only differences.
/// A changed or new entry is attributed to the new head checkpoint; an unchanged
/// one keeps the checkpoint that set it.
pub(crate) fn track_in(conn: &Connection, layer: &LayerId, head: &Candidate) -> Result<()> {
    let mut stale = tracked_in(conn, layer)?;
    let mut upsert = conn.prepare_cached(
        "INSERT INTO layer_paths(path,layer,kind,content,checkpoint,provenance) \
         VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(path,layer) DO UPDATE SET \
         kind=excluded.kind,content=excluded.content,checkpoint=excluded.checkpoint,\
         provenance=excluded.provenance",
    )?;
    for (path, entry) in head.changes.entries() {
        if stale
            .remove(path.as_str())
            .is_none_or(|(before, ..)| &before != entry)
        {
            let (kind, content) = encode_entry(entry);
            upsert.execute(params![
                path.as_str(),
                layer.as_str(),
                kind,
                content,
                head.id.object().to_string(),
                head.provenance.map(|p| p.to_string())
            ])?;
        }
    }
    let mut delete = conn.prepare_cached("DELETE FROM layer_paths WHERE path=?1 AND layer=?2")?;
    for path in stale.keys() {
        delete.execute(params![path, layer.as_str()])?;
    }
    Ok(())
}

/// Removes a Layer that is no longer live (published or discarded).
pub(crate) fn untrack_in(conn: &Connection, layer: &LayerId) -> Result<()> {
    conn.prepare_cached("DELETE FROM layer_paths WHERE layer=?1")?
        .execute([layer.as_str()])?;
    Ok(())
}

/// Rebuilds the index, attribution included, from every live Layer's history
/// and compares it with the stored rows.
pub(crate) fn check_radar(checkpoints: &mut CheckpointReader<'_>) -> Result<()> {
    let conn = checkpoints.conn;
    let layers = conn
        .prepare("SELECT DISTINCT layer FROM layer_heads ORDER BY layer")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut expected = 0;
    for layer in layers {
        let layer = stored_layer(&layer)?;
        let derived = match layer_state_in(conn, &layer)? {
            LayerState::Active { head } => attribution_in(checkpoints, &layer, head.revision())?,
            _ => Default::default(),
        };
        let tracked = tracked_in(conn, &layer)?;
        expected += derived.len();
        if tracked.len() != derived.len()
            || derived
                .iter()
                .any(|(path, row)| tracked.get(path.as_str()) != Some(row))
        {
            return Err(Error::CorruptRepository(format!(
                "Layer path index disagrees with Layer {}",
                layer.as_str()
            )));
        }
    }
    let rows: i64 = conn.query_row("SELECT count(*) FROM layer_paths", [], |r| r.get(0))?;
    if rows != expected as i64 {
        return Err(Error::CorruptRepository(
            "Layer path index names an unknown Layer".into(),
        ));
    }
    Ok(())
}
