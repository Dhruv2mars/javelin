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

use javelin::{FileEntry, Manifest, ObjectId, RepoPath, Repository, VersionNumber};
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
    tree: ObjectId,
    op: &str,
    expected: u64,
    lose_reply: bool,
) -> Run {
    let mut receiver = spawn(&["serve", dest]);
    let mut sender = spawn(&["push", source, &tree.to_string(), op, &expected.to_string()]);
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

fn sent(run: &Run, kind: Kind) -> Vec<(ObjectId, usize)> {
    run.sent
        .iter()
        .filter_map(|m| match m {
            Message::Objects(items) => Some(items),
            _ => None,
        })
        .flatten()
        .filter(|(k, _, _)| *k == kind)
        .map(|(_, id, bytes)| (*id, bytes.len()))
        .collect()
}

#[test]
fn separate_processes_transfer_publish_and_retry_after_lost_reply() {
    let (src_dir, mut source) = repo();
    let (root, blobs, manifest) = fixture(&mut source);
    let (dst_dir, dest) = repo();
    drop(dest);
    let src = src_dir.path().to_str().unwrap();
    let dst = dst_dir.path().to_str().unwrap();

    // The receiver commits World 1, but the reply is lost in transit.
    let lost = push(src, dst, root, "op-1", 0, true);
    assert!(!lost.ok, "sender cannot know the outcome: {}", lost.report);
    assert!(
        lost.report.starts_with("error truncated"),
        "{}",
        lost.report
    );
    assert!(matches!(lost.replies.last(), Some(Message::Outcome(_))));
    assert_eq!(sent(&lost, Kind::Blob).len(), 3);
    assert_eq!(sent(&lost, Kind::Tree).len(), chunks(&source, root).len());
    let world = Repository::open(dst_dir.path()).unwrap().history().unwrap();
    assert_eq!(world.len(), 2);

    // Retrying identical input in new processes returns the durable outcome, and
    // the receiver's root answers the whole tree with one probe.
    let accepted = format!("accepted 1 {root} op-1");
    for _ in 0..2 {
        let retry = push(src, dst, root, "op-1", 0, false);
        assert!(retry.ok, "{}", retry.report);
        assert_eq!(retry.report, accepted);
        assert!(!retry.sent.iter().any(|m| matches!(m, Message::Objects(_))));
        let wants: Vec<_> = retry
            .sent
            .iter()
            .filter(|m| matches!(m, Message::Want(_)))
            .collect();
        assert_eq!(wants, vec![&Message::Want(vec![(Kind::Tree, root)])]);
    }

    // Same operation ID with changed input is refused; a new stale attempt is recorded.
    let changed = push(src, dst, root, "op-1", 1, false);
    assert!(!changed.ok);
    assert!(
        changed.report.starts_with("error operation_id_reused"),
        "{}",
        changed.report
    );
    let stale = push(src, dst, root, "op-2", 0, false);
    assert_eq!(stale.report, "stale 1");

    let dest = Repository::open(dst_dir.path()).unwrap();
    assert_eq!(
        dest.history().unwrap(),
        world,
        "retries never advance World again"
    );
    assert_eq!(
        dest.manifest_at(VersionNumber::new(1).unwrap()).unwrap(),
        manifest
    );
    for (blob, bytes) in &blobs {
        assert_eq!(&dest.read_blob(*blob).unwrap(), bytes);
    }
    assert_eq!(
        source.history().unwrap().len(),
        1,
        "source World is untouched"
    );

    // An incremental publish sends the new blob and only the chunks on its path:
    // `src`'s chunk and the root, not `bin`.
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
        .put_snapshot(&Manifest::new(entries).unwrap())
        .unwrap();
    let second = push(src, dst, next, "op-3", 1, false);
    assert_eq!(second.report, format!("accepted 2 {next} op-3"));
    assert_eq!(sent(&second, Kind::Blob), vec![(added, 9)]);
    assert_eq!(sent(&second, Kind::Tree).len(), 2);
    dest.check().unwrap();
}

#[test]
fn receiver_process_refuses_unsupported_version() {
    let (dst_dir, dest) = repo();
    drop(dest);
    let mut receiver = spawn(&["serve", dst_dir.path().to_str().unwrap()]);
    let mut stdin = receiver.stdin.take().unwrap();
    codec::write_frame(&mut stdin, &Message::Hello { min: 1, max: 1 }).unwrap();
    drop(stdin);
    let out = receiver.wait_with_output().unwrap();
    assert!(!out.status.success());
    let reply = codec::read_frame(&mut out.stdout.as_slice()).unwrap();
    assert!(matches!(reply, Some(Message::Error { code, .. }) if code == "unsupported_version"));
    let history = Repository::open(dst_dir.path()).unwrap().history().unwrap();
    assert_eq!(history.len(), 1);
}

#[test]
fn pushing_a_root_the_source_lacks_sends_nothing() {
    let (src_dir, _source) = repo();
    let (dst_dir, dest) = repo();
    let before = dest.history().unwrap();
    drop(dest);
    let absent = ObjectId::for_tree(b"\x00\x01");
    let run = push(
        src_dir.path().to_str().unwrap(),
        dst_dir.path().to_str().unwrap(),
        absent,
        "absent",
        0,
        false,
    );
    assert!(!run.ok);
    assert!(
        run.report.starts_with("error missing_object"),
        "{}",
        run.report
    );
    assert!(
        !run.sent
            .iter()
            .any(|m| matches!(m, Message::Objects(_) | Message::Publish { .. }))
    );
    let dest = Repository::open(dst_dir.path()).unwrap();
    assert_eq!(dest.history().unwrap(), before);
}

#[test]
fn successful_push_splits_want_and_object_counts() {
    let (src_dir, mut source) = repo();
    let (dst_dir, mut dest) = repo();
    let mut entries = Vec::new();
    let mut missing = BTreeSet::new();
    // MAX_WANT distinct blobs in one directory, one already at the receiver.
    for i in 0..MAX_WANT {
        let bytes = (i as u64).to_be_bytes();
        let content = source.put_blob(&bytes).unwrap();
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
    let root = source.put_snapshot(&manifest).unwrap();
    drop(dest);
    let run = push(
        src_dir.path().to_str().unwrap(),
        dst_dir.path().to_str().unwrap(),
        root,
        "boundary",
        0,
        false,
    );
    assert!(run.ok, "{}", run.report);
    assert_eq!(run.report, format!("accepted 1 {root} boundary"));
    let wants: Vec<_> = run
        .sent
        .iter()
        .filter_map(|m| match m {
            Message::Want(items) => Some(items),
            _ => None,
        })
        .collect();
    assert!(wants.iter().all(|items| items.len() <= MAX_WANT));
    let offered: Vec<_> = wants.into_iter().flatten().collect();
    assert_eq!(
        offered.len(),
        offered.iter().collect::<BTreeSet<_>>().len(),
        "each object is offered once"
    );
    let blobs: BTreeSet<_> = sent(&run, Kind::Blob)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(sent(&run, Kind::Blob).len(), missing.len());
    assert_eq!(blobs, missing);
    let batches: Vec<_> = run
        .sent
        .iter()
        .filter_map(|m| match m {
            Message::Objects(items) => Some(items.len()),
            _ => None,
        })
        .collect();
    assert!(batches.iter().all(|n| *n <= MAX_OBJECTS));
    assert_eq!(batches[..3], [MAX_OBJECTS; 3]);
    let dest = Repository::open(dst_dir.path()).unwrap();
    assert_eq!(
        dest.manifest_at(VersionNumber::new(1).unwrap()).unwrap(),
        manifest
    );
    dest.check().unwrap();
}

#[test]
fn successful_push_splits_legal_blobs_at_frame_byte_limit() {
    let (src_dir, mut source) = repo();
    let mut entries = Vec::new();
    // Two individually legal blobs cannot share a MAX_FRAME message.
    for i in 0..2 {
        let content = source.put_blob(&vec![i; MAX_BLOB]).unwrap();
        entries.push((
            RepoPath::parse(&format!("large-{i}")).unwrap(),
            FileEntry::Regular {
                content,
                executable: false,
            },
        ));
    }
    let manifest = Manifest::new(entries).unwrap();
    let root = source.put_snapshot(&manifest).unwrap();
    let (dst_dir, dest) = repo();
    drop(dest);
    let run = push(
        src_dir.path().to_str().unwrap(),
        dst_dir.path().to_str().unwrap(),
        root,
        "boundary",
        0,
        false,
    );
    assert!(run.ok, "{}", run.report);
    let batches: Vec<_> = run
        .sent
        .iter()
        .filter_map(|m| match m {
            Message::Objects(items) => Some(items),
            _ => None,
        })
        .collect();
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].len(), 1);
    assert_eq!(batches[0][0].2.len(), MAX_BLOB);
    // The second frame holds the other blob and then the root chunk.
    assert_eq!(batches[1].len(), 2);
    let dest = Repository::open(dst_dir.path()).unwrap();
    assert_eq!(
        dest.manifest_at(VersionNumber::new(1).unwrap()).unwrap(),
        manifest
    );
}
