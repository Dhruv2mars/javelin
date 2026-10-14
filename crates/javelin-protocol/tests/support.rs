#![allow(dead_code)]

use std::io::Cursor;

use javelin::{FileEntry, Manifest, ObjectId, RepoPath, Repository};
use javelin_protocol::{Message, ProtocolError, codec, serve};
use tempfile::TempDir;

pub fn repo() -> (TempDir, Repository) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    (dir, repo)
}

/// A small source tree with a regular file, an executable and a symlink.
pub fn fixture(repo: &mut Repository) -> (ObjectId, Vec<(ObjectId, Vec<u8>)>, Manifest) {
    let files: [(&str, &[u8], u8); 3] = [
        ("bin/tool", b"#!/bin/sh\necho tool\n", 1),
        ("link", b"src/main.rs", 2),
        ("src/main.rs", &[0, 255, 1, 2, 3], 0),
    ];
    let mut blobs = Vec::new();
    let mut entries = Vec::new();
    for (path, bytes, kind) in files {
        let id = repo.put_blob(bytes).unwrap();
        blobs.push((id, bytes.to_vec()));
        let entry = match kind {
            2 => FileEntry::Symlink { target: id },
            k => FileEntry::Regular {
                content: id,
                executable: k == 1,
            },
        };
        entries.push((RepoPath::parse(path).unwrap(), entry));
    }
    let manifest = Manifest::new(entries).unwrap();
    let id = repo.put_manifest(&manifest).unwrap();
    (id, blobs, manifest)
}

pub fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = (body.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(body);
    out
}

pub fn hello() -> Vec<u8> {
    frame(&codec::encode(&Message::Hello { min: 1, max: 1 }))
}

/// Runs one receiver session over raw bytes and decodes every reply frame.
pub fn session(repo: &mut Repository, input: &[u8]) -> (Result<(), ProtocolError>, Vec<Message>) {
    let mut output = Vec::new();
    let result = serve(repo, Cursor::new(input), &mut output);
    let mut replies = Vec::new();
    let mut cursor = Cursor::new(output);
    while let Some(reply) = codec::read_frame(&mut cursor).unwrap() {
        replies.push(reply);
    }
    (result, replies)
}

/// Asserts a session ended with the given refusal, reported to the peer.
pub fn refused(repo: &mut Repository, input: &[u8], code: &str) {
    let before = repo.history().unwrap();
    let (result, replies) = session(repo, input);
    assert_eq!(result.unwrap_err().code, code);
    match replies.last() {
        Some(Message::Error { code: sent, .. }) => assert_eq!(sent, code),
        other => panic!("expected error reply, got {other:?}"),
    }
    assert_eq!(
        repo.history().unwrap(),
        before,
        "refusal must not advance World"
    );
}
