//! The node's budget (`docs/direction.md`, What a node needs, 8): a limit per provider quota a
//! write can cross, set by the node's owner, with defaults from the Cloudflare Free plan
//! (`tools/node-cloudflare/README.md`). A write that would cross one is refused with
//! `quota_exceeded` before it is served, so it changes nothing and can be retried once the
//! owner raises the budget or the day's count resets. The self-hosted node and the Durable
//! Object run this same code over their own storage.
//!
//! Use is measured on the node's two SQLite databases, whatever stores them: storage is their
//! pages, and rows written are the pages each request writes, counted as 64 KiB rows, the
//! page row a Durable Object keeps (`crates/javelin-node-cloudflare/src/pages.rs`). Before a
//! write, its cost is estimated from its body plus a reserve for the pages it touches besides
//! (indexes, records, composed trees), so a write is refused while the estimate does not fit.
//! Admin pushes, such as provisioning, are not counted.

use std::io;

use javelin_protocol::{
    Message, ProtocolError,
    codec::{Meter, Usage, code},
    limits::MAX_BLOB,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{FileWorldLog, auth::Gate, storage::Storage};

/// The owner's budget, a document in the node's storage.
pub const BUDGET_FILE: &str = "budget.json";
/// A Durable Object page row: the unit rows written are counted in.
pub const ROW: u64 = 64 << 10;
/// Rows a write may touch besides its body: B-tree pages, the node's records and the
/// ledger, and the trees a Publish composes.
const RESERVE_ROWS: u64 = 16;
/// Seconds in one UTC day. Cloudflare's daily quotas reset at 00:00 UTC.
const DAY: u64 = 86_400;

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS budget_rows (
    day INTEGER PRIMARY KEY,
    rows INTEGER NOT NULL
);";

/// Limits on the node's use. Fields absent from `budget.json` keep their default.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Budget {
    /// Bytes the repository and the node's records may take. Default 1 GB, the documented
    /// per-object storage on the Free plan.
    pub storage_bytes: u64,
    /// 64 KiB rows the node may write per UTC day. Default 100,000, the Free plan's daily
    /// rows written; it is per account, so an owner with several repositories divides it.
    pub rows_written_per_day: u64,
    /// The largest object one upload may carry. Default the protocol's own bound.
    pub largest_object: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            storage_bytes: 1_000_000_000,
            rows_written_per_day: 100_000,
            largest_object: MAX_BLOB as u64,
        }
    }
}

/// The names `quota_exceeded` and `Usage` give each budget.
pub const STORAGE_BYTES: &str = "storage_bytes";
pub const ROWS_WRITTEN_PER_DAY: &str = "rows_written_per_day";
pub const LARGEST_OBJECT: &str = "largest_object";

/// The budget in `storage`, or the default when the owner never wrote one. A document that
/// does not parse is `InvalidData`, so a mistyped budget stops the node instead of being
/// ignored.
pub fn stored(storage: &dyn Storage) -> io::Result<Budget> {
    match storage.read(BUDGET_FILE)? {
        Some(bytes) => parse(&bytes),
        None => Ok(Budget::default()),
    }
}

