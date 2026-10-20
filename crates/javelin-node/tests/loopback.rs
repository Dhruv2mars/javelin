use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    thread,
};

use javelin::{
    FileEntry, LayerId, ObjectId, OperationId, PublishAttribution, RepoPath, SparseChanges,
    VersionNumber,
};
use javelin_node::{FileWorldLog, bind_loopback, serve};
use javelin_protocol::{
    Message,
    codec::{PublishOutcome, decode, encode},
    receiver::WorldLog,
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

/// A node with Layer `work` over World 1, holding one checkpoint. Returns its head.
fn seeded(dir: &TempDir) -> (FileWorldLog, ObjectId) {
    let mut log = FileWorldLog::open(dir.path()).unwrap();
    log.create_layer(&layer("work"), v(0)).unwrap();
    let content = log.repository().put_blob(b"one").unwrap();
    let changes = SparseChanges::new([(
        RepoPath::parse("a.txt").unwrap(),
        Some(FileEntry::Regular {
            content,
            executable: false,
        }),
    )])
    .unwrap();
    let head = log
        .append(&op("write-1"), &layer("work"), &changes)
        .unwrap();
    (log, head.checkpoint().object())
}

fn publish_request(expected: ObjectId) -> Vec<u8> {
    encode(&Message::PublishLayer {
        operation: op("publish-1"),
        layer: layer("work"),
        expected,
        base: v(0),
        attribution: PublishAttribution::Human,
    })
    .unwrap()
}

/// One request on its own connection. Returns the status and the body.
fn exchange(addr: SocketAddr, request: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = std::str::from_utf8(&reply[..split]).unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, reply[split + 4..].to_vec())
}

#[test]
fn publish_layer_over_loopback_replays_the_recorded_outcome() {
    let dir = TempDir::new().unwrap();
    let (log, expected) = seeded(&dir);
    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut log = log;
        serve(&listener, "demo", &mut log, Some(2)).unwrap();
    });

    let body = publish_request(expected);
    let request = format!(
        "POST /v2/r/demo HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let first = exchange(addr, &request, &body);
    let second = exchange(addr, &request, &body);
    server.join().unwrap();

    assert_eq!(first.0, 200);
    assert_eq!(second.0, 200);
    let outcome = decode(&first.1).unwrap();
    assert!(matches!(
        outcome,
        Message::PublishOutcome(PublishOutcome::Published { number, .. }) if number == v(1)
    ));
    assert_eq!(decode(&second.1).unwrap(), outcome);
}

#[test]
fn transport_faults_have_http_statuses() {
    let dir = TempDir::new().unwrap();
    let (log, _) = seeded(&dir);
    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut log = log;
        serve(&listener, "demo", &mut log, Some(5)).unwrap();
    });

    assert_eq!(
        exchange(
            addr,
            "POST /v2/r/other HTTP/1.1\r\nContent-Length: 0\r\n\r\n",
            &[]
        )
        .0,
        404
    );
    assert_eq!(
        exchange(addr, "GET /v2/r/demo HTTP/1.1\r\n\r\n", &[]).0,
        405
    );
    assert_eq!(
        exchange(addr, "POST /v2/r/demo HTTP/1.1\r\n\r\n", &[]).0,
        411
    );
    assert_eq!(
        exchange(
            addr,
            "POST /v2/r/demo HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
            &[]
        )
        .0,
        400
    );
    let (status, reply) = exchange(
        addr,
        "POST /v2/r/demo HTTP/1.1\r\nContent-Length: 3\r\n\r\n",
        &[0xff, 0xff, 0xff],
    );
    assert_eq!(status, 200);
    assert!(matches!(decode(&reply).unwrap(), Message::Error { .. }));
    server.join().unwrap();
}

#[test]
fn non_loopback_addresses_are_refused() {
    assert!(bind_loopback("0.0.0.0:0").is_err());
    assert!(bind_loopback("not-an-address").is_err());
    assert!(bind_loopback("127.0.0.1:0").is_ok());
}
