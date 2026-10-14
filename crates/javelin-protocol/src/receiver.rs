//! Reference receiver. Objects are stored only through the public native API and
//! accepted only when the returned ID equals the claimed ID. World references are
//! written only by `compare_and_advance`, which revalidates the whole graph.

use std::io::{Read, Write};

use javelin::Repository;

use crate::{
    Kind, Message, Outcome, ProtocolError,
    codec::{VERSION, read_frame, write_frame},
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
            let mut missing = Vec::new();
            for (kind, id) in items {
                let found = match kind {
                    Kind::Blob => repo.read_blob(id).map(drop),
                    Kind::Manifest => repo.manifest_bytes(id).map(drop),
                };
                match found {
                    Ok(()) => {}
                    Err(javelin::Error::MissingObject(missing_id)) if missing_id == id => {
                        missing.push((kind, id))
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(Message::Missing(missing))
        }
        Message::Blobs(blobs) => {
            for (claimed, bytes) in &blobs {
                let stored = repo.put_blob(bytes)?;
                if stored != *claimed {
                    return Err(mismatch(*claimed, stored));
                }
            }
            Ok(Message::Stored {
                count: blobs.len() as u32,
            })
        }
        Message::Manifests(manifests) => {
            for (claimed, manifest) in &manifests {
                // put_manifest refuses missing or wrong-kind blob references.
                let stored = repo.put_manifest(manifest)?;
                if stored != *claimed {
                    return Err(mismatch(*claimed, stored));
                }
            }
            Ok(Message::Stored {
                count: manifests.len() as u32,
            })
        }
        Message::Publish {
            operation,
            expected,
            manifest,
        } => {
            let outcome = repo.compare_and_advance(&operation, expected, manifest)?;
            Ok(Message::Outcome(Outcome::from_native(&outcome)?))
        }
        _ => Err(unexpected()),
    }
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
