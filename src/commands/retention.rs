use super::*;

pub(super) fn gc(store: &mut Store, dry_run: bool, json_output: bool) -> Result<()> {
    let config = Config::load(&store.root)?;
    let current_time = now();
    let trace_cutoff = chrono::Utc::now()
        .checked_sub_signed(crate::config::retention_duration(
            config.retention.raw_trace_days,
            "retention.raw_trace_days",
        )?)
        .ok_or_else(|| JavelinError::policy("retention.raw_trace_days is too large"))?
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let expired_provenance = store.expired_provenance_sessions(&trace_cutoff)?;
    let mut expired_discarded = store.expired_discarded_layers(&current_time)?;
    let mut claims_expired = 0;
    let mut discarded_purged = Vec::new();
    if !dry_run {
        claims_expired = store.expire_claims(&current_time)?;
        for session in &expired_provenance {
            store.purge_provenance(session)?;
        }
        while !expired_discarded.is_empty() {
            let current_batch = std::mem::take(&mut expired_discarded);
            let mut remaining = Vec::new();
            let mut progressed = false;
            for layer_id in current_batch {
                if store.all_children(&layer_id)?.is_empty() {
                    let trash = store.metadata.join("trash").join(&layer_id);
                    store.purge_layer(&layer_id)?;
                    if trash.exists() {
                        fs::remove_dir_all(&trash)
                            .jctx("DISCARD_IO", "cannot purge expired retained view")?;
                    }
                    discarded_purged.push(layer_id);
                    progressed = true;
                } else {
                    remaining.push(layer_id);
                }
            }
            if !progressed {
                break;
            }
            expired_discarded = remaining;
        }
    }
    let reachable = collect_reachability(store)?.all();
    let all = store.objects.all_ids()?;
    let unreachable = all
        .into_iter()
        .filter(|id| !reachable.contains(id))
        .collect::<Vec<_>>();
    if !dry_run {
        let tx = store
            .conn
            .transaction()
            .jctx("STORE_TX", "cannot begin GC metadata removal")?;
        for id in &unreachable {
            tx.execute("DELETE FROM object_metadata WHERE id = ?1", [id])
                .jctx("STORE_WRITE", "cannot remove GC metadata")?;
        }
        tx.commit()
            .jctx("STORE_TX", "cannot commit GC metadata removal")?;
        for id in &unreachable {
            store.objects.remove(id)?;
        }
        store.append_event(
            "gc.completed",
            Some("world"),
            None,
            &json!({
                "objects_removed": unreachable.len(),
                "discarded_layers_purged": discarded_purged,
                "provenance_sessions_purged": expired_provenance,
                "claims_expired": claims_expired,
            }),
        )?;
    }
    emit(
        json_output,
        &json!({
            "dry_run": dry_run,
            "reachable": reachable.len(),
            "unreachable": unreachable,
            "expired_discarded_layers": if dry_run { expired_discarded } else { discarded_purged.clone() },
            "expired_provenance_sessions": expired_provenance,
            "claims_expired": claims_expired,
        }),
        format!(
            "{} unreachable objects{}",
            unreachable.len(),
            if dry_run { " (dry run)" } else { " removed" }
        ),
    )
}
pub(super) fn events(store: &Store, since: i64, follow: bool, jsonl: bool) -> Result<()> {
    let events = store.events_since(since)?;
    if jsonl {
        let mut cursor = since;
        for event in &events {
            println!("{event}");
            std::io::stdout()
                .flush()
                .jctx("OUTPUT_IO", "cannot flush event stream")?;
            cursor = event["cursor"].as_i64().unwrap_or(cursor);
        }
        if follow {
            loop {
                thread::sleep(Duration::from_millis(250));
                for event in store.events_since(cursor)? {
                    println!("{event}");
                    std::io::stdout()
                        .flush()
                        .jctx("OUTPUT_IO", "cannot flush event stream")?;
                    cursor = event["cursor"].as_i64().unwrap_or(cursor);
                }
            }
        }
        Ok(())
    } else {
        if follow {
            return Err(JavelinError::invalid("--follow requires --jsonl or --json"));
        }
        emit(
            false,
            &events,
            events
                .iter()
                .map(|event| {
                    format!(
                        "{}\t{}",
                        event["cursor"],
                        event["type"].as_str().unwrap_or("unknown")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }
}
