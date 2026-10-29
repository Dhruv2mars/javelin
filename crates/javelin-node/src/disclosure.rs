//! The public frontier (`docs/direction.md`, Superposition): the disclosure delay and
//! embargoes that hold readers outside the repository below its head.
//!
//! A reader outside the repository sees the World at its public frontier: the newest version
//! older than the delay with no unreleased embargo at or below it. Members see the head.
//! Version IDs never change; versions above the frontier, and everything only they reach,
//! are absent to an outsider (`javelin::Reader::with_frontier`).
//!
//! The node keeps what decides the frontier in `node.db`:
//!
//! - the delay, a repository setting only Maintain changes;
//! - each version's time: the node's clock when it first saw the version, at the request
//!   that published it or, after a restart between the two writes, at the next request. A
//!   late stamp only delays disclosure;
//! - embargoes, keyed by the operation ID of the Publish they hold. An embargo is stored
//!   before its Publish, so the version it holds is never disclosed early, and is tied to
//!   that version once the Publish is decided;
//! - the newest frontier served so far. Disclosure is one-way: raising the delay never takes
//!   back a version that was already public.

use javelin::{OperationId, PublishOutcome, Reader, Repository, VersionNumber};
use javelin_protocol::{
    ProtocolError,
    codec::{Disclosure, Embargoed, code},
};
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS disclosure_delay (
    one INTEGER PRIMARY KEY CHECK(one=0),
    delay INTEGER NOT NULL CHECK(delay>=0)
);
CREATE TABLE IF NOT EXISTS version_times (
    version INTEGER PRIMARY KEY CHECK(version>=0),
    at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS embargoes (
    operation TEXT PRIMARY KEY,
    until INTEGER,
    released INTEGER,
    version INTEGER UNIQUE
);
CREATE TABLE IF NOT EXISTS disclosed (
    one INTEGER PRIMARY KEY CHECK(one=0),
    version INTEGER NOT NULL CHECK(version>=0)
);";

/// Stamps every version the node has not seen with `now`, never earlier than the last
/// stamp, and ties each embargo whose Publish is decided to the version it made. Runs
/// before every request, so no read sees a version before its time and embargo are known.
pub fn stamp(records: &Connection, repo: &Repository, now: u64) -> Result<(), ProtocolError> {
    let head = repo.head()?.number.get();
    let last: Option<(i64, i64)> = records
        .query_row(
            "SELECT version,at FROM version_times ORDER BY version DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(storage)?;
    let (next, at) = match last {
        Some((version, at)) => (version as u64 + 1, (at as u64).max(now)),
        None => (0, now),
    };
    for version in next..=head {
        records
            .execute(
                "INSERT INTO version_times(version,at) VALUES(?1,?2)",
                params![version as i64, at as i64],
            )
            .map_err(storage)?;
    }
    let pending = records
        .prepare("SELECT operation FROM embargoes WHERE version IS NULL")
        .and_then(|mut statement| {
            statement
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(storage)?;
    for operation in pending {
        let operation = OperationId::parse(&operation)?;
        let Some(receipt) = repo.publication(&operation)? else {
            continue;
        };
        // A Publish refused or in conflict made no version, and its operation cannot
        // publish again, so its embargo holds nothing.
        match receipt.outcome {
            PublishOutcome::Published { version } => records.execute(
                "UPDATE embargoes SET version=?2 WHERE operation=?1",
                params![operation.as_str(), version.number.get() as i64],
            ),
            _ => records.execute(
                "DELETE FROM embargoes WHERE operation=?1",
                [operation.as_str()],
            ),
        }
        .map_err(storage)?;
    }
    Ok(())
}

/// The public frontier at Unix time `now`: the newest version older than the delay with no
/// unreleased embargo at or below it, and never below a frontier already served. Stamps
/// first, so every decided embargo is tied to its version before the frontier is decided.
pub fn frontier(
    records: &Connection,
    repo: &Repository,
    now: u64,
) -> Result<VersionNumber, ProtocolError> {
    stamp(records, repo, now)?;
    let head = repo.head()?.number;
    let delay = delay(records)?;
    let delayed = if delay == 0 {
        head.get()
    } else {
        let cutoff = now.saturating_sub(delay);
        records
            .query_row(
                "SELECT MAX(version) FROM version_times WHERE at<=?1",
                [cutoff.min(i64::MAX as u64) as i64],
                |r| r.get::<_, Option<i64>>(0),
            )
            .map_err(storage)?
            .map_or(0, |version| version as u64)
    };
    let held = records
        .query_row(
            "SELECT MIN(version) FROM embargoes WHERE version IS NOT NULL AND released IS NULL \
             AND (until IS NULL OR until>?1)",
            [now.min(i64::MAX as u64) as i64],
            |r| r.get::<_, Option<i64>>(0),
        )
        .map_err(storage)?
        .map_or(u64::MAX, |version| (version as u64).saturating_sub(1));
    let disclosed: u64 = records
        .query_row("SELECT version FROM disclosed WHERE one=0", [], |r| {
            r.get::<_, i64>(0)
        })
        .optional()
        .map_err(storage)?
        .map_or(0, |version| version as u64);
    let frontier = delayed.min(held).min(head.get()).max(disclosed);
    if frontier > disclosed {
        records
            .execute(
                "INSERT INTO disclosed(one,version) VALUES(0,?1) \
                 ON CONFLICT(one) DO UPDATE SET version=excluded.version",
                [frontier as i64],
            )
            .map_err(storage)?;
    }
    Ok(VersionNumber::new(frontier)?)
}

/// `reader` as it reads now: a reader outside the repository is held at the frontier.
pub fn hold(
    records: &Connection,
    repo: &Repository,
    now: u64,
    reader: Reader,
) -> Result<Reader, ProtocolError> {
    if reader.is_member() {
        return Ok(reader);
    }
    Ok(reader.with_frontier(frontier(records, repo, now)?))
}

/// The delay, in seconds. Zero until a maintainer sets one.
pub fn delay(records: &Connection) -> Result<u64, ProtocolError> {
    Ok(records
        .query_row("SELECT delay FROM disclosure_delay WHERE one=0", [], |r| {
            r.get::<_, i64>(0)
        })
        .optional()
        .map_err(storage)?
        .map_or(0, |delay| delay as u64))
}

/// Sets the delay. The caller checks the Maintain role.
pub fn set_delay(records: &Connection, delay: u64) -> Result<(), ProtocolError> {
    let delay = i64::try_from(delay)
        .map_err(|_| ProtocolError::new(code::INVALID_INPUT, "delay is out of range"))?;
    records
        .execute(
            "INSERT INTO disclosure_delay(one,delay) VALUES(0,?1) \
             ON CONFLICT(one) DO UPDATE SET delay=excluded.delay",
            [delay],
        )
        .map_err(storage)?;
    Ok(())
}

/// Embargoes the Publish `operation`, released at `until` or by a maintainer. Repeating the
/// same embargo is safe at any time. Changing it is refused once the Publish is decided,
/// and a decided Publish cannot gain one: its version may already be public. The caller
/// checks the Maintain role.
pub fn embargo(
    records: &Connection,
    repo: &Repository,
    operation: &OperationId,
    until: Option<u64>,
) -> Result<(), ProtocolError> {
    let until = until
        .map(i64::try_from)
        .transpose()
        .map_err(|_| ProtocolError::new(code::INVALID_INPUT, "release time is out of range"))?;
    let stored: Option<Option<i64>> = records
        .query_row(
            "SELECT until FROM embargoes WHERE operation=?1",
            [operation.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?;
    if stored == Some(until) {
        return Ok(());
    }
    if repo.publication(operation)?.is_some() {
        return Err(ProtocolError::new(
            code::INVALID_INPUT,
            format!(
                "Publish {} is already decided; an embargo must be set before it",
                operation.as_str()
            ),
        ));
    }
    records
        .execute(
            "INSERT INTO embargoes(operation,until) VALUES(?1,?2) \
             ON CONFLICT(operation) DO UPDATE SET until=excluded.until",
            params![operation.as_str(), until],
        )
        .map_err(storage)?;
    Ok(())
}

/// Stores the embargo a Publish carries (`PublishRequest`'s `embargo`), before that Publish
/// runs. A Publish that made a version keeps the embargo it was decided with: another one is
/// `operation_id_reused`, the refusal code returned. One that made no version holds nothing,
/// so its retry replays whatever it carries. The caller checks the Maintain role.
pub(crate) fn carried(
    records: &Connection,
    repo: &Repository,
    operation: &OperationId,
    embargo: Option<Option<u64>>,
) -> Result<Option<&'static str>, ProtocolError> {
    let Some(until) = embargo else {
        return Ok(None);
    };
    let Err(error) = self::embargo(records, repo, operation, until) else {
        return Ok(None);
    };
    match repo.publication(operation)?.map(|receipt| receipt.outcome) {
        Some(PublishOutcome::Published { .. }) => Ok(Some(code::OPERATION_ID_REUSED)),
        Some(_) => Ok(None),
        None => Err(error),
    }
}

/// Releases the embargo on `version` at `now`. Releasing a released embargo changes
/// nothing. The caller checks the Maintain role.
pub fn release(
    records: &Connection,
    version: VersionNumber,
    now: u64,
) -> Result<(), ProtocolError> {
    let changed = records
        .execute(
            "UPDATE embargoes SET released=COALESCE(released,?2) WHERE version=?1",
            params![version.get() as i64, now.min(i64::MAX as u64) as i64],
        )
        .map_err(storage)?;
    if changed == 0 {
        return Err(ProtocolError::new(
            code::INVALID_INPUT,
            format!("World {version} has no embargo"),
        ));
    }
    Ok(())
}

/// The disclosure as `reader` may know it. A reader outside the repository learns whether
/// the public view is delayed and where its frontier is, and nothing about any one version.
pub fn state(
    records: &Connection,
    repo: &Repository,
    now: u64,
    reader: &Reader,
) -> Result<Disclosure, ProtocolError> {
    let delay = delay(records)?;
    let frontier = frontier(records, repo, now)?;
    if !reader.is_member() {
        return Ok(Disclosure {
            delayed: delay > 0,
            delay: None,
            frontier,
            embargoes: Vec::new(),
        });
    }
    let embargoes = records
        .prepare(
            "SELECT version,until FROM embargoes WHERE version IS NOT NULL AND released IS NULL \
             AND (until IS NULL OR until>?1) ORDER BY version",
        )
        .and_then(|mut statement| {
            statement
                .query_map([now.min(i64::MAX as u64) as i64], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(storage)?
        .into_iter()
        .map(|(version, until)| {
            Ok(Embargoed {
                version: VersionNumber::new(version as u64)?,
                until: until.map(|until| until as u64),
            })
        })
        .collect::<Result<Vec<_>, ProtocolError>>()?;
    Ok(Disclosure {
        delayed: delay > 0,
        delay: Some(delay),
        frontier,
        embargoes,
    })
}

fn storage(error: rusqlite::Error) -> ProtocolError {
    ProtocolError::new("storage", error.to_string())
}
