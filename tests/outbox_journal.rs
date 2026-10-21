use std::{fs, time::Duration};

use javelin::{
    OperationId,
    outbox::{Entry, Fault, Kind, Outbox, Transport, backoff, journal_path},
};
use tempfile::TempDir;

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}

/// Answers each record with `reply(seq)` and records the sequence numbers it saw.
struct Scripted<F: FnMut(&Entry) -> Result<Vec<u8>, Fault>> {
    reply: F,
    seen: Vec<u64>,
}

impl<F: FnMut(&Entry) -> Result<Vec<u8>, Fault>> Transport for Scripted<F> {
    fn deliver(&mut self, entry: &Entry) -> Result<Vec<u8>, Fault> {
        self.seen.push(entry.seq);
        (self.reply)(entry)
    }
}

fn scripted(
    reply: impl FnMut(&Entry) -> Result<Vec<u8>, Fault>,
) -> Scripted<impl FnMut(&Entry) -> Result<Vec<u8>, Fault>> {
    Scripted {
        reply,
        seen: Vec::new(),
    }
}

fn three_records(root: &TempDir) -> Outbox {
    let mut outbox = Outbox::open(root.path(), "demo").unwrap();
    for (scope, operation) in [("a", "op-1"), ("b", "op-2"), ("a", "op-3")] {
        outbox
            .enqueue(Kind::Publish, scope, &op(operation), operation.as_bytes())
            .unwrap();
    }
    outbox
}

#[test]
fn queued_records_survive_reopen_in_order_with_their_fields() {
    let root = TempDir::new().unwrap();
    drop(three_records(&root));

    let outbox = Outbox::open(root.path(), "demo").unwrap();
    let pending: Vec<_> = outbox.pending().collect();
    assert_eq!(pending.len(), 3);
    assert_eq!(pending[0].seq, 1);
    assert_eq!(pending[0].kind, Kind::Publish);
    assert_eq!(pending[0].scope, "a");
    assert_eq!(pending[0].operation, op("op-1"));
    assert_eq!(pending[0].payload, b"op-1");
    assert!(outbox.damage().is_none());
    assert_eq!(
        journal_path(root.path(), "demo").unwrap(),
        root.path().join(".javelin/outbox/demo.journal")
    );
}

#[test]
fn a_second_handle_is_refused_while_the_first_is_open() {
    let root = TempDir::new().unwrap();
    let mut first = Outbox::open(root.path(), "demo").unwrap();
    first
        .enqueue(Kind::Upload, "a", &op("op-1"), b"one")
        .unwrap();
    assert!(Outbox::open(root.path(), "demo").is_err());
    drop(first);

    // Had the second handle been allowed, its sequence 1 would have overwritten op-1.
    let second = Outbox::open(root.path(), "demo").unwrap();
    assert_eq!(second.entry(1).unwrap().operation, op("op-1"));
    assert!(second.damage().is_none());
}

#[test]
fn replay_delivers_each_record_once_and_acknowledged_records_stay_acknowledged() {
    let root = TempDir::new().unwrap();
    let mut outbox = three_records(&root);
    let mut transport = scripted(|entry| Ok(format!("ok-{}", entry.seq).into_bytes()));

    let report = outbox.replay(&mut transport).unwrap();
    assert_eq!(transport.seen, [1, 2, 3]);
    assert_eq!(report.delivered.len(), 3);
    assert_eq!(report.delivered[2].outcome, b"ok-3");
    assert!(report.stalled.is_none());
    assert_eq!(outbox.pending().count(), 0);
    drop(outbox);

    let mut reopened = Outbox::open(root.path(), "demo").unwrap();
    assert_eq!(reopened.pending().count(), 0);
    assert_eq!(
        reopened.entry(2).unwrap().outcome.as_deref(),
        Some(&b"ok-2"[..])
    );
    let mut again = scripted(|_| panic!("acknowledged records are not resent"));
    assert_eq!(reopened.replay(&mut again).unwrap(), Default::default());
}

#[test]
fn a_fault_stops_the_pass_and_leaves_later_records_queued() {
    let root = TempDir::new().unwrap();
    let mut outbox = three_records(&root);
    let mut transport = scripted(|entry| match entry.seq {
        2 => Err(Fault::NodeUnavailable("connection refused".into())),
        _ => Ok(b"ok".to_vec()),
    });

    let report = outbox.replay(&mut transport).unwrap();
    assert_eq!(transport.seen, [1, 2]);
    assert_eq!(report.delivered.len(), 1);
    let stall = report.stalled.unwrap();
    assert_eq!(stall.seq, 2);
    assert_eq!(stall.fault.code(), "node_unavailable");
    assert_eq!(outbox.pending().map(|e| e.seq).collect::<Vec<_>>(), [2, 3]);
}

#[test]
fn an_expired_credential_keeps_the_record_queued_with_its_code() {
    let root = TempDir::new().unwrap();
    let mut outbox = three_records(&root);
    let mut transport = scripted(|_| Err(Fault::ControlPlaneUnavailable("token expired".into())));

    let report = outbox.replay(&mut transport).unwrap();
    let stall = report.stalled.unwrap();
    assert_eq!(stall.seq, 1);
    assert_eq!(stall.fault.code(), "control_plane_unavailable");
    assert_eq!(outbox.pending().count(), 3);
}

