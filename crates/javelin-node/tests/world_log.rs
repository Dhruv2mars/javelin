use javelin::{
    Error, FileEntry, LayerId, ObjectId, OperationId, PublishAttribution, RepoPath, SparseChanges,
    VersionNumber,
};
use javelin_node::FileWorldLog;
use javelin_protocol::{
    codec::{PublishOutcome, code},
    receiver::{WorldLog, refusal},
};
use tempfile::TempDir;

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn layer(s: &str) -> LayerId {
    LayerId::parse(s).unwrap()
}
fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}
fn refused(code: &str) -> PublishOutcome {
    PublishOutcome::Refused { code: code.into() }
}

/// One file written by `layer`, as a checkpoint on top of the Layer's head.
fn write(
    log: &mut FileWorldLog,
    operation: &str,
    layer_name: &str,
    path: &str,
    bytes: &[u8],
) -> ObjectId {
    let content = log.repository().put_blob(bytes).unwrap();
    let changes = SparseChanges::new([(
        RepoPath::parse(path).unwrap(),
        Some(FileEntry::Regular {
            content,
            executable: false,
        }),
    )])
    .unwrap();
    log.append(&op(operation), &layer(layer_name), &changes)
        .unwrap()
        .checkpoint()
        .object()
}

/// A node with Layer `work` over World 1, holding one checkpoint. Returns its head.
fn seeded(dir: &TempDir) -> (FileWorldLog, ObjectId) {
    let mut log = FileWorldLog::open(dir.path()).unwrap();
    log.create_layer(&layer("work"), v(0)).unwrap();
    let expected = write(&mut log, "write-1", "work", "a.txt", b"one");
    (log, expected)
}

fn publish(
    log: &mut FileWorldLog,
    operation: &str,
    layer_name: &str,
    expected: ObjectId,
    base: u64,
) -> PublishOutcome {
    log.publish_layer(
        &op(operation),
        &layer(layer_name),
        expected,
        v(base),
        &PublishAttribution::Human,
    )
    .unwrap()
}

#[test]
fn publish_is_idempotent_and_a_layer_publishes_once() {
    let dir = TempDir::new().unwrap();
    let (mut log, expected) = seeded(&dir);

    let first = publish(&mut log, "publish-1", "work", expected, 0);
    let PublishOutcome::Published { number, .. } = first.clone() else {
        panic!("expected publication: {first:?}");
    };
    assert_eq!(number, v(1));
    assert_eq!(publish(&mut log, "publish-1", "work", expected, 0), first);
    assert_eq!(
        publish(&mut log, "publish-2", "work", expected, 0),
        refused(code::LAYER_PUBLISHED)
    );
    assert_eq!(log.head().unwrap().number, v(1));
}

#[test]
fn reused_operation_with_different_input_is_refused() {
    let dir = TempDir::new().unwrap();
    let (mut log, expected) = seeded(&dir);
    publish(&mut log, "publish-1", "work", expected, 0);

    let outcome = log
        .publish_layer(
            &op("publish-1"),
            &layer("work"),
            expected,
            v(0),
            &PublishAttribution::Agent {
                context: None,
                allow_missing_trace: false,
            },
        )
        .unwrap();
    assert_eq!(outcome, refused(code::OPERATION_ID_REUSED));
}

#[test]
fn stale_expected_head_reports_the_current_head() {
    let dir = TempDir::new().unwrap();
    let (mut log, first) = seeded(&dir);
    let current = write(&mut log, "write-2", "work", "b.txt", b"two");

    assert_eq!(
        publish(&mut log, "publish-1", "work", first, 0),
        PublishOutcome::LayerStale { head: current }
    );
    assert_eq!(log.head().unwrap().number, v(0));
}

#[test]
fn unknown_layer_and_wrong_base_are_refused() {
    let dir = TempDir::new().unwrap();
    let (mut log, expected) = seeded(&dir);

    assert_eq!(
        publish(&mut log, "publish-1", "missing", expected, 0),
        refused(refusal::UNKNOWN_LAYER)
    );
    assert_eq!(
        publish(&mut log, "publish-2", "work", expected, 2),
        refused(code::INVALID_INPUT)
    );
}

