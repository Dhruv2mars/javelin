//! The Durable Object log against a running `tools/spike-do` object, through the node's
//! `DoLog` client. Ignored by default: run it against workerd with
//! `JAVELIN_DO_URL=127.0.0.1:18787 cargo test -p javelin-node --test log_do -- --ignored`.

use std::{
    net::{SocketAddr, ToSocketAddrs},
    time::{SystemTime, UNIX_EPOCH},
};

use javelin::{LayerId, ObjectId, OperationId, PublishAttribution, VersionNumber};
use javelin_node::{CommitReply, ComposedVersion, DoLog, Publish};
use javelin_protocol::codec::PublishOutcome;

fn target() -> SocketAddr {
    std::env::var("JAVELIN_DO_URL")
        .expect("set JAVELIN_DO_URL to host:port of a running spike-do")
        .to_socket_addrs()
        .expect("JAVELIN_DO_URL resolves")
        .next()
        .expect("JAVELIN_DO_URL has an address")
}

/// A repository name nothing else uses, since the object persists across runs.
fn repo() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("rust-{nanos:x}")
}

fn id(s: &str) -> ObjectId {
    ObjectId::for_blob(s.as_bytes())
}

fn version(s: &str) -> ComposedVersion {
    ComposedVersion {
        id: id(&format!("version-{s}")),
        tree: id(&format!("tree-{s}")),
    }
}

#[test]
#[ignore = "needs a running spike-do object; see the file header"]
fn commits_once_replay_and_refusals_hold_through_the_client() {
    let log = DoLog::new(target(), repo());
    let (genesis, zero) = log.head().unwrap();
    assert_eq!(genesis, VersionNumber::new(0).unwrap());
    assert_eq!(zero, ObjectId::parse(&"0".repeat(64)).unwrap());

    let layer = LayerId::parse("work").unwrap();
    let expected = id("checkpoint-1");
    log.create_layer(&layer, expected).unwrap();

    let operation = OperationId::parse("publish-1").unwrap();
    let human = PublishAttribution::Human;
    let publish = Publish {
        operation: &operation,
        layer: &layer,
        expected,
        base: genesis,
        attribution: &human,
    };
    let composed = version("one");
    let first = log.commit(&publish, genesis, &composed).unwrap();
    assert_eq!(
        first,
        CommitReply::Published {
            number: VersionNumber::new(1).unwrap(),
            id: composed.id,
            tree: composed.tree,
            replayed: false,
        }
    );

    // The same request after a lost reply replays the recorded version.
    let replay = log.commit(&publish, genesis, &composed).unwrap();
    assert!(
        matches!(replay, CommitReply::Published { replayed: true, number, .. } if number.get() == 1)
    );

    // The same operation with another request is refused.
    let agent = PublishAttribution::Agent {
        context: None,
        allow_missing_trace: false,
    };
    let other = Publish {
        attribution: &agent,
        ..publish
    };
    assert_eq!(
        log.commit(&other, genesis, &composed)
            .unwrap()
            .into_outcome(),
        Some(PublishOutcome::Refused {
            code: "operation_id_reused".into()
        })
    );

    // A second Layer composed on the old head is told the head moved.
    let second = LayerId::parse("second").unwrap();
    log.create_layer(&second, id("checkpoint-2")).unwrap();
    let late = Publish {
        operation: &OperationId::parse("publish-2").unwrap(),
        layer: &second,
        expected: id("checkpoint-2"),
        base: genesis,
        attribution: &human,
    };
    assert_eq!(
        log.commit(&late, genesis, &version("two")).unwrap(),
        CommitReply::HeadMoved {
            head: VersionNumber::new(1).unwrap()
        }
    );

    // Composed on the current head, it commits as version 2.
    let current = VersionNumber::new(1).unwrap();
    assert!(matches!(
        log.commit(&late, current, &version("two")).unwrap(),
        CommitReply::Published { replayed: false, number, .. } if number.get() == 2
    ));

    // A Layer that already published is refused, not re-published.
    assert_eq!(
        log.commit(
            &Publish {
                operation: &OperationId::parse("publish-3").unwrap(),
                ..late
            },
            VersionNumber::new(2).unwrap(),
            &version("three"),
        )
        .unwrap()
        .into_outcome(),
        Some(PublishOutcome::Refused {
            code: "layer_published".into()
        })
    );
}
