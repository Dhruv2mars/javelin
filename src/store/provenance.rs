use super::*;

impl Store {
    pub fn begin_provenance(
        &mut self,
        layer_id: Option<&str>,
        actor: &serde_json::Value,
    ) -> Result<String> {
        if let Some(layer_id) = layer_id {
            let _ = self.layer(layer_id)?;
        }
        let id = ulid::Ulid::new().to_string();
        self.conn
            .execute(
                "INSERT INTO provenance_sessions(id, layer_id, actor_json, status, started_at)
                 VALUES (?1, ?2, ?3, 'active', ?4)",
                params![id, layer_id, actor.to_string(), now()],
            )
            .jctx("STORE_WRITE", "cannot begin provenance session")?;
        self.append_event(
            "provenance.started",
            Some("provenance"),
            Some(&id),
            &json!({"layer_id": layer_id, "actor": actor}),
        )?;
        Ok(id)
    }

    pub fn append_provenance_event(
        &mut self,
        session_id: &str,
        event_type: &str,
        payload: &serde_json::Value,
    ) -> Result<String> {
        let (layer_id, actor_json, status): (Option<String>, String, String) = self
            .conn
            .query_row(
                "SELECT layer_id, actor_json, status FROM provenance_sessions WHERE id = ?1",
                [session_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .jctx("STORE_QUERY", "cannot read provenance session")?
            .ok_or_else(|| {
                JavelinError::invalid(format!("unknown provenance session {session_id}"))
            })?;
        if status != "active" {
            return Err(JavelinError::policy("provenance session has ended"));
        }
        let event_id = ulid::Ulid::new().to_string();
        self.conn
            .execute(
                "INSERT INTO provenance_events(event_id, session_id, layer_id, timestamp, actor_json,
                 event_type, payload_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![event_id, session_id, layer_id, now(), actor_json, event_type, payload.to_string()],
            )
            .jctx("STORE_WRITE", "cannot append provenance event")?;
        Ok(event_id)
    }

    pub fn attach_provenance(
        &mut self,
        session_id: &str,
        name: &str,
        media_type: Option<&str>,
        object_id: &str,
    ) -> Result<String> {
        let exists: bool = self
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM provenance_sessions WHERE id = ?1)",
                [session_id],
                |row| row.get(0),
            )
            .jctx("STORE_QUERY", "cannot inspect provenance session")?;
        if !exists {
            return Err(JavelinError::invalid(format!(
                "unknown provenance session {session_id}"
            )));
        }
        let id = ulid::Ulid::new().to_string();
        self.conn
            .execute(
                "INSERT INTO provenance_attachments(id, session_id, object_id, name, media_type,
                 purged, created_at) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
                params![id, session_id, object_id, name, media_type, now()],
            )
            .jctx("STORE_WRITE", "cannot attach provenance payload")?;
        Ok(id)
    }

    pub fn end_provenance(&mut self, session_id: &str) -> Result<()> {
        let changed = self
            .conn
            .execute(
                "UPDATE provenance_sessions SET status = 'ended', ended_at = ?1
                 WHERE id = ?2 AND status = 'active'",
                params![now(), session_id],
            )
            .jctx("STORE_WRITE", "cannot end provenance session")?;
        if changed != 1 {
            return Err(JavelinError::invalid(
                "provenance session is unknown or ended",
            ));
        }
        self.append_event(
            "provenance.ended",
            Some("provenance"),
            Some(session_id),
            &json!({}),
        )?;
        Ok(())
    }

    pub fn provenance_session(&self, session_id: &str) -> Result<serde_json::Value> {
        let session = self
            .conn
            .query_row(
                "SELECT id, layer_id, actor_json, status, started_at, ended_at
                 FROM provenance_sessions WHERE id = ?1",
                [session_id],
                |row| {
                    let actor: String = row.get(2)?;
                    Ok(json!({
                        "id": row.get::<_, String>(0)?,
                        "layer_id": row.get::<_, Option<String>>(1)?,
                        "actor": serde_json::from_str::<serde_json::Value>(&actor).unwrap_or(json!({})),
                        "status": row.get::<_, String>(3)?,
                        "started_at": row.get::<_, String>(4)?,
                        "ended_at": row.get::<_, Option<String>>(5)?,
                    }))
                },
            )
            .optional()
            .jctx("STORE_QUERY", "cannot read provenance session")?
            .ok_or_else(|| JavelinError::invalid(format!("unknown provenance session {session_id}")))?;
        let mut statement = self
            .conn
            .prepare(
                "SELECT event_id, layer_id, timestamp, actor_json, event_type, payload_json FROM provenance_events
                 WHERE session_id = ?1 ORDER BY timestamp, event_id",
            )
            .jctx("STORE_QUERY", "cannot prepare provenance event query")?;
        let events = statement
            .query_map([session_id], |row| {
                let actor: String = row.get(3)?;
                let payload: String = row.get(5)?;
                Ok(json!({
                    "schema": "javelin.provenance.v1",
                    "event_id": row.get::<_, String>(0)?,
                    "session_id": session_id,
                    "layer_id": row.get::<_, Option<String>>(1)?,
                    "timestamp": row.get::<_, String>(2)?,
                    "actor": serde_json::from_str::<serde_json::Value>(&actor).unwrap_or(json!({})),
                    "type": row.get::<_, String>(4)?,
                    "payload": serde_json::from_str::<serde_json::Value>(&payload).unwrap_or(json!({})),
                }))
            })
            .jctx("STORE_QUERY", "cannot read provenance events")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .jctx("STORE_QUERY", "cannot decode provenance events")?;
        Ok(json!({"session": session, "events": events}))
    }

    pub fn search_provenance(&self, query: &str) -> Result<Vec<serde_json::Value>> {
        let escaped = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let mut statement = self
            .conn
            .prepare(
                "SELECT DISTINCT s.id, s.layer_id, s.actor_json, s.status, s.started_at
                 FROM provenance_sessions s LEFT JOIN provenance_events e ON e.session_id = s.id
                 WHERE s.id LIKE ?1 ESCAPE '\\' OR s.actor_json LIKE ?1 ESCAPE '\\'
                    OR e.payload_json LIKE ?1 ESCAPE '\\' OR e.event_type LIKE ?1 ESCAPE '\\'
                 ORDER BY s.started_at",
            )
            .jctx("STORE_QUERY", "cannot prepare provenance search")?;
        let rows = statement
            .query_map([pattern], |row| {
                let actor: String = row.get(2)?;
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "layer_id": row.get::<_, Option<String>>(1)?,
                    "actor": serde_json::from_str::<serde_json::Value>(&actor).unwrap_or(json!({})),
                    "status": row.get::<_, String>(3)?,
                    "started_at": row.get::<_, String>(4)?,
                }))
            })
            .jctx("STORE_QUERY", "cannot search provenance")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .jctx("STORE_QUERY", "cannot decode provenance search")
    }

    pub fn purge_provenance(&mut self, session_id: &str) -> Result<()> {
        let tx = self
            .conn
            .transaction()
            .jctx("STORE_TX", "cannot begin provenance purge")?;
        tx.execute(
            "UPDATE provenance_events SET payload_json = '{\"purged\":true}' WHERE session_id = ?1",
            [session_id],
        )
        .jctx("STORE_WRITE", "cannot purge provenance events")?;
        tx.execute(
            "UPDATE provenance_attachments SET object_id = NULL, purged = 1 WHERE session_id = ?1",
            [session_id],
        )
        .jctx("STORE_WRITE", "cannot purge provenance attachments")?;
        tx.commit()
            .jctx("STORE_TX", "cannot commit provenance purge")?;
        self.append_event(
            "provenance.purged",
            Some("provenance"),
            Some(session_id),
            &json!({}),
        )?;
        Ok(())
    }

    pub fn expired_provenance_sessions(&self, cutoff: &str) -> Result<Vec<String>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id FROM provenance_sessions WHERE started_at <= ?1 AND
                 (EXISTS(SELECT 1 FROM provenance_events e WHERE e.session_id = provenance_sessions.id
                  AND e.payload_json != '{\"purged\":true}') OR
                  EXISTS(SELECT 1 FROM provenance_attachments a WHERE a.session_id = provenance_sessions.id
                  AND a.purged = 0)) ORDER BY started_at",
            )
            .jctx("STORE_QUERY", "cannot prepare expired provenance query")?;
        let rows = statement
            .query_map([cutoff], |row| row.get(0))
            .jctx("STORE_QUERY", "cannot read expired provenance sessions")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .jctx("STORE_QUERY", "cannot decode expired provenance sessions")
    }
}
