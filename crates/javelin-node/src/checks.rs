//! Check results and the required-check gate (`docs/ci-execution.md` sections 4, 7 and 11;
//! unit U17). An external reporter posts a completed result for a tree, and the node stores
//! it under the reporter's identity. A required check at Publish is met when the latest
//! stored result for the source tree, from an allowed reporter, is `success`.
//!
//! Results are append-only rows in `node.db`, enforced by triggers, so a stored result is
//! never rewritten. A report's operation replays its original `id` and `seq`. Policy is
//! node-owned, and an empty policy leaves Publish unchanged.

use javelin::{CheckpointId, ObjectId, Repository};
use javelin_collaboration::{AppliedPermissions, PrincipalId};
use javelin_identity::{Grant, Need, Scope};
use javelin_protocol::{
    Message, ProtocolError,
    codec::{CheckOutcome, CheckReport, Conclusion, code, encode},
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{auth::Gate, world_log::FileWorldLog};

/// Tables for check results and policy. Created with the node's other tables.
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS check_results (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    operation TEXT NOT NULL UNIQUE,
    id TEXT NOT NULL,
    subject TEXT NOT NULL,
    name TEXT NOT NULL,
    reporter TEXT NOT NULL,
    conclusion INTEGER NOT NULL,
    request BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS check_results_latest ON check_results (subject, name, reporter, seq);
CREATE TRIGGER IF NOT EXISTS check_results_frozen BEFORE UPDATE ON check_results
BEGIN SELECT RAISE(ABORT, 'check results are immutable'); END;
CREATE TRIGGER IF NOT EXISTS check_results_kept BEFORE DELETE ON check_results
BEGIN SELECT RAISE(ABORT, 'check results are immutable'); END;
CREATE TABLE IF NOT EXISTS check_policy (
    name TEXT NOT NULL,
    reporter TEXT
);";

/// Check names whose first segment only the node may write. No token can report them.
const RESERVED: [&str; 2] = ["approval", "publish"];
/// Longest check name, in bytes. It must also fit the wire's one-byte length.
const MAX_NAME: usize = 255;

/// A check that must pass at Publish. `reporter` names the identity whose result counts;
/// `None` accepts a result from any reporter whose token allows the name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequiredCheck {
    pub name: String,
    pub reporter: Option<String>,
}

/// Replaces the node's required-check policy. Publish reads it at its correctness boundary.
pub fn set_required(
    log: &mut FileWorldLog,
    required: &[RequiredCheck],
) -> Result<(), ProtocolError> {
    for check in required {
        if !valid_name(&check.name) {
            return Err(ProtocolError::new(
                code::CHECK_NAME_INVALID,
                format!("invalid check name {:?}", check.name),
            ));
        }
        if is_reserved(&check.name) {
            return Err(ProtocolError::new(
                code::FORBIDDEN,
                format!("{:?} is reserved for the node", check.name),
            ));
        }
    }
    let (_, records) = log.parts();
    let tx = records.transaction().map_err(storage)?;
    tx.execute("DELETE FROM check_policy", [])
        .map_err(storage)?;
    for check in required {
        tx.execute(
            "INSERT INTO check_policy (name, reporter) VALUES (?1, ?2)",
            params![check.name, check.reporter],
        )
        .map_err(storage)?;
    }
    tx.commit().map_err(storage)
}

/// Stores one completed check result for a tree. The caller's token must carry
/// `checks:report` with a pattern that matches the name. Name, repository and subject
/// refusals come back as the outcome's code, and nothing is stored for them.
pub fn report(
    log: &mut FileWorldLog,
    gate: &Gate,
    token: Option<&str>,
    now: u64,
    report: &CheckReport,
) -> Result<CheckOutcome, ProtocolError> {
    let grant = gate.check(
        token,
        now,
        Need {
            scope: Scope::CheckReport,
            layer: None,
        },
    )?;
    let permissions = gate
        .permissions()
        .ok_or_else(|| ProtocolError::new(code::FORBIDDEN, "no permissions are applied yet"))?;
    let reporter = reporter(&grant)?;
    let request = encode(&Message::CheckReport(report.clone()))?;
    let (repo, records) = log.parts();
    if let Some(stored) = recorded(records, &report.operation)? {
        return Ok(
            if stored.request == request && stored.reporter == reporter {
                CheckOutcome::Accepted {
                    id: stored.id,
                    seq: stored.seq,
                }
            } else {
                refused(code::OPERATION_ID_REUSED)
            },
        );
    }
    if report.repository != permissions.repository.as_str() {
        return Ok(refused(code::FORBIDDEN));
    }
    if let Err(refusal) = check_name(&grant, &report.name) {
        return Ok(refused(refusal));
    }
    if let Some(checkpoint) = report.subject_checkpoint {
        let id = CheckpointId::parse(&checkpoint.to_string())?;
        if repo.read_checkpoint(id).is_err() {
            return Ok(refused(code::MISSING_OBJECT));
        }
        if checkpoint_tree(repo, id)? != report.subject_tree {
            return Ok(refused(code::TREE_MISMATCH));
        }
    }
    if !repo.has_tree(report.subject_tree)? {
        return Ok(refused(code::MISSING_OBJECT));
    }
    let id = ObjectId::for_blob(&[request.as_slice(), reporter.as_bytes()].concat());
    records
        .execute(
            "INSERT INTO check_results (operation, id, subject, name, reporter, conclusion, request)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                report.operation.as_str(),
                id.to_string(),
                report.subject_tree.to_string(),
                report.name,
                reporter,
                report.conclusion.code(),
                request,
            ],
        )
        .map_err(storage)?;
    Ok(CheckOutcome::Accepted {
        id,
        seq: records.last_insert_rowid() as u64,
    })
}

