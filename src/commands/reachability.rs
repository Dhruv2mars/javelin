use super::*;

pub(super) struct Reachability {
    pub roots: BTreeSet<String>,
    pub blobs: BTreeSet<String>,
}

impl Reachability {
    pub fn all(&self) -> BTreeSet<String> {
        self.roots.union(&self.blobs).cloned().collect()
    }
}

pub(super) fn collect_reachability(store: &Store) -> Result<Reachability> {
    let mut roots = BTreeSet::new();
    let mut blobs = BTreeSet::new();
    for version in store.world_history()? {
        roots.insert(version.root_tree);
    }
    for layer in store.layers(true)? {
        for checkpoint in store.checkpoint_history(&layer.id)? {
            roots.insert(checkpoint.root_tree);
        }
    }
    for root in &roots {
        let tree = store.objects.read_tree(root)?;
        for entry in tree.entries {
            match entry.kind {
                EntryKind::Directory if entry.object_id.is_some() || entry.executable => {
                    return Err(JavelinError::corruption(format!(
                        "directory {} has invalid portable metadata",
                        entry.path
                    )));
                }
                EntryKind::File | EntryKind::Symlink => {
                    blobs.insert(entry.object_id.ok_or_else(|| {
                        JavelinError::corruption(format!(
                            "tracked path {} has no blob reference",
                            entry.path
                        ))
                    })?);
                }
                EntryKind::Directory => {}
            }
        }
    }
    for query in [
        "SELECT stdout_object FROM validation_runs WHERE stdout_object IS NOT NULL",
        "SELECT stderr_object FROM validation_runs WHERE stderr_object IS NOT NULL",
        "SELECT object_id FROM provenance_attachments WHERE object_id IS NOT NULL",
    ] {
        let mut statement = store.conn.prepare(query).jctx(
            7,
            "STORE_QUERY",
            "cannot prepare reachability query",
        )?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .jctx(7, "STORE_QUERY", "cannot read reachable objects")?;
        for row in rows {
            blobs.insert(row.jctx(7, "STORE_QUERY", "cannot decode reachable object")?);
        }
    }
    for conflict in store.conflicts(None, true)? {
        for entry in [
            conflict.base_entry,
            conflict.target_entry,
            conflict.private_entry,
        ]
        .into_iter()
        .flatten()
        {
            if let Some(object_id) = entry.object_id {
                blobs.insert(object_id);
            }
        }
    }
    Ok(Reachability { roots, blobs })
}
