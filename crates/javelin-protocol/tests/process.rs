//! Separate sender and receiver processes over two separate repositories. The test
//! process only relays frames, as a network would, and can lose the Publish reply.

mod support;

use std::{
    collections::BTreeSet,
    io::{Read, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};

use javelin::{FileEntry, Manifest, ObjectId, RepoPath, Repository};
use javelin_protocol::{Kind, Message, codec, limits::*};
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_javelin-protocol");

fn read_raw(r: &mut impl Read) -> Option<Vec<u8>> {
    let mut header = [0; 4];
    r.read_exact(&mut header).ok()?;
    let mut body = vec![0; u32::from_be_bytes(header) as usize];
    r.read_exact(&mut body).ok()?;
    Some(body)
}

/// Forwards raw frames unchanged and records the decoded messages it saw.
fn relay(
    mut from: ChildStdout,
    mut to: ChildStdin,
    seen: Arc<Mutex<Vec<Message>>>,
    drop_outcome: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while let Some(body) = read_raw(&mut from) {
            assert!(body.len() <= MAX_FRAME, "peer exceeded the frame limit");
            let message = codec::decode(&body).expect("peer sent a valid frame");
            let is_outcome = matches!(message, Message::Outcome(_));
            seen.lock().unwrap().push(message);
            if drop_outcome && is_outcome {
                return; // Lose the reply and close the sender's input.
            }
            let mut frame = (body.len() as u32).to_be_bytes().to_vec();
            frame.extend(body);
            if to.write_all(&frame).is_err() {
                return;
            }
        }
    })
}

struct Run {
    ok: bool,
    report: String,
    sent: Vec<Message>,
    replies: Vec<Message>,
}

