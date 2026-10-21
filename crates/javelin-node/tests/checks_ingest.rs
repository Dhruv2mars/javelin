//! Check-result ingestion (`docs/ci-execution.md` section 11, tests 1–6).

#[path = "checks_support/mod.rs"]
mod support;

use javelin::ObjectId;
use javelin_identity::Scope;
use javelin_node::checks;
use javelin_protocol::codec::{CheckOutcome, Conclusion, code};
use support::*;
use tempfile::TempDir;

/// A node with `work` holding one file, a second checkpoint on `other`, and the permissions
/// that let `ci-bot` report.
fn seeded(
    dir: &TempDir,
) -> (
    javelin_node::FileWorldLog,
    javelin_node::auth::Gate,
    ObjectId,
    ObjectId,
) {
    let mut log = new_node(dir);
    let first = write(&mut log, "write-1", "work", "a.txt");
    let other = write(&mut log, "write-2", "other", "b.txt");
    let gate = gate(permissions(1, true, true));
    (log, gate, first, other)
}

fn accepted(outcome: CheckOutcome) -> (ObjectId, u64) {
    match outcome {
        CheckOutcome::Accepted { id, seq } => (id, seq),
        other => panic!("expected accepted, got {other:?}"),
    }
}

fn refused_with(outcome: CheckOutcome) -> String {
    match outcome {
        CheckOutcome::Refused { code } => code,
        other => panic!("expected refused, got {other:?}"),
    }
}

#[test]
fn accepts_completed_result_for_tree() {
    let dir = TempDir::new().unwrap();
    let (mut log, gate, first, _) = seeded(&dir);
    let tree = tree_of(&mut log, first);
    let token = reporter("ci-bot", &["ci/*"]);

    let (_, seq) = accepted(report(
        &mut log,
        &gate,
        &token,
        &report_for("report-1", tree, "ci/unit", Conclusion::Success),
    ));
    assert!(seq >= 1);
}

#[test]
fn replay_same_operation_returns_original() {
    let dir = TempDir::new().unwrap();
    let (mut log, gate, first, _) = seeded(&dir);
    let tree = tree_of(&mut log, first);
    let token = reporter("ci-bot", &["ci/*"]);
    let message = report_for("report-1", tree, "ci/unit", Conclusion::Success);

    let original = accepted(report(&mut log, &gate, &token, &message));
    // The reply was lost; the reporter retries with the same operation.
    let replayed = accepted(report(&mut log, &gate, &token, &message));
    assert_eq!(original, replayed);

    let rows: i64 = rusqlite::Connection::open(dir.path().join("node.db"))
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM check_results WHERE operation = 'report-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn rejects_tree_mismatch() {
    let dir = TempDir::new().unwrap();
    let (mut log, gate, first, other) = seeded(&dir);
    let other_tree = tree_of(&mut log, other);
    let token = reporter("ci-bot", &["ci/*"]);
    let mut message = report_for("report-1", other_tree, "ci/unit", Conclusion::Success);
    // The checkpoint composes to `first`'s tree, not the tree the report names.
    message.subject_checkpoint = Some(first);

    assert_eq!(
        refused_with(report(&mut log, &gate, &token, &message)),
        code::TREE_MISMATCH
    );
}

#[test]
fn rejects_unknown_tree() {
    let dir = TempDir::new().unwrap();
    let (mut log, gate, _, _) = seeded(&dir);
    let token = reporter("ci-bot", &["ci/*"]);
    let absent = ObjectId::for_blob(b"never stored");
    let message = report_for("report-1", absent, "ci/unit", Conclusion::Success);

    assert_eq!(
        refused_with(report(&mut log, &gate, &token, &message)),
        code::MISSING_OBJECT
    );
}

#[test]
fn token_pattern_limits_names() {
    let dir = TempDir::new().unwrap();
    let (mut log, gate, first, _) = seeded(&dir);
    let tree = tree_of(&mut log, first);
    let token = reporter("ci-bot", &["ci/*"]);

    let named =
        |operation: &str, name: &str| report_for(operation, tree, name, Conclusion::Success);
    assert_eq!(
        refused_with(report(&mut log, &gate, &token, &named("n-1", "approval/x"))),
        code::FORBIDDEN
    );
    assert_eq!(
        refused_with(report(&mut log, &gate, &token, &named("n-2", "publish/x"))),
        code::FORBIDDEN
    );
    assert_eq!(
        refused_with(report(&mut log, &gate, &token, &named("n-3", "deploy/x"))),
        code::CHECK_NAME_NOT_ALLOWED
    );
    assert_eq!(
        refused_with(report(&mut log, &gate, &token, &named("n-4", "Ci/unit"))),
        code::CHECK_NAME_INVALID
    );
    // A wildcard pattern still never reaches a reserved name.
    let wildcard = reporter("ci-bot", &["*"]);
    assert_eq!(
        refused_with(report(
            &mut log,
            &gate,
            &wildcard,
            &named("n-5", "publish/x")
        )),
        code::FORBIDDEN
    );
    // A token without the scope is refused before any name is read.
    let unscoped = support::token("ci-bot", &[Scope::Read], &["ci/*"]);
    assert_eq!(
        checks::report(
            &mut log,
            &gate,
            Some(&unscoped),
            NOW,
            &named("n-6", "ci/unit"),
        )
        .unwrap_err()
        .code,
        code::FORBIDDEN
    );
    accepted(report(&mut log, &gate, &token, &named("n-7", "ci/unit")));
}

#[test]
fn stored_result_is_immutable() {
    let dir = TempDir::new().unwrap();
    let (mut log, gate, first, _) = seeded(&dir);
    let tree = tree_of(&mut log, first);
    let token = reporter("ci-bot", &["ci/*"]);

    let (first_id, _) = accepted(report(
        &mut log,
        &gate,
        &token,
        &report_for("report-1", tree, "ci/unit", Conclusion::Failure),
    ));
    let stored = |connection: &rusqlite::Connection| -> Vec<u8> {
        connection
            .query_row(
                "SELECT request FROM check_results WHERE operation = 'report-1'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    let connection = rusqlite::Connection::open(dir.path().join("node.db")).unwrap();
    let before = stored(&connection);

    // A re-run is a new result with a new ID; the first stays as it was.
    let (rerun_id, _) = accepted(report(
        &mut log,
        &gate,
        &token,
        &report_for("report-2", tree, "ci/unit", Conclusion::Success),
    ));
    assert_ne!(first_id, rerun_id);
    assert_eq!(stored(&connection), before);

    // The schema refuses edits and deletes outright, not only the node's own code.
    assert!(
        connection
            .execute("UPDATE check_results SET conclusion = 0", [])
            .is_err()
    );
    assert!(connection.execute("DELETE FROM check_results", []).is_err());
}