/// The refusal that a Publish of `checkpoint` meets under the policy, or `None` when every
/// required check is met. `permissions` is `None` where no applied permissions are known.
pub(crate) fn gate(
    conn: &Connection,
    repo: &mut Repository,
    checkpoint: CheckpointId,
    permissions: Option<&AppliedPermissions>,
) -> Result<Option<&'static str>, ProtocolError> {
    let required = required(conn)?;
    if required.is_empty() {
        return Ok(None);
    }
    let tree = checkpoint_tree(repo, checkpoint)?;
    unmet(conn, &required, tree, permissions)
}

/// As [`gate`], for a tree that the caller already holds, such as a revision's source tree.
pub(crate) fn gate_tree(
    conn: &Connection,
    tree: ObjectId,
    permissions: Option<&AppliedPermissions>,
) -> Result<Option<&'static str>, ProtocolError> {
    let required = required(conn)?;
    unmet(conn, &required, tree, permissions)
}

/// The first unmet requirement, as its refusal code. A requirement is met by the latest
/// result for `tree` and name, from its reporter, with conclusion `success`. Missing
/// results refuse `checks_missing`; a result that is not `success` refuses `checks_failed`.
/// A result whose reporter no longer holds a role refuses `stale_permissions`.
fn unmet(
    conn: &Connection,
    required: &[RequiredCheck],
    tree: ObjectId,
    permissions: Option<&AppliedPermissions>,
) -> Result<Option<&'static str>, ProtocolError> {
    for check in required {
        let Some(latest) = latest(conn, tree, check)? else {
            return Ok(Some(code::CHECKS_MISSING));
        };
        if let Some(permissions) = permissions {
            let holds_role = PrincipalId::parse(&latest.reporter)
                .ok()
                .and_then(|reporter| permissions.role(&reporter))
                .is_some();
            if !holds_role {
                return Ok(Some(code::STALE_PERMISSIONS));
            }
        }
        if latest.conclusion != Conclusion::Success {
            return Ok(Some(code::CHECKS_FAILED));
        }
    }
    Ok(None)
}

struct Latest {
    reporter: String,
    conclusion: Conclusion,
}

fn latest(
    conn: &Connection,
    tree: ObjectId,
    check: &RequiredCheck,
) -> Result<Option<Latest>, ProtocolError> {
    conn.query_row(
        "SELECT reporter, conclusion FROM check_results
         WHERE subject = ?1 AND name = ?2 AND (?3 IS NULL OR reporter = ?3)
         ORDER BY seq DESC LIMIT 1",
        params![tree.to_string(), check.name, check.reporter],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, u8>(1)?)),
    )
    .optional()
    .map_err(storage)?
    .map(|(reporter, conclusion)| {
        Ok(Latest {
            reporter,
            conclusion: Conclusion::from_code(conclusion)
                .ok_or_else(|| storage_detail("unknown conclusion code"))?,
        })
    })
    .transpose()
}

fn required(conn: &Connection) -> Result<Vec<RequiredCheck>, ProtocolError> {
    let mut statement = conn
        .prepare("SELECT name, reporter FROM check_policy ORDER BY rowid")
        .map_err(storage)?;
    let rows = statement
        .query_map([], |row| {
            Ok(RequiredCheck {
                name: row.get(0)?,
                reporter: row.get(1)?,
            })
        })
        .map_err(storage)?;
    rows.collect::<Result<_, _>>().map_err(storage)
}