fn spawn(args: &[&str]) -> Child {
    Command::new(BIN)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn push(
    source: &str,
    dest: &str,
    manifest: ObjectId,
    op: &str,
    expected: u64,
    lose_reply: bool,
) -> Run {
    let mut receiver = spawn(&["serve", dest]);
    let mut sender = spawn(&[
        "push",
        source,
        &manifest.to_string(),
        op,
        &expected.to_string(),
    ]);
    let sent = Arc::new(Mutex::new(Vec::new()));
    let replies = Arc::new(Mutex::new(Vec::new()));
    let up = relay(
        sender.stdout.take().unwrap(),
        receiver.stdin.take().unwrap(),
        sent.clone(),
        false,
    );
    let down = relay(
        receiver.stdout.take().unwrap(),
        sender.stdin.take().unwrap(),
        replies.clone(),
        lose_reply,
    );
    let sender_out = sender.wait_with_output().unwrap();
    up.join().unwrap();
    down.join().unwrap();
    let receiver_out = receiver.wait_with_output().unwrap();
    assert!(
        receiver_out.status.success() || !receiver_out.stderr.is_empty(),
        "receiver failures are reported"
    );
    let report = String::from_utf8(sender_out.stderr).unwrap();
    Run {
        ok: sender_out.status.success(),
        report: report.trim().to_owned(),
        sent: sent.lock().unwrap().clone(),
        replies: replies.lock().unwrap().clone(),
    }
}

fn blobs_sent(run: &Run) -> usize {
    run.sent
        .iter()
        .map(|m| match m {
            Message::Blobs(b) => b.len(),
            _ => 0,
        })
        .sum()
}

#[test]
fn separate_processes_transfer_publish_and_retry_after_lost_reply() {
    let (src_dir, mut source) = repo();
    let (id, blobs, manifest) = fixture(&mut source);
    let (dst_dir, dest) = repo();
    drop(dest);
    let src = src_dir.path().to_str().unwrap();
    let dst = dst_dir.path().to_str().unwrap();

    // The receiver commits World 1, but the reply is lost in transit.
    let lost = push(src, dst, id, "op-1", 0, true);
    assert!(!lost.ok, "sender cannot know the outcome: {}", lost.report);
    assert!(
        lost.report.starts_with("error truncated"),
        "{}",
        lost.report
    );
    assert!(matches!(lost.replies.last(), Some(Message::Outcome(_))));
    assert_eq!(blobs_sent(&lost), 3);
    let world = Repository::open(dst_dir.path()).unwrap().history().unwrap();
    assert_eq!(world.len(), 2);

    // Retrying identical input in new processes returns the durable outcome.
    let accepted = format!("accepted 1 {id} op-1");
    for _ in 0..2 {
        let retry = push(src, dst, id, "op-1", 0, false);
        assert!(retry.ok, "{}", retry.report);
        assert_eq!(retry.report, accepted);
        assert_eq!(blobs_sent(&retry), 0, "have/want skips stored objects");
        assert!(
            !retry
                .sent
                .iter()
                .any(|m| matches!(m, Message::Manifests(_)))
        );
    }

    // Same operation ID with changed input is refused; a new stale attempt is recorded.
    let changed = push(src, dst, id, "op-1", 1, false);
    assert!(!changed.ok);
    assert!(
        changed.report.starts_with("error operation_id_reused"),
        "{}",
        changed.report
    );
    let stale = push(src, dst, id, "op-2", 0, false);
    assert_eq!(stale.report, "stale 1");

    let dest = Repository::open(dst_dir.path()).unwrap();
    assert_eq!(
        dest.history().unwrap(),
        world,
        "retries never advance World again"
    );
    assert_eq!(dest.read_manifest(id).unwrap(), manifest);
    for (blob, bytes) in &blobs {
        assert_eq!(&dest.read_blob(*blob).unwrap(), bytes);
    }
    assert_eq!(
        source.history().unwrap().len(),
        1,
        "source World is untouched"
    );

    // An incremental publish transfers only the new blob.
    let added = source.put_blob(b"new file\n").unwrap();
    let mut entries: Vec<_> = manifest.entries().clone().into_iter().collect();
    entries.push((
        RepoPath::parse("src/new.txt").unwrap(),
        FileEntry::Regular {
            content: added,
            executable: false,
        },
    ));
    let next = source
        .put_manifest(&Manifest::new(entries).unwrap())
        .unwrap();
    let second = push(src, dst, next, "op-3", 1, false);
    assert_eq!(second.report, format!("accepted 2 {next} op-3"));
    assert_eq!(blobs_sent(&second), 1);
    dest.check().unwrap();
}

#[test]
fn receiver_process_refuses_unsupported_version() {
    let (dst_dir, dest) = repo();
    drop(dest);
    let mut receiver = spawn(&["serve", dst_dir.path().to_str().unwrap()]);
    let mut stdin = receiver.stdin.take().unwrap();
    codec::write_frame(&mut stdin, &Message::Hello { min: 2, max: 2 }).unwrap();
    drop(stdin);
    let out = receiver.wait_with_output().unwrap();
    assert!(!out.status.success());
    let reply = codec::read_frame(&mut out.stdout.as_slice()).unwrap();
    assert!(matches!(reply, Some(Message::Error { code, .. }) if code == "unsupported_version"));
    let history = Repository::open(dst_dir.path()).unwrap().history().unwrap();
    assert_eq!(history.len(), 1);
}

#[test]
fn invalid_native_manifest_fails_before_object_negotiation() {
    let (src_dir, mut source) = repo();
    let blob = source.put_blob(b"must never reach the receiver").unwrap();
    let manifest = Manifest::new([(
        RepoPath::parse(&"p".repeat(MAX_PATH + 1)).unwrap(),
        FileEntry::Regular {
            content: blob,
            executable: false,
        },
    )])
    .unwrap();
    let id = source.put_manifest(&manifest).unwrap();
    let (dst_dir, dest) = repo();
    let before = dest.history().unwrap();
    drop(dest);

    let run = push(
        src_dir.path().to_str().unwrap(),
        dst_dir.path().to_str().unwrap(),
        id,
        "invalid-manifest",
        0,
        false,
    );
    assert!(!run.ok);
    assert!(
        run.report.starts_with("error limit_exceeded"),
        "{}",
        run.report
    );
    assert!(
        matches!(run.sent.as_slice(), [Message::Hello { .. }]),
        "preflight must precede Want, object transfer and Publish"
    );
    let dest = Repository::open(dst_dir.path()).unwrap();
    assert_eq!(dest.history().unwrap(), before);
    assert!(!dest.has_blob(blob).unwrap());
    assert!(!dest.has_manifest(id).unwrap());
}

/// Checks actual missing IDs, deduplication, accepted publication and stored data.
fn assert_published(
    run: &Run,
    dest: &Repository,
    id: ObjectId,
    manifest: &Manifest,
    missing: &BTreeSet<ObjectId>,
) {
    assert!(run.ok, "{}", run.report);
    assert_eq!(run.report, format!("accepted 1 {id} boundary"));
    let sent: Vec<_> = run
        .sent
        .iter()
        .filter_map(|m| match m {
            Message::Blobs(items) => Some(items.iter().map(|(id, _)| *id)),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(sent.len(), missing.len(), "each missing blob is sent once");
    assert_eq!(sent.into_iter().collect::<BTreeSet<_>>(), *missing);
    let manifests: Vec<_> = run
        .sent
        .iter()
        .filter_map(|m| match m {
            Message::Manifests(items) => Some(items.iter().map(|(id, _)| *id)),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(manifests, vec![id]);
    assert!(matches!(run.replies.last(), Some(Message::Outcome(
        javelin_protocol::Outcome::Accepted { manifest: accepted, .. }
    )) if *accepted == id));
    assert_eq!(dest.history().unwrap().len(), 2);
    assert_eq!(dest.read_manifest(id).unwrap(), *manifest);
    for message in &run.sent {
        if let Message::Blobs(items) = message {
            for (id, bytes) in items {
                assert_eq!(dest.read_blob(*id).unwrap(), *bytes);
            }
        }
    }
}

#[test]
fn successful_push_splits_want_and_blob_counts() {
    let (src_dir, mut source) = repo();
    let (dst_dir, mut dest) = repo();
    let mut entries = Vec::new();
    let mut missing = BTreeSet::new();
    let mut offered = BTreeSet::new();
    // One more offered object than MAX_WANT, with one blob already present.
    for i in 0..MAX_WANT {
        let bytes = (i as u64).to_be_bytes();
        let content = source.put_blob(&bytes).unwrap();
        offered.insert((Kind::Blob, content));
        if i == 0 {
            dest.put_blob(&bytes).unwrap();
        } else {
            missing.insert(content);
        }
        entries.push((
            RepoPath::parse(&format!("file-{i:04}")).unwrap(),
            FileEntry::Regular {
                content,
                executable: false,
            },
        ));
    }
    // Shared content is offered and transferred only once.
    entries.push((RepoPath::parse("shared").unwrap(), entries[1].1.clone()));
    let manifest = Manifest::new(entries).unwrap();
    let id = source.put_manifest(&manifest).unwrap();
    offered.insert((Kind::Manifest, id));
    drop(dest);
    let run = push(
        src_dir.path().to_str().unwrap(),
        dst_dir.path().to_str().unwrap(),
        id,
        "boundary",
        0,
        false,
    );
    let wants: Vec<_> = run
        .sent
        .iter()
        .filter_map(|m| match m {
            Message::Want(items) => Some(items),
            _ => None,
        })
        .collect();
    assert_eq!(
        wants.iter().map(|items| items.len()).collect::<Vec<_>>(),
        vec![MAX_WANT, 1]
    );
    let actual: Vec<_> = wants.into_iter().flatten().copied().collect();
    assert_eq!(actual.len(), offered.len());
    assert_eq!(actual.into_iter().collect::<BTreeSet<_>>(), offered);
    let batches: Vec<_> = run
        .sent
        .iter()
        .filter_map(|m| match m {
            Message::Blobs(items) => Some(items.len()),
            _ => None,
        })
        .collect();
    assert_eq!(
        batches,
        vec![MAX_BLOBS, MAX_BLOBS, MAX_BLOBS, MAX_BLOBS - 1]
    );
    let dest = Repository::open(dst_dir.path()).unwrap();
    assert_published(&run, &dest, id, &manifest, &missing);
}

#[test]
fn successful_push_splits_legal_blobs_at_frame_byte_limit() {
    let (src_dir, mut source) = repo();
    let mut entries = Vec::new();
    let mut missing = BTreeSet::new();
    // Two individually legal blobs cannot share a MAX_FRAME message.
    for i in 0..2 {
        let content = source.put_blob(&vec![i; MAX_BLOB]).unwrap();
        missing.insert(content);
        entries.push((
            RepoPath::parse(&format!("large-{i}")).unwrap(),
            FileEntry::Regular {
                content,
                executable: false,
            },
        ));
    }
    let manifest = Manifest::new(entries).unwrap();
    let id = source.put_manifest(&manifest).unwrap();
    let (dst_dir, dest) = repo();
    drop(dest);
    let run = push(
        src_dir.path().to_str().unwrap(),
        dst_dir.path().to_str().unwrap(),
        id,
        "boundary",
        0,
        false,
    );
    let batches: Vec<_> = run
        .sent
        .iter()
        .filter_map(|m| match m {
            Message::Blobs(items) => Some(items),
            _ => None,
        })
        .collect();
    assert_eq!(batches.len(), 2);
    for batch in batches {
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].1.len(), MAX_BLOB);
    }
    let dest = Repository::open(dst_dir.path()).unwrap();
    assert_published(&run, &dest, id, &manifest, &missing);
}
