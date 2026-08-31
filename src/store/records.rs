use super::*;

pub(super) fn world_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorldVersion> {
    Ok(WorldVersion {
        id: row.get(0)?,
        sequence: row.get(1)?,
        parent_version: row.get(2)?,
        root_tree: row.get(3)?,
        accepted_contribution: row.get(4)?,
        created_at: row.get(5)?,
    })
}

pub(super) fn layer_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Layer> {
    let target_kind = row
        .get::<_, String>(5)?
        .parse()
        .map_err(|message: String| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                std::io::Error::new(std::io::ErrorKind::InvalidData, message).into(),
            )
        })?;
    let status = row
        .get::<_, String>(7)?
        .parse()
        .map_err(|message: String| {
            rusqlite::Error::FromSqlConversionFailure(
                7,
                rusqlite::types::Type::Text,
                std::io::Error::new(std::io::ErrorKind::InvalidData, message).into(),
            )
        })?;
    Ok(Layer {
        id: row.get(0)?,
        name: row.get(1)?,
        origin_ref: row.get(2)?,
        synchronized_ref: row.get(3)?,
        head_checkpoint: row.get(4)?,
        target_kind,
        target_id: row.get(6)?,
        status,
        view_path: row.get(8)?,
        created_at: row.get(9)?,
    })
}

pub(super) fn checkpoint_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Checkpoint> {
    Ok(Checkpoint {
        id: row.get(0)?,
        layer_id: row.get(1)?,
        sequence: row.get(2)?,
        previous_checkpoint: row.get(3)?,
        root_tree: row.get(4)?,
        synchronized_ref: row.get(5)?,
        reason: row.get(6)?,
        created_at: row.get(7)?,
    })
}

pub(super) fn conflict_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConflictRecord> {
    let base: Option<String> = row.get(4)?;
    let target: Option<String> = row.get(5)?;
    let private: Option<String> = row.get(6)?;
    Ok(ConflictRecord {
        id: row.get(0)?,
        layer_id: row.get(1)?,
        path: row.get(2)?,
        conflict_type: row.get(3)?,
        base_entry: decode_optional(base),
        target_entry: decode_optional(target),
        private_entry: decode_optional(private),
        target_ref: row.get(7)?,
        status: row.get(8)?,
        resolution: row.get(9)?,
        created_at: row.get(10)?,
    })
}

pub(super) fn encode_optional(entry: &Option<TreeEntry>) -> Result<Option<String>> {
    entry
        .as_ref()
        .map(|entry| {
            serde_json::to_string(entry).map_err(|error| {
                JavelinError::corruption(format!("cannot encode path state: {error}"))
            })
        })
        .transpose()
}

pub(super) fn decode_optional(value: Option<String>) -> Option<TreeEntry> {
    value.and_then(|value| serde_json::from_str(&value).ok())
}

pub(super) fn append_event_tx(
    tx: &Transaction<'_>,
    event_type: &str,
    subject_kind: Option<&str>,
    subject_id: Option<&str>,
    payload: &serde_json::Value,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO events(event_type, subject_kind, subject_id, payload_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            event_type,
            subject_kind,
            subject_id,
            payload.to_string(),
            now()
        ],
    )
    .jctx("STORE_WRITE", "cannot append event")?;
    Ok(tx.last_insert_rowid())
}

pub(super) fn append_event_conn(
    conn: &Connection,
    event_type: &str,
    subject_kind: Option<&str>,
    subject_id: Option<&str>,
    payload: &serde_json::Value,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO events(event_type, subject_kind, subject_id, payload_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            event_type,
            subject_kind,
            subject_id,
            payload.to_string(),
            now()
        ],
    )
    .jctx("STORE_WRITE", "cannot append event")?;
    Ok(conn.last_insert_rowid())
}