/// The tree a checkpoint holds, stored as a snapshot if it is not yet.
fn checkpoint_tree(
    repo: &mut Repository,
    checkpoint: CheckpointId,
) -> Result<ObjectId, ProtocolError> {
    let manifest = repo.checkpoint_manifest(checkpoint)?;
    Ok(repo.put_snapshot(&manifest)?)
}

/// Whether `grant` may report `name`. Reserved names are refused before any pattern applies.
fn check_name(grant: &Grant, name: &str) -> Result<(), &'static str> {
    if !valid_name(name) {
        return Err(code::CHECK_NAME_INVALID);
    }
    if is_reserved(name) {
        return Err(code::FORBIDDEN);
    }
    if grant.checks.iter().any(|pattern| glob(pattern, name)) {
        Ok(())
    } else {
        Err(code::CHECK_NAME_NOT_ALLOWED)
    }
}

/// Whether the node alone may write `name`: its first segment is a reserved prefix.
fn is_reserved(name: &str) -> bool {
    RESERVED.contains(&name.split('/').next().unwrap_or_default())
}

/// `^[a-z0-9_.-]+(/[a-z0-9_.()-]+)*$`, at most `MAX_NAME` bytes.
fn valid_name(name: &str) -> bool {
    name.len() <= MAX_NAME
        && name.split('/').enumerate().all(|(index, segment)| {
            !segment.is_empty()
                && segment.bytes().all(|b| {
                    b.is_ascii_lowercase()
                        || b.is_ascii_digit()
                        || b"_.-".contains(&b)
                        || (index > 0 && b"()".contains(&b))
                })
        })
}

/// Matches `name` against `pattern`, where `*` stands for any run of bytes.
fn glob(pattern: &str, name: &str) -> bool {
    let (pattern, name) = (pattern.as_bytes(), name.as_bytes());
    let (mut p, mut n) = (0, 0);
    let (mut star, mut resume) = (None, 0);
    while n < name.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            resume = n;
            p += 1;
        } else if p < pattern.len() && pattern[p] == name[n] {
            p += 1;
            n += 1;
        } else if let Some(at) = star {
            p = at + 1;
            resume += 1;
            n = resume;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&b| b == b'*')
}

fn reporter(grant: &Grant) -> Result<String, ProtocolError> {
    grant
        .principal
        .clone()
        .ok_or_else(|| ProtocolError::new(code::UNAUTHORIZED, "a token is required"))
}

struct Recorded {
    request: Vec<u8>,
    reporter: String,
    id: ObjectId,
    seq: u64,
}

fn recorded(
    conn: &Connection,
    operation: &javelin::OperationId,
) -> Result<Option<Recorded>, ProtocolError> {
    let row = conn
        .query_row(
            "SELECT request, reporter, id, seq FROM check_results WHERE operation = ?1",
            [operation.as_str()],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()
        .map_err(storage)?;
    row.map(|(request, reporter, id, seq)| {
        Ok(Recorded {
            request,
            reporter,
            id: ObjectId::parse(&id).map_err(|_| storage_detail("malformed check ID"))?,
            seq: seq as u64,
        })
    })
    .transpose()
}

fn refused(code: &str) -> CheckOutcome {
    CheckOutcome::Refused { code: code.into() }
}

fn storage(error: rusqlite::Error) -> ProtocolError {
    storage_detail(&error.to_string())
}

fn storage_detail(detail: &str) -> ProtocolError {
    ProtocolError::new("storage", detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_grammar() {
        assert!(valid_name("ci/unit"));
        assert!(valid_name("build.x_y-z"));
        assert!(valid_name("ci/unit(1)"));
        assert!(!valid_name(""));
        assert!(!valid_name("CI/unit"));
        assert!(!valid_name("ci//unit"));
        assert!(!valid_name("ci/"));
        assert!(!valid_name("ci/unit "));
        assert!(!valid_name("(ci)"));
        assert!(!valid_name(&"a".repeat(256)));
    }

    #[test]
    fn globs_match_whole_names() {
        assert!(glob("ci/*", "ci/unit"));
        assert!(glob("ci/*", "ci/a/b"));
        assert!(glob("*", "approval/x"));
        assert!(glob("ci/unit", "ci/unit"));
        assert!(!glob("ci/*", "cix/unit"));
        assert!(!glob("ci/unit", "ci/units"));
        assert!(!glob("ci/*", "ci"));
        assert!(glob("*/unit", "ci/unit"));
    }
}
