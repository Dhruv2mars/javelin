//! The node's budget (`docs/direction.md`, What a node needs, 8): a limit per provider quota a
//! write can cross, set by the node's owner, with defaults from the Cloudflare Free plan
//! (`tools/node-cloudflare/README.md`). A write that would cross one is refused with
//! `quota_exceeded` before it is served, so it changes nothing and can be retried once the
//! owner raises the budget or the day's count resets. The self-hosted node and the Durable
//! Object run this same code over their own storage.
//!
//! Use is measured on the node's two SQLite databases, whatever stores them: storage is their
//! pages, and rows written are the pages each request writes, counted as 64 KiB rows, the
//! page row a Durable Object keeps (`crates/javelin-node-cloudflare/src/pages.rs`), plus the
//! size row it writes for each page a database grows by. Before a write, its cost is bounded
//! from what it carries (`cost`): its body twice over in rows (pages and size rows), a few
//! rows and bytes per object, since each upload commits on its own, and a reserve for the
//! records it touches. A Publish is bounded once it is prepared, from the chunks it is about
//! to insert (`Room::publish`), after its replay is answered. A write is refused while its
//! bound does not fit. Admin pushes, such as provisioning, are not counted.

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
/// Rows and pages a write may touch besides its body and objects: B-tree pages, the node's
/// records and the count.
const RESERVE_ROWS: u64 = 16;
/// The same for a Publish, which also writes its version, attempt, blame and stamps: one
/// file's Publish measured 30 rows on a Durable Object.
const PUBLISH_RESERVE_ROWS: u64 = 64;
/// Rows one uploaded object may write: its row and index pages, its upload record, in a
/// transaction of its own (measured 3 per object on a Durable Object).
const OBJECT_ROWS: u64 = 4;
/// Bytes one object or changed path may add besides its own bytes (measured about 400).
const OBJECT_BYTES: u64 = 512;
/// Bytes one `object_access` row may take: an object ID, an audience and a version, in a
/// table keyed by both (measured about 160 with its B-tree).
const GRANT_BYTES: u64 = 192;
/// Pages one opening's transaction may write besides its `object_access` leaves: its seal, the
/// pages above the leaves and the header.
const OPENING_PAGES: u64 = 8;
/// Rows one forked version may write besides its paths: its version row, attempt and
/// indexes, in a transaction of its own.
const VERSION_ROWS: u64 = 16;
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

/// What a fork still writes into its target (`fork::Plan`): its objects (blobs, trees,
/// openings and traces) and their bytes, the largest one, the `object_access` rows they add,
/// the versions past the target's head, and the paths those versions change with their bytes.
/// `openings` holds the rows each opening still to write adds, and `access_rows` the rows the
/// whole fork leaves in `object_access`.
#[derive(Clone, Debug, Default)]
pub(crate) struct ForkWrite {
    pub objects: u64,
    pub bytes: u64,
    pub largest: u64,
    pub grants: u64,
    pub openings: Vec<u64>,
    pub access_rows: u64,
    pub versions: u64,
    pub paths: u64,
    pub path_bytes: u64,
}

/// What the budget leaves, read before a write starts: the budget, the bytes stored, today's
/// rows and the repository's page size.
pub(crate) struct Room {
    budget: Budget,
    bytes: u64,
    rows: u64,
    page_size: u64,
}

impl Room {
    pub(crate) fn now(log: &FileWorldLog, now: u64) -> Result<Self, ProtocolError> {
        let (bytes, page_size) = stored_bytes(log)?;
        Ok(Self {
            budget: log.budget(),
            bytes,
            rows: rows_today(log.records(), now)?,
            page_size,
        })
    }

    /// Refuses a prepared Publish whose bound would cross the budget. The detail carries the
    /// numbers; `wire::handle` keeps them for maintainers only.
    pub(crate) fn publish(&self, write: &javelin::PublishWrite) -> Result<(), ProtocolError> {
        // Each path is stored in the changeset and in blame, each with an index.
        let bytes =
            write.bytes + 4 * write.path_bytes + (write.objects + write.paths) * OBJECT_BYTES;
        self.fits(
            bytes + PUBLISH_RESERVE_ROWS * self.page_size,
            2 * bytes.div_ceil(ROW) + write.objects + PUBLISH_RESERVE_ROWS,
        )
    }

    /// Refuses a fork whose bound would cross the budget, before anything is written: an
    /// object larger than the owner allows, or more bytes or rows than are left. Each version
    /// is written by a transaction of its own, so each carries a Publish's reserve of rows.
    /// Each opening is one too, and rewrites every `object_access` page it inserts into: at
    /// most one per row it adds, never more than the table's leaves, plus the pages above.
    pub(crate) fn fork(&self, write: &ForkWrite) -> Result<(), ProtocolError> {
        if write.largest > self.budget.largest_object {
            return Err(quota(
                LARGEST_OBJECT,
                format!(
                    "an object of {} bytes is larger than the {} allowed",
                    write.largest, self.budget.largest_object
                ),
            ));
        }
        let bytes = write.bytes
            + 4 * write.path_bytes
            + (write.objects + write.paths + write.versions) * OBJECT_BYTES
            + write.grants * GRANT_BYTES;
        let leaves = (write.access_rows * GRANT_BYTES).div_ceil(self.page_size) + 1;
        let opening_pages: u64 = write
            .openings
            .iter()
            .map(|grants| (*grants).min(leaves) + OPENING_PAGES)
            .sum();
        self.fits(
            2 * bytes + PUBLISH_RESERVE_ROWS * self.page_size,
            2 * bytes.div_ceil(ROW)
                + (opening_pages * self.page_size).div_ceil(ROW)
                + write.objects * OBJECT_ROWS
                + write.versions * VERSION_ROWS
                + PUBLISH_RESERVE_ROWS,
        )
    }

