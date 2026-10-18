//! Reference receiver. Objects are stored only through the public native API and
//! accepted only when their computed ID equals the claimed ID. A tree chunk is
//! stored only after everything it names, so having a tree means having all of it.
//! World references are written only by `advance_to_tree`, which verifies the
//! changed part of the candidate tree. Fetch reads any stored object by ID.

use std::collections::BTreeSet;
use std::io::{Read, Write};

use javelin::Repository;

use crate::{
    Kind, Message, Outcome, ProtocolError,
    codec::{OBJECT_OVERHEAD, VERSION, read_frame, write_frame},
    limits::*,
};

/// Serves one session. Any refusal is reported to the peer and ends the session.
pub fn serve(
    repo: &mut Repository,
    mut r: impl Read,
    mut w: impl Write,
) -> Result<(), ProtocolError> {
    let mut negotiated = false;
    loop {
        let result = match read_frame(&mut r) {
            Ok(None) => return Ok(()),
            Ok(Some(message)) if !negotiated => negotiate(message).inspect(|_| negotiated = true),
            Ok(Some(message)) => handle(repo, message),
            Err(e) => Err(e),
        };
        match result {
            Ok(reply) => write_frame(&mut w, &reply)?,
            Err(e) => {
                // Best effort: the peer may already be gone.
                let _ = write_frame(
                    &mut w,
                    &Message::Error {
                        code: e.code.clone(),
                        detail: e.detail.clone(),
                    },
                );
                return Err(e);
            }
        }
    }
}

fn negotiate(message: Message) -> Result<Message, ProtocolError> {
    match message {
        Message::Hello { min, max } if (min..=max).contains(&VERSION) => {
            Ok(Message::HelloOk { version: VERSION })
        }
        Message::Hello { .. } => Err(ProtocolError::new(
            "unsupported_version",
            format!("supported versions: {VERSION}..={VERSION}"),
        )),
        _ => Err(unexpected()),
    }
}

fn handle(repo: &mut Repository, message: Message) -> Result<Message, ProtocolError> {
    match message {
        Message::Want(items) => {
            // Streamed probes still hash stored bytes, so repeats amplify work.
            unique(&items)?;
            let mut missing = Vec::new();
            for (kind, id) in items {
                let found = match kind {
                    Kind::Blob => repo.has_blob(id),
                    Kind::Tree => repo.has_tree(id),
                }?;
                if !found {
                    missing.push((kind, id));
                }
            }
            Ok(Message::Missing(missing))
        }
        Message::Objects(objects) => {
            for (kind, claimed, bytes) in &objects {
                let actual = kind.id(bytes);
                if actual != *claimed {
                    return Err(mismatch(*claimed, actual));
                }
                match kind {
                    Kind::Blob => repo.put_blob(bytes)?,
                    Kind::Tree => repo.put_tree(bytes)?,
                };
            }
            Ok(Message::Stored {
                count: objects.len() as u32,
            })
        }
        Message::Publish {
            operation,
            expected,
            tree,
        } => {
            let outcome = repo.advance_to_tree(&operation, expected, tree)?;
            Ok(Message::Outcome(Outcome::from_native(&outcome)?))
        }
        Message::Resolve { version } => {
            let world = match version {
                Some(number) => repo.version(number)?,
                None => repo.head()?,
            };
            Ok(Message::Resolved {
                number: world.number,
                id: world.id,
                tree: world.tree,
            })
        }
        // The reply is the longest prefix of the request that fits one frame.
        Message::Get(items) => {
            unique(&items)?;
            let mut objects = Vec::new();
            let mut size = 5;
            for (kind, id) in items.into_iter().take(MAX_OBJECTS) {
                let bytes = match kind {
                    Kind::Blob => repo.read_blob_bounded(id, MAX_BLOB)?,
                    Kind::Tree => repo.tree_bytes_bounded(id, MAX_TREE)?,
                };
                size += OBJECT_OVERHEAD + bytes.len();
                if !objects.is_empty() && size > MAX_FRAME {
                    break;
                }
                objects.push((kind, id, bytes));
            }
            Ok(Message::Objects(objects))
        }
        _ => Err(unexpected()),
    }
}

fn unique(items: &[(Kind, javelin::ObjectId)]) -> Result<(), ProtocolError> {
    if items.iter().collect::<BTreeSet<_>>().len() != items.len() {
        return Err(ProtocolError::new("malformed", "duplicate object request"));
    }
    Ok(())
}

fn mismatch(claimed: javelin::ObjectId, stored: javelin::ObjectId) -> ProtocolError {
    ProtocolError::new(
        "id_mismatch",
        format!("claimed {claimed} but content has ID {stored}"),
    )
}

fn unexpected() -> ProtocolError {
    ProtocolError::new(
        "unexpected_message",
        "message not valid in this session state",
    )
}
