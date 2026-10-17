//! Reference sender: negotiates, transfers only what the receiver lacks (blobs
//! before the manifest), then publishes with a caller-chosen operation ID.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};

use javelin::{FileEntry, Manifest, ObjectId, OperationId, RepoPath, Repository, VersionNumber};

use crate::{
    Kind, Message, Outcome, ProtocolError,
    codec::{VERSION, encode, read_frame, write_frame},
    limits::*,
};

/// Retrying after an uncertain failure must reuse the same operation, expected
/// version and manifest; the receiver then returns the original durable outcome.
pub fn push(
    mut r: impl Read,
    mut w: impl Write,
    source: &Repository,
    manifest_id: ObjectId,
    operation: &OperationId,
    expected: VersionNumber,
) -> Result<Outcome, ProtocolError> {
    let mut call = |message: Message| exchange(&mut r, &mut w, &message);
    match call(Message::Hello {
        min: VERSION,
        max: VERSION,
    })? {
        Message::HelloOk { version } if version == VERSION => {}
        _ => return Err(unexpected()),
    }

    // The native capped read verifies canonical bytes, not the referenced graph.
    // Reconstruct through public domain parsers; publication remains authoritative.
    let bytes = source.manifest_bytes_bounded(manifest_id, MAX_FRAME)?;
    let entries: BTreeMap<String, FileEntry> = serde_json::from_slice(&bytes)
        .map_err(|e| ProtocolError::new("corrupt_object", e.to_string()))?;
    let manifest = Manifest::new(
        entries
            .into_iter()
            .map(|(path, entry)| Ok((RepoPath::parse(&path)?, entry)))
            .collect::<javelin::Result<Vec<_>>>()?,
    )?;
    // Native paths and manifests can exceed wire limits. Validate the complete
    // message with the codec before negotiating or transferring any objects.
    let blobs: BTreeSet<ObjectId> = manifest.entries().values().map(|e| e.content()).collect();
    let manifest_message = Message::Manifests(vec![(manifest_id, manifest)]);
    encode(&manifest_message)?;
    let mut offered: Vec<(Kind, ObjectId)> = vec![(Kind::Manifest, manifest_id)];
    offered.extend(blobs.into_iter().map(|id| (Kind::Blob, id)));

    let mut missing = Vec::new();
    for chunk in offered.chunks(MAX_WANT) {
        let chunk_set: BTreeSet<_> = chunk.iter().collect();
        match call(Message::Want(chunk.to_vec()))? {
            // Only offered objects, each at most once: a receiver cannot amplify transfer.
            Message::Missing(items)
                if items.iter().all(|item| chunk_set.contains(item))
                    && items.iter().collect::<BTreeSet<_>>().len() == items.len() =>
            {
                missing.extend(items)
            }
            _ => return Err(unexpected()),
        }
    }

    let mut batch: Vec<(ObjectId, Vec<u8>)> = Vec::new();
    let mut size = 0;
    for &(kind, id) in &missing {
        if kind != Kind::Blob {
            continue;
        }
        let bytes = source.read_blob_bounded(id, MAX_BLOB)?;
        let cost = 36 + bytes.len();
        if !batch.is_empty() && (batch.len() == MAX_BLOBS || size + cost > MAX_FRAME - 5) {
            send_batch(&mut call, Message::Blobs(std::mem::take(&mut batch)))?;
            size = 0;
        }
        size += cost;
        batch.push((id, bytes));
    }
    if !batch.is_empty() {
        send_batch(&mut call, Message::Blobs(batch))?;
    }
    if missing.contains(&(Kind::Manifest, manifest_id)) {
        send_batch(&mut call, manifest_message)?;
    }

    match call(Message::Publish {
        operation: operation.clone(),
        expected,
        manifest: manifest_id,
    })? {
        Message::Outcome(outcome) if consistent(&outcome, operation, expected, manifest_id) => {
            Ok(outcome)
        }
        _ => Err(unexpected()),
    }
}

/// An outcome must answer this exact Publish: acceptance is the next version with the
/// requested manifest and operation; staleness reports a head beyond `expected`.
fn consistent(
    outcome: &Outcome,
    operation: &OperationId,
    expected: VersionNumber,
    manifest_id: ObjectId,
) -> bool {
    match outcome {
        Outcome::Accepted {
            number,
            manifest,
            operation: accepted,
        } => {
            Some(number.get()) == expected.get().checked_add(1)
                && *manifest == manifest_id
                && accepted == operation
        }
        Outcome::Stale { actual } => *actual > expected,
    }
}

fn send_batch(
    call: &mut impl FnMut(Message) -> Result<Message, ProtocolError>,
    message: Message,
) -> Result<(), ProtocolError> {
    let sent = match &message {
        Message::Blobs(items) => items.len(),
        Message::Manifests(items) => items.len(),
        _ => unreachable!("only object batches are sent here"),
    };
    match call(message)? {
        Message::Stored { count } if count as usize == sent => Ok(()),
        _ => Err(unexpected()),
    }
}

fn exchange(
    r: &mut impl Read,
    w: &mut impl Write,
    message: &Message,
) -> Result<Message, ProtocolError> {
    write_frame(w, message)?;
    match read_frame(r)? {
        None => Err(ProtocolError::new(
            "truncated",
            "receiver closed the session before replying",
        )),
        Some(Message::Error { code, detail }) => Err(ProtocolError { code, detail }),
        Some(reply) => Ok(reply),
    }
}

fn unexpected() -> ProtocolError {
    ProtocolError::new("unexpected_message", "unexpected reply from receiver")
}