/// A budget in the `budget.json` layout.
pub fn parse(bytes: &[u8]) -> io::Result<Budget> {
    serde_json::from_slice(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Replaces the stored budget.
pub fn store(storage: &dyn Storage, budget: &Budget) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(budget).map_err(io::Error::other)?;
    storage.write(BUDGET_FILE, &bytes)
}

/// The UTC day of Unix time `now`.
pub fn day(now: u64) -> u64 {
    now / DAY
}

/// The node's use against each budget at `now`.
pub fn usage(log: &FileWorldLog, now: u64) -> Result<Usage, ProtocolError> {
    let budget = log.budget();
    let (bytes, _) = stored_bytes(log)?;
    let rows = rows_today(log.records(), now)?;
    Ok(Usage {
        day: day(now),
        meters: vec![
            Meter {
                budget: STORAGE_BYTES.into(),
                used: Some(bytes),
                limit: budget.storage_bytes,
            },
            Meter {
                budget: ROWS_WRITTEN_PER_DAY.into(),
                used: Some(rows),
                limit: budget.rows_written_per_day,
            },
            Meter {
                budget: LARGEST_OBJECT.into(),
                used: None,
                limit: budget.largest_object,
            },
        ],
    })
}

/// Refuses a write whose estimated cost would cross the budget, before anything is stored.
/// `body` is the request's length. The refusal names the budget; only a maintainer, who may
/// read the whole repository, also learns the use and the limit. A caller the gate refuses
/// gets that refusal instead, so a refusal never tells it more than the write would have.
pub fn admit(
    log: &FileWorldLog,
    gate: &Gate,
    token: Option<&str>,
    now: u64,
    message: &Message,
    body: usize,
) -> Result<(), ProtocolError> {
    if !writes(message) {
        return Ok(());
    }
    let budget = log.budget();
    let body = body as u64;
    let crossing = if let Some(size) = largest_object(message)
        && size > budget.largest_object
    {
        Some((
            LARGEST_OBJECT,
            format!(
                "an object of {size} bytes is larger than the {} allowed",
                budget.largest_object
            ),
        ))
    } else {
        let (bytes, page_size) = stored_bytes(log)?;
        let cost = body + RESERVE_ROWS * page_size;
        if bytes.saturating_add(cost) > budget.storage_bytes {
            Some((
                STORAGE_BYTES,
                format!(
                    "{bytes} of {} bytes stored; this write needs up to {cost} more",
                    budget.storage_bytes
                ),
            ))
        } else {
            let rows = rows_today(log.records(), now)?;
            let cost = body.div_ceil(ROW) + RESERVE_ROWS;
            (rows.saturating_add(cost) > budget.rows_written_per_day).then(|| {
                (
                    ROWS_WRITTEN_PER_DAY,
                    format!(
                        "{rows} of {} rows written today (UTC); this write needs up to {cost} more",
                        budget.rows_written_per_day
                    ),
                )
            })
        }
    };
    let Some((name, numbers)) = crossing else {
        return Ok(());
    };
    let grant = gate.check(token, now, crate::wire::need(message))?;
    let detail = if gate.reader(&grant).reads_all() {
        format!("{name}: {numbers}")
    } else {
        name.to_string()
    };
    Err(ProtocolError::new(code::QUOTA_EXCEEDED, detail))
}

/// Adds the rows this request wrote to today's count. The count's own write is one more
/// row, counted here, so a request that wrote nothing leaves the count as it was.
pub fn meter(log: &FileWorldLog, now: u64) -> Result<(), ProtocolError> {
    let records = log.records();
    let repository = log.repo().database_use()?;
    let mut rows = rows_of(log.repo().take_pages_written()?, repository.page_size);
    let own = javelin::database_use(records)?;
    rows += rows_of(javelin::take_pages_written(records)?, own.page_size);
    if rows == 0 {
        return Ok(());
    }
    let today = day(now);
    records
        .execute(
            "INSERT INTO budget_rows (day, rows) VALUES (?1, ?2)
             ON CONFLICT (day) DO UPDATE SET rows = rows + excluded.rows",
            params![today as i64, (rows + 1) as i64],
        )
        .map_err(storage)?;
    records
        .execute("DELETE FROM budget_rows WHERE day < ?1", [today as i64])
        .map_err(storage)?;
    javelin::take_pages_written(records)?;
    Ok(())
}

/// Pages of `page_size` bytes as 64 KiB rows, rounded up, and one more for a database that
/// wrote any: a Durable Object writes the file's size row when the file grows.
fn rows_of(pages: u64, page_size: u64) -> u64 {
    match pages {
        0 => 0,
        pages => (pages * page_size).div_ceil(ROW) + 1,
    }
}

/// Bytes the two databases take, and the repository's page size.
fn stored_bytes(log: &FileWorldLog) -> Result<(u64, u64), ProtocolError> {
    let repository = log.repo().database_use()?;
    let records = javelin::database_use(log.records())?;
    Ok((repository.bytes + records.bytes, repository.page_size))
}

fn rows_today(records: &Connection, now: u64) -> Result<u64, ProtocolError> {
    let rows: Option<i64> = records
        .query_row(
            "SELECT rows FROM budget_rows WHERE day = ?1",
            [day(now) as i64],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    Ok(rows.unwrap_or(0).max(0) as u64)
}

/// Messages that store something. Reads, `Want` among them, are never refused here.
fn writes(message: &Message) -> bool {
    matches!(
        message,
        Message::Objects(_)
            | Message::LayerCreate { .. }
            | Message::LayerAppend { .. }
            | Message::LayerStack { .. }
            | Message::LayerTake { .. }
            | Message::PublishLayer { .. }
            | Message::OpenRequest { .. }
            | Message::Revise { .. }
            | Message::Approve { .. }
            | Message::Comment { .. }
            | Message::PublishRequest { .. }
            | Message::CheckReport(_)
            | Message::SetDelay { .. }
            | Message::Embargo { .. }
            | Message::Release { .. }
    )
}

/// The largest object an upload carries.
fn largest_object(message: &Message) -> Option<u64> {
    match message {
        Message::Objects(objects) => objects.iter().map(|(_, _, bytes)| bytes.len() as u64).max(),
        _ => None,
    }
}

fn storage(error: rusqlite::Error) -> ProtocolError {
    ProtocolError::new("storage", error.to_string())
}