#[test]
fn a_torn_last_record_is_ignored_and_reported_and_earlier_records_survive() {
    let root = TempDir::new().unwrap();
    drop(three_records(&root));
    let path = journal_path(root.path(), "demo").unwrap();
    let full = fs::read(&path).unwrap();
    fs::write(&path, &full[..full.len() - 5]).unwrap();

    let mut outbox = Outbox::open(root.path(), "demo").unwrap();
    let damage = outbox.damage().unwrap().clone();
    assert_eq!(damage.reason, "torn_record");
    assert_eq!(damage.offset + damage.bytes, (full.len() - 5) as u64);
    assert_eq!(outbox.pending().map(|e| e.seq).collect::<Vec<_>>(), [1, 2]);

    // The next record takes the lost sequence number and is visible after reopening,
    // which proves the damage was moved aside rather than left in front of it.
    assert_eq!(
        outbox
            .enqueue(Kind::Append, "c", &op("op-4"), b"four")
            .unwrap(),
        3
    );
    drop(outbox);
    let reopened = Outbox::open(root.path(), "demo").unwrap();
    assert!(reopened.damage().is_none());
    assert_eq!(
        reopened.pending().map(|e| e.seq).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    let kept: Vec<_> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.contains("damaged"))
        .collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    let saved = fs::read(path.parent().unwrap().join(&kept[0])).unwrap();
    assert_eq!(saved, &full[damage.offset as usize..full.len() - 5]);
}

#[test]
fn a_checksum_failure_stops_the_scan_and_reports_the_tail() {
    let root = TempDir::new().unwrap();
    drop(three_records(&root));
    let path = journal_path(root.path(), "demo").unwrap();
    let mut bytes = fs::read(&path).unwrap();
    // Flip a byte inside the second record's body.
    let first_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let second_body = 8 + 36 + first_len + 36;
    bytes[second_body + 3] ^= 0xff;
    fs::write(&path, &bytes).unwrap();

    let outbox = Outbox::open(root.path(), "demo").unwrap();
    assert_eq!(outbox.damage().unwrap().reason, "checksum_mismatch");
    assert_eq!(outbox.pending().map(|e| e.seq).collect::<Vec<_>>(), [1]);
}

#[test]
fn a_file_that_is_not_a_journal_is_refused() {
    let root = TempDir::new().unwrap();
    let path = journal_path(root.path(), "demo").unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"not an outbox at all").unwrap();
    assert!(Outbox::open(root.path(), "demo").is_err());
    assert_eq!(fs::read(&path).unwrap(), b"not an outbox at all");
}

#[test]
fn names_that_could_escape_the_outbox_directory_are_refused() {
    let root = TempDir::new().unwrap();
    for name in ["", "../x", "a/b", ".hidden", "x y"] {
        assert!(Outbox::open(root.path(), name).is_err(), "{name:?}");
    }
}

#[test]
fn empty_scope_and_oversized_payload_are_refused_before_writing() {
    let root = TempDir::new().unwrap();
    let mut outbox = Outbox::open(root.path(), "demo").unwrap();
    assert!(outbox.enqueue(Kind::Upload, "", &op("x"), b"p").is_err());
    let huge = vec![0; 33 << 20];
    assert!(outbox.enqueue(Kind::Upload, "a", &op("x"), &huge).is_err());
    assert_eq!(
        outbox.enqueue(Kind::Upload, "a", &op("x"), b"p").unwrap(),
        1
    );
}

#[cfg(unix)]
#[test]
fn an_existing_journal_opens_under_an_ancestor_without_read_permission() {
    use std::os::unix::fs::PermissionsExt;

    let root = TempDir::new().unwrap();
    let repo = root.path().join("search_only/repo");
    let mut outbox = Outbox::open(&repo, "demo").unwrap();
    outbox
        .enqueue(Kind::Upload, "a", &op("op-1"), b"one")
        .unwrap();
    drop(outbox);

    // Search (execute) permission is enough to reach the journal; opening the ancestor
    // for reading is not needed once the journal exists.
    let search_only = root.path().join("search_only");
    fs::set_permissions(&search_only, fs::Permissions::from_mode(0o111)).unwrap();
    let reopened = Outbox::open(&repo, "demo");
    fs::set_permissions(&search_only, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(reopened.unwrap().pending().count(), 1);
}

#[test]
fn backoff_grows_then_caps_and_never_waits_nothing() {
    assert_eq!(backoff(0, 0.0), Duration::from_millis(125));
    assert_eq!(backoff(0, 1.0), Duration::from_millis(250));
    assert!(backoff(1, 0.5) > backoff(0, 0.5));
    assert_eq!(backoff(30, 1.0), Duration::from_secs(60));
    assert!(backoff(12, 0.0) >= Duration::from_secs(30));
}
