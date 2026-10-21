//! The required-check gate at Publish (`docs/ci-execution.md` section 11, tests 7–15).

#[path = "checks_support/mod.rs"]
mod support;

use javelin::ObjectId;
use javelin_identity::Scope;
use javelin_node::{FileWorldLog, checks, review::handle};
use javelin_protocol::{
    Message,
    codec::{Conclusion, PublishOutcome, RequestId, ReviewOutcome, code},
};
use support::*;
use tempfile::TempDir;

/// The Layer `work` with one checkpoint, and that checkpoint's tree.
fn seeded(dir: &TempDir) -> (FileWorldLog, ObjectId, ObjectId) {
    let mut log = new_node(dir);
    let first = write(&mut log, "write-1", "work", "a.txt");
    let tree = tree_of(&mut log, first);
    (log, first, tree)
}

#[test]
fn publish_refuses_missing_required_check() {
    let dir = TempDir::new().unwrap();
    let (mut log, first, _) = seeded(&dir);
    require(&mut log, &[("ci/unit", None)]);

    assert_eq!(
        publish(&mut log, "publish-1", "work", first),
        refused(code::CHECKS_MISSING)
    );
    assert_eq!(log.head().unwrap().number, v(0));
}

#[test]
fn publish_refuses_failed_required_check() {
    let dir = TempDir::new().unwrap();
    let (mut log, first, tree) = seeded(&dir);
    require(&mut log, &[("ci/unit", None)]);
    let gate = gate(permissions(1, true, true));
    report(
        &mut log,
        &gate,
        &reporter("ci-bot", &["ci/*"]),
        &report_for("report-1", tree, "ci/unit", Conclusion::Failure),
    );

    assert_eq!(
        publish(&mut log, "publish-1", "work", first),
        refused(code::CHECKS_FAILED)
    );
}

#[test]
fn publish_accepts_success_for_source_tree() {
    let dir = TempDir::new().unwrap();
    let (mut log, first, tree) = seeded(&dir);
    require(&mut log, &[("ci/unit", None)]);
    let gate = gate(permissions(1, true, true));
    report(
        &mut log,
        &gate,
        &reporter("ci-bot", &["ci/*"]),
        &report_for("report-1", tree, "ci/unit", Conclusion::Success),
    );

    assert!(matches!(
        publish(&mut log, "publish-1", "work", first),
        PublishOutcome::Published { .. }
    ));
}

#[test]
fn result_for_other_tree_does_not_count() {
    let dir = TempDir::new().unwrap();
    let (mut log, first, _) = seeded(&dir);
    let side = write(&mut log, "side-1", "side", "b.txt");
    let side_tree = tree_of(&mut log, side);
    require(&mut log, &[("ci/unit", None)]);
    let gate = gate(permissions(1, true, true));
    report(
        &mut log,
        &gate,
        &reporter("ci-bot", &["ci/*"]),
        &report_for("report-1", side_tree, "ci/unit", Conclusion::Success),
    );

    assert_eq!(
        publish(&mut log, "publish-1", "work", first),
        refused(code::CHECKS_MISSING)
    );
}

#[test]
fn nonintersecting_advance_keeps_result_valid() {
    let dir = TempDir::new().unwrap();
    let (mut log, first, tree) = seeded(&dir);
    require(&mut log, &[("ci/unit", None)]);
    let gate = gate(permissions(1, true, true));
    report(
        &mut log,
        &gate,
        &reporter("ci-bot", &["ci/*"]),
        &report_for("report-1", tree, "ci/unit", Conclusion::Success),
    );
    // Another Layer advances the World on a path `work` does not touch. It meets the same
    // policy on its own tree.
    let side = write(&mut log, "side-1", "side", "b.txt");
    let side_tree = tree_of(&mut log, side);
    report(
        &mut log,
        &gate,
        &reporter("ci-bot", &["ci/*"]),
        &report_for("report-side", side_tree, "ci/unit", Conclusion::Success),
    );
    assert!(matches!(
        publish(&mut log, "publish-side", "side", side),
        PublishOutcome::Published { .. }
    ));

    assert!(matches!(
        publish(&mut log, "publish-1", "work", first),
        PublishOutcome::Published { .. }
    ));
}

#[test]
fn wrong_reporter_does_not_satisfy_requirement() {
    let dir = TempDir::new().unwrap();
    let (mut log, first, tree) = seeded(&dir);
    require(&mut log, &[("ci/unit", Some("ci-bot"))]);
    let gate = gate(permissions(1, true, true));
    report(
        &mut log,
        &gate,
        &reporter("ci-2", &["ci/*"]),
        &report_for("report-1", tree, "ci/unit", Conclusion::Success),
    );

    assert_eq!(
        publish(&mut log, "publish-1", "work", first),
        refused(code::CHECKS_MISSING)
    );
}

#[test]
fn empty_policy_leaves_publish_unchanged() {
    let dir = TempDir::new().unwrap();
    let (mut log, first, _) = seeded(&dir);
    checks::set_required(&mut log, &[]).unwrap();

    assert!(matches!(
        publish(&mut log, "publish-1", "work", first),
        PublishOutcome::Published { .. }
    ));
}

