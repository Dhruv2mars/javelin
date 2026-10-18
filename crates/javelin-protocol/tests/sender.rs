//! The reference sender against a scripted malicious receiver. Contradictory or
//! amplifying replies are refused rather than trusted.

mod support;

use std::{io::pipe, thread};

use javelin::{ObjectId, OperationId, VersionNumber};
use javelin_protocol::{
    Kind, Message, Outcome,
    codec::{read_frame, write_frame},
    push,
};
use support::*;

/// Runs `push` against a fake receiver whose replies come from `reply`.
/// Returns the push result and every message the sender wrote.
fn against(
    reply: impl Fn(&Message) -> Message + Send + 'static,
) -> (
    Result<Outcome, javelin_protocol::ProtocolError>,
    Vec<Message>,
    ObjectId,
) {
    let (_s, mut source) = repo();
    let (id, _, _) = fixture(&mut source);
    let (mut from_sender, to_receiver) = pipe().unwrap();
    let (from_receiver, mut to_sender) = pipe().unwrap();
    let fake = thread::spawn(move || {
        let mut seen = Vec::new();
        while let Ok(Some(message)) = read_frame(&mut from_sender) {
            let answer = reply(&message);
            seen.push(message);
            if write_frame(&mut to_sender, &answer).is_err() {
                break;
            }
        }
        seen
    });
    let result = push(
        from_receiver,
        to_receiver,
        &source,
        id,
        &OperationId::parse("op-1").unwrap(),
        VersionNumber::new(0).unwrap(),
    );
    (result, fake.join().unwrap(), id)
}

type Items = Vec<(Kind, ObjectId)>;

fn script(
    missing: fn(&[(Kind, ObjectId)]) -> Items,
    outcome: fn(ObjectId) -> Outcome,
) -> impl Fn(&Message) -> Message {
    move |message| match message {
        Message::Hello { .. } => Message::HelloOk { version: 2 },
        Message::Want(items) => Message::Missing(missing(items)),
        Message::Objects(o) => Message::Stored {
            count: o.len() as u32,
        },
        Message::Publish { tree, .. } => Message::Outcome(outcome(*tree)),
        _ => Message::Error {
            code: "unexpected_message".into(),
            detail: String::new(),
        },
    }
}

fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}

fn accepted(tree: ObjectId) -> Outcome {
    Outcome::Accepted {
        number: v(1),
        tree,
        operation: OperationId::parse("op-1").unwrap(),
    }
}

#[test]
fn accepts_a_consistent_outcome() {
    let (result, seen, id) = against(script(|items| items.to_vec(), accepted));
    assert_eq!(result.unwrap(), accepted(id));
    let blobs: usize = seen
        .iter()
        .map(|m| match m {
            Message::Objects(o) => o.iter().filter(|(k, _, _)| *k == Kind::Blob).count(),
            _ => 0,
        })
        .sum();
    assert_eq!(blobs, 3);
}

#[test]
fn refuses_duplicate_missing_items_before_sending_objects() {
    let (result, seen, _) = against(script(
        |items| std::iter::repeat_n(items[0], 2).collect(),
        accepted,
    ));
    assert_eq!(result.unwrap_err().code, "unexpected_message");
    assert!(!seen.iter().any(|m| matches!(m, Message::Objects(_))));
}

#[test]
fn refuses_unoffered_missing_items() {
    let (result, _, _) = against(script(|items| vec![(Kind::Blob, items[0].1)], accepted));
    assert_eq!(result.unwrap_err().code, "unexpected_message");
}

#[test]
fn refuses_outcomes_that_do_not_answer_the_publish() {
    let cases: [fn(ObjectId) -> Outcome; 5] = [
        |m| Outcome::Accepted {
            number: v(0),
            tree: m,
            operation: OperationId::parse("op-1").unwrap(),
        },
        |m| Outcome::Accepted {
            number: v(2),
            tree: m,
            operation: OperationId::parse("op-1").unwrap(),
        },
        |_| accepted(ObjectId::parse(&"ab".repeat(32)).unwrap()),
        |m| Outcome::Accepted {
            number: v(1),
            tree: m,
            operation: OperationId::parse("unrelated").unwrap(),
        },
        |_| Outcome::Stale { actual: v(0) },
    ];
    for outcome in cases {
        let (result, _, _) = against(script(|items| items.to_vec(), outcome));
        assert_eq!(result.unwrap_err().code, "unexpected_message");
    }
    let (result, _, _) = against(script(
        |items| items.to_vec(),
        |_| Outcome::Stale { actual: v(4) },
    ));
    assert_eq!(result.unwrap(), Outcome::Stale { actual: v(4) });
}