#[test]
fn a_layer_touching_an_advanced_path_conflicts() {
    let dir = TempDir::new().unwrap();
    let (mut log, expected) = seeded(&dir);
    log.create_layer(&layer("other"), v(0)).unwrap();
    let other = write(&mut log, "write-3", "other", "a.txt", b"other");

    assert!(matches!(
        publish(&mut log, "publish-1", "work", expected, 0),
        PublishOutcome::Published { .. }
    ));
    match publish(&mut log, "publish-2", "other", other, 0) {
        PublishOutcome::Conflict { paths } => {
            assert_eq!(paths, vec![RepoPath::parse("a.txt").unwrap()]);
        }
        other => panic!("expected conflict: {other:?}"),
    }
}

#[test]
fn recorded_outcome_survives_a_restart() {
    let dir = TempDir::new().unwrap();
    let (mut log, expected) = seeded(&dir);
    let first = publish(&mut log, "publish-1", "work", expected, 0);
    drop(log);

    let mut reopened = FileWorldLog::open(dir.path()).unwrap();
    assert_eq!(
        publish(&mut reopened, "publish-1", "work", expected, 0),
        first
    );
    assert_eq!(reopened.head().unwrap().number, v(1));
}

#[test]
fn second_node_on_the_same_directory_is_refused() {
    let dir = TempDir::new().unwrap();
    let _held = FileWorldLog::open(dir.path()).unwrap();
    assert!(matches!(
        FileWorldLog::open(dir.path()),
        Err(Error::Busy(_))
    ));
}

#[test]
fn a_stale_refusal_replays_after_the_layer_is_published_by_another_operation() {
    let dir = TempDir::new().unwrap();
    let (mut log, first) = seeded(&dir);
    let current = write(&mut log, "write-2", "work", "b.txt", b"two");
    let stale = PublishOutcome::LayerStale { head: current };
    assert_eq!(publish(&mut log, "publish-1", "work", first, 0), stale);

    assert!(matches!(
        publish(&mut log, "publish-2", "work", current, 0),
        PublishOutcome::Published { .. }
    ));
    // The identical retry replays the recorded refusal, not what the Layer is now.
    assert_eq!(publish(&mut log, "publish-1", "work", first, 0), stale);
    assert_eq!(log.head().unwrap().number, v(1));
}

#[test]
fn a_stale_refusal_replays_after_a_restart() {
    let dir = TempDir::new().unwrap();
    let (mut log, first) = seeded(&dir);
    let current = write(&mut log, "write-2", "work", "b.txt", b"two");
    let stale = PublishOutcome::LayerStale { head: current };
    assert_eq!(publish(&mut log, "publish-1", "work", first, 0), stale);
    drop(log);

    // After the restart another operation publishes the Layer, so the refusal can only
    // come back from the persisted record.
    let mut reopened = FileWorldLog::open(dir.path()).unwrap();
    assert!(matches!(
        publish(&mut reopened, "publish-2", "work", current, 0),
        PublishOutcome::Published { .. }
    ));
    assert_eq!(publish(&mut reopened, "publish-1", "work", first, 0), stale);
    assert_eq!(
        reopened
            .publish_layer(
                &op("publish-1"),
                &layer("work"),
                first,
                v(0),
                &PublishAttribution::Agent {
                    context: None,
                    allow_missing_trace: false,
                },
            )
            .unwrap(),
        refused(code::OPERATION_ID_REUSED)
    );
    assert_eq!(reopened.head().unwrap().number, v(1));
}

#[test]
fn a_recorded_operation_refuses_each_changed_request_field() {
    let dir = TempDir::new().unwrap();
    let (mut log, expected) = seeded(&dir);
    log.create_layer(&layer("other"), v(0)).unwrap();
    let reused = refused(code::OPERATION_ID_REUSED);
    let published = publish(&mut log, "publish-1", "work", expected, 0);
    assert!(matches!(published, PublishOutcome::Published { .. }));

    let other_expected = write(&mut log, "write-2", "other", "b.txt", b"two");
    let agent = PublishAttribution::Agent {
        context: None,
        allow_missing_trace: false,
    };
    let variants = [
        ("layer", "other", expected, 0, PublishAttribution::Human),
        (
            "expected",
            "work",
            other_expected,
            0,
            PublishAttribution::Human,
        ),
        ("base", "work", expected, 1, PublishAttribution::Human),
        ("attribution", "work", expected, 0, agent),
    ];
    for (field, layer_name, expected, base, attribution) in variants {
        let outcome = log
            .publish_layer(
                &op("publish-1"),
                &layer(layer_name),
                expected,
                v(base),
                &attribution,
            )
            .unwrap();
        assert_eq!(outcome, reused, "changing {field} must reuse the operation");
    }
    assert_eq!(
        publish(&mut log, "publish-1", "work", expected, 0),
        published
    );
}