#[test]
fn queued_publish_rechecks_checks_at_replay() {
    let dir = TempDir::new().unwrap();
    let (mut log, first, tree) = seeded(&dir);
    require(&mut log, &[("ci/unit", None)]);
    let gate = gate(permissions(1, true, true));

    // The publish was queued during an outage. Its first arrival finds no result.
    assert_eq!(
        publish(&mut log, "publish-queued", "work", first),
        refused(code::CHECKS_MISSING)
    );
    // The result arrives later. The queued operation keeps its one durable outcome.
    report(
        &mut log,
        &gate,
        &reporter("ci-bot", &["ci/*"]),
        &report_for("report-1", tree, "ci/unit", Conclusion::Success),
    );
    assert_eq!(
        publish(&mut log, "publish-queued", "work", first),
        refused(code::CHECKS_MISSING)
    );
    // A new operation decides afresh, and the met check lets it publish.
    assert!(matches!(
        publish(&mut log, "publish-2", "work", first),
        PublishOutcome::Published { .. }
    ));
}

/// The Publish Request `pr-1`, whose only revision is checkpoint `first`.
fn request() -> RequestId {
    RequestId::parse("pr-1").unwrap()
}

/// alice authors revision 1 on `work` at `checkpoint`, and rita approves it.
fn approved(log: &mut FileWorldLog, gate: &javelin_node::auth::Gate, checkpoint: ObjectId) {
    let author = token("alice", &[Scope::ReviewWrite], &[]);
    let open = Message::OpenRequest {
        operation: op("open-1"),
        request: request(),
        layer: layer("work"),
        checkpoint,
        base: v(0),
    };
    assert!(matches!(
        handle(log, gate, Some(&author), NOW, &open).unwrap(),
        Message::ReviewOutcome(ReviewOutcome::Accepted { revision: 1 })
    ));
    let reviewer = token("rita", &[Scope::ReviewApprove], &[]);
    let approve = Message::Approve {
        operation: op("approve-1"),
        request: request(),
        revision: 1,
    };
    assert!(matches!(
        handle(log, gate, Some(&reviewer), NOW, &approve).unwrap(),
        Message::ReviewOutcome(ReviewOutcome::Accepted { revision: 1 })
    ));
}

/// mona publishes revision 1 of `pr-1`.
fn publish_request(
    log: &mut FileWorldLog,
    gate: &javelin_node::auth::Gate,
    operation: &str,
) -> PublishOutcome {
    let maintainer = token("mona", &[Scope::Publish], &[]);
    let message = Message::PublishRequest {
        operation: op(operation),
        request: request(),
        revision: 1,
        base: v(0),
    };
    match handle(log, gate, Some(&maintainer), NOW, &message).unwrap() {
        Message::PublishOutcome(outcome) => outcome,
        other => panic!("expected a publish outcome, got {other:?}"),
    }
}

#[test]
fn publish_request_refuses_missing_check_then_publishes() {
    let dir = TempDir::new().unwrap();
    let (mut log, first, tree) = seeded(&dir);
    require(&mut log, &[("ci/unit", None)]);
    let gate = gate(permissions(1, true, true));
    approved(&mut log, &gate, first);

    assert_eq!(
        publish_request(&mut log, &gate, "publish-1"),
        refused(code::CHECKS_MISSING)
    );
    report(
        &mut log,
        &gate,
        &reporter("ci-bot", &["ci/*"]),
        &report_for("report-1", tree, "ci/unit", Conclusion::Success),
    );
    // A refused PublishRequest records nothing, so the same operation may succeed later.
    assert!(matches!(
        publish_request(&mut log, &gate, "publish-1"),
        PublishOutcome::Published { .. }
    ));
}

#[test]
fn reporter_revoked_before_publish_is_refused() {
    let dir = TempDir::new().unwrap();
    let (mut log, first, tree) = seeded(&dir);
    require(&mut log, &[("ci/unit", None)]);
    let mut gate = gate(permissions(1, true, true));
    approved(&mut log, &gate, first);
    report(
        &mut log,
        &gate,
        &reporter("ci-bot", &["ci/*"]),
        &report_for("report-1", tree, "ci/unit", Conclusion::Success),
    );
    // A newer epoch removes ci-bot's role before Publish.
    gate.apply(permissions(2, false, true)).unwrap();

    assert_eq!(
        publish_request(&mut log, &gate, "publish-1"),
        refused(code::STALE_PERMISSIONS)
    );
}

#[test]
fn policy_refuses_names_only_the_node_writes() {
    let dir = TempDir::new().unwrap();
    let mut log = new_node(&dir);
    let policy = [checks::RequiredCheck {
        name: "publish/x".into(),
        reporter: None,
    }];

    let refusal = checks::set_required(&mut log, &policy).unwrap_err();
    assert_eq!(refusal.code, code::FORBIDDEN);
}