    /// Refuses a write of up to `bytes` more stored and `rows` more written today.
    fn fits(&self, bytes: u64, rows: u64) -> Result<(), ProtocolError> {
        let budget = &self.budget;
        if self.bytes.saturating_add(bytes) > budget.storage_bytes {
            return Err(quota(
                STORAGE_BYTES,
                format!(
                    "{} of {} bytes stored; this write needs up to {bytes} more",
                    self.bytes, budget.storage_bytes
                ),
            ));
        }
        if self.rows.saturating_add(rows) > budget.rows_written_per_day {
            return Err(quota(
                ROWS_WRITTEN_PER_DAY,
                format!(
                    "{} of {} rows written today (UTC); this write needs up to {rows} more",
                    self.rows, budget.rows_written_per_day
                ),
            ));
        }
        Ok(())
    }
}

/// The bound on what a write other than a Publish adds, as `(bytes, rows)`: `body` is the
/// request's length.
pub fn cost(message: &Message, body: usize, page_size: u64) -> (u64, u64) {
    let body = body as u64;
    let objects = match message {
        Message::Objects(objects) => objects.len() as u64,
        _ => 0,
    };
    (
        body + objects * OBJECT_BYTES + RESERVE_ROWS * page_size,
        2 * body.div_ceil(ROW) + objects * OBJECT_ROWS + RESERVE_ROWS,
    )
}

/// Refuses a write whose bound would cross the budget, before anything is stored. `body` is
/// the request's length. A decided Publish replays without a check; a new one is checked here
/// and again once prepared (`Room::publish`). A caller the gate refuses gets that refusal instead, so a
/// refusal never tells it more than the write would have. The detail carries the numbers;
/// `wire::handle` keeps them for maintainers only.
pub fn admit(
    log: &FileWorldLog,
    gate: &Gate,
    token: Option<&str>,
    now: u64,
    message: &Message,
    body: usize,
) -> Result<(), ProtocolError> {
    if !writes(message) || replays(log, message)? {
        return Ok(());
    }
    let room = Room::now(log, now)?;
    let crossing = match largest_object(message) {
        Some(size) if size > room.budget.largest_object => Err(quota(
            LARGEST_OBJECT,
            format!(
                "an object of {size} bytes is larger than the {} allowed",
                room.budget.largest_object
            ),
        )),
        _ => {
            let (bytes, rows) = cost(message, body, room.page_size);
            room.fits(bytes, rows)
        }
    };
    if crossing.is_err() {
        gate.check(token, now, crate::wire::need(message))?;
    }
    crossing
}

/// `Room::publish` as a Publish bound: a crossing is kept in `crossed` for the caller to
/// answer with, and aborts the Publish.
pub(crate) fn hold(
    room: &Room,
    write: &javelin::PublishWrite,
    crossed: &mut Option<ProtocolError>,
) -> javelin::Result<()> {
    room.publish(write).map_err(|refusal| {
        let detail = refusal.detail.clone();
        *crossed = Some(refusal);
        javelin::Error::InvalidInput(detail)
    })
}

/// Whether `message` is a Publish already decided, which replays its recorded outcome
/// however little budget is left. A new one is admitted here for the records any outcome
/// writes, and bounded again once prepared (`Room::publish`).
pub(crate) fn replays(log: &FileWorldLog, message: &Message) -> Result<bool, ProtocolError> {
    Ok(match message {
        Message::PublishLayer { operation, .. } => {
            log.decided(operation)?.is_some() || log.repo().publication(operation)?.is_some()
        }
        Message::PublishRequest { operation, .. } => {
            crate::review::is_recorded(log.records(), operation)?
                || log.repo().publication(operation)?.is_some()
        }
        _ => false,
    })
}

/// The detail a caller who may not read the whole repository gets: the budget's name.
pub(crate) fn redact(detail: &str) -> String {
    detail.split(':').next().unwrap_or(detail).to_string()
}

fn quota(budget: &str, numbers: String) -> ProtocolError {
    ProtocolError::new(code::QUOTA_EXCEEDED, format!("{budget}: {numbers}"))
}

/// Adds the rows this request wrote to today's count: its pages as 64 KiB rows, and one size
/// row for each 64 KiB a database grew, since a Durable Object writes a file's size with each
/// page that extends it. The count's own write is one more row, counted here, so a request
/// that wrote nothing leaves the count as it was.
pub fn meter(log: &mut FileWorldLog, now: u64) -> Result<(), ProtocolError> {
    let repository = log.repo().database_use()?;
    let records = javelin::database_use(log.records())?;
    let written = log.repo().take_pages_written()? * repository.page_size
        + javelin::take_pages_written(log.records())? * records.page_size;
    let [repository_before, records_before] = log.metered_sizes();
    let grown = repository.bytes.saturating_sub(repository_before)
        + records.bytes.saturating_sub(records_before);
    let rows = written.div_ceil(ROW) + grown.div_ceil(ROW);
    if rows > 0 {
        let today = day(now);
        let records = log.records();
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
    }
    log.set_metered_sizes()?;
    Ok(())
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
            | Message::PublishRequest { .. }
            | Message::OpenRequest { .. }
            | Message::Revise { .. }
            | Message::Approve { .. }
            | Message::Comment { .. }
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
