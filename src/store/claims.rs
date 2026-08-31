use super::*;

impl Store {
    pub fn expire_claims(&mut self, cutoff: &str) -> Result<usize> {
        self.conn
            .execute(
                "UPDATE claims SET released_at = ?1 WHERE released_at IS NULL AND expires_at <= ?1",
                [cutoff],
            )
            .jctx("STORE_WRITE", "cannot expire Claims")
    }

    pub fn create_claim(&mut self, layer_id: &str, resource: &str, seconds: u64) -> Result<String> {
        let layer = self.layer(layer_id)?;
        let id = ulid::Ulid::new().to_string();
        let expires = (Utc::now() + chrono::Duration::seconds(seconds as i64))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.conn
            .execute(
                "INSERT INTO claims(id, layer_id, resource, kind, expires_at, released_at, created_at)
                 VALUES (?1, ?2, ?3, 'path', ?4, NULL, ?5)",
                params![id, layer.id, resource, expires, now()],
            )
            .jctx("STORE_WRITE", "cannot create Claim")?;
        self.append_event(
            "claim.created",
            Some("claim"),
            Some(&id),
            &json!({"layer_id": layer.id, "resource": resource, "expires_at": expires}),
        )?;
        Ok(id)
    }

    pub fn claims(&self) -> Result<Vec<serde_json::Value>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT c.id, c.layer_id, l.name, c.resource, c.kind, c.expires_at, c.released_at,
                 c.created_at FROM claims c JOIN layers l ON l.id = c.layer_id
                 WHERE c.released_at IS NULL AND c.expires_at > ?1 ORDER BY c.created_at",
            )
            .jctx("STORE_QUERY", "cannot prepare Claim list")?;
        let rows = statement
            .query_map([now()], |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "layer_id": row.get::<_, String>(1)?,
                    "layer_name": row.get::<_, String>(2)?,
                    "resource": row.get::<_, String>(3)?,
                    "kind": row.get::<_, String>(4)?,
                    "expires_at": row.get::<_, String>(5)?,
                    "released_at": row.get::<_, Option<String>>(6)?,
                    "created_at": row.get::<_, String>(7)?,
                }))
            })
            .jctx("STORE_QUERY", "cannot read Claims")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .jctx("STORE_QUERY", "cannot decode Claims")
    }

    pub fn renew_claim(&mut self, id: &str, seconds: u64) -> Result<String> {
        let expires = (Utc::now() + chrono::Duration::seconds(seconds as i64))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let changed = self
            .conn
            .execute(
                "UPDATE claims SET expires_at = ?1 WHERE id = ?2 AND released_at IS NULL",
                params![expires, id],
            )
            .jctx("STORE_WRITE", "cannot renew Claim")?;
        if changed != 1 {
            return Err(JavelinError::invalid("Claim is unknown or released"));
        }
        self.append_event(
            "claim.renewed",
            Some("claim"),
            Some(id),
            &json!({"expires_at": expires}),
        )?;
        Ok(expires)
    }

    pub fn release_claim(&mut self, id: &str) -> Result<()> {
        let changed = self
            .conn
            .execute(
                "UPDATE claims SET released_at = ?1 WHERE id = ?2 AND released_at IS NULL",
                params![now(), id],
            )
            .jctx("STORE_WRITE", "cannot release Claim")?;
        if changed != 1 {
            return Err(JavelinError::invalid("Claim is unknown or released"));
        }
        self.append_event("claim.released", Some("claim"), Some(id), &json!({}))?;
        Ok(())
    }
}
