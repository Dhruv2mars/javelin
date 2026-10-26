use std::{
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    thread,
    time::{Duration, Instant},
};

use javelin::{
    FileEntry, LayerId, ObjectId, OperationId, PublishAttribution, RepoPath, SparseChanges,
    VersionNumber,
};
use javelin_node::{FileWorldLog, bind_loopback, serve};
use javelin_protocol::{
    Message,
    codec::{Kind, PublishOutcome, decode, encode},
    limits::MAX_BLOB,
    receiver::WorldLog,
};
use tempfile::TempDir;

#[path = "wire_support/mod.rs"]
mod wire_support;

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

/// One request on its own connection, with the owner's token. Returns the status and the
/// body.
fn exchange(addr: SocketAddr, request: impl AsRef<[u8]>, body: &[u8]) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .write_all(&wire_support::authorized("demo", request.as_ref()))
        .unwrap();
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
        serve(
            &listener,
            "demo",
            &mut log,
            &wire_support::gate("demo"),
            Some(2),
        )
        .unwrap();
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
        serve(
            &listener,
            "demo",
            &mut log,
            &wire_support::gate("demo"),
            Some(5),
        )
        .unwrap();
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

/// The status line and headers of one exchange, and its body.
fn exchange_head(addr: SocketAddr, request: impl AsRef<[u8]>, body: &[u8]) -> (String, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(request.as_ref()).unwrap();
    stream.write_all(body).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    (
        String::from_utf8(reply[..split].to_vec()).unwrap(),
        reply[split + 4..].to_vec(),
    )
}

#[test]
fn browsers_may_read_but_never_write() {
    let dir = TempDir::new().unwrap();
    let (log, expected) = seeded(&dir);
    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut log = log;
        serve(&listener, "demo", &mut log, &wire_support::gate("demo"), Some(7)).unwrap();
    });
    let send = |origin: bool, token: bool, body: &[u8]| {
        let origin = if origin {
            "Origin: http://forge.test\r\n"
        } else {
            ""
        };
        let authorization = if token {
            format!("Authorization: Bearer {}\r\n", wire_support::token("demo"))
        } else {
            String::new()
        };
        exchange_head(
            addr,
            format!(
                "POST /v2/r/demo HTTP/1.1\r\n{origin}{authorization}Content-Length: {}\r\n\r\n",
                body.len()
            ),
            body,
        )
    };
    // A browser reads anonymously, as the public repository allows.
    let post = |origin: bool, body: &[u8]| send(origin, !origin, body);

    let (head, _) = exchange_head(
        addr,
        "OPTIONS /v2/r/demo HTTP/1.1\r\nOrigin: http://forge.test\r\nAccess-Control-Request-Method: POST\r\n\r\n",
        &[],
    );
    assert!(head.starts_with("HTTP/1.1 204"), "{head}");
    assert!(head.contains("Access-Control-Allow-Origin: *"));
    assert!(head.contains("Access-Control-Allow-Methods: POST"));
    assert!(head.contains("Access-Control-Allow-Headers: content-type"));

    // A write from a browser origin is refused before it is served, even with a valid token.
    let (head, reply) = post(true, &publish_request(expected));
    assert!(!head.contains("Access-Control-Allow-Origin"), "{head}");
    assert!(matches!(
        decode(&reply).unwrap(),
        Message::Error { code, .. } if code == "forbidden"
    ));
    let (_, reply) = send(true, true, &publish_request(expected));
    assert!(matches!(
        decode(&reply).unwrap(),
        Message::Error { code, .. } if code == "forbidden"
    ));
    let resolve = encode(&Message::Resolve { version: None }).unwrap();
    let (head, reply) = post(true, &resolve);
    assert!(head.contains("Access-Control-Allow-Origin: *"), "{head}");
    assert!(matches!(
        decode(&reply).unwrap(),
        Message::Resolved { number, .. } if number == v(0)
    ));

    // The same write without an origin, with the owner's token, is served, and a browser can
    // read who made it.
    let (head, reply) = post(false, &publish_request(expected));
    assert!(!head.contains("Access-Control-Allow-Origin"), "{head}");
    assert!(matches!(
        decode(&reply).unwrap(),
        Message::PublishOutcome(PublishOutcome::Published { number, .. }) if number == v(1)
    ));
    let attribution = encode(&Message::Attribution { version: v(1) }).unwrap();
    let (head, reply) = post(true, &attribution);
    assert!(head.contains("Access-Control-Allow-Origin: *"), "{head}");
    assert_eq!(
        decode(&reply).unwrap(),
        Message::Attributed {
            number: v(1),
            publication: Some((layer("work"), PublishAttribution::Human)),
        }
    );
    let genesis = encode(&Message::Attribution { version: v(0) }).unwrap();
    assert_eq!(
        decode(&post(true, &genesis).1).unwrap(),
        Message::Attributed {
            number: v(0),
            publication: None,
        }
    );
    server.join().unwrap();
}

#[test]
fn non_loopback_addresses_are_refused() {
    assert!(bind_loopback("0.0.0.0:0").is_err());
    assert!(bind_loopback("not-an-address").is_err());
    assert!(bind_loopback("127.0.0.1:0").is_ok());
}

#[test]
fn a_stalled_client_does_not_block_the_next_one() {
    let dir = TempDir::new().unwrap();
    let (log, expected) = seeded(&dir);
    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut log = log;
        serve(
            &listener,
            "demo",
            &mut log,
            &wire_support::gate("demo"),
            Some(2),
        )
        .unwrap();
    });

    // Sends one byte and then nothing, so the node waits on it until the deadline.
    let mut stalled = TcpStream::connect(addr).unwrap();
    stalled.write_all(b"P").unwrap();

    let body = publish_request(expected);
    let request = format!(
        "POST /v2/r/demo HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let started = Instant::now();
    let (status, reply) = exchange(addr, &request, &body);
    assert!(started.elapsed() < Duration::from_secs(30));
    drop(stalled);
    server.join().unwrap();

    assert_eq!(status, 200);
    assert!(matches!(
        decode(&reply).unwrap(),
        Message::PublishOutcome(PublishOutcome::Published { .. })
    ));
}

#[test]
fn the_header_limit_is_exact_and_needs_a_blank_line() {
    let dir = TempDir::new().unwrap();
    let (log, expected) = seeded(&dir);
    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut log = log;
        serve(
            &listener,
            "demo",
            &mut log,
            &wire_support::gate("demo"),
            Some(2),
        )
        .unwrap();
    });

    let body = publish_request(expected);
    let headers =
        |count: usize| -> String { (0..count).map(|i| format!("X-{i}: y\r\n")).collect() };
    let request = |count: usize| {
        format!(
            "POST /v2/r/demo HTTP/1.1\r\n{}Content-Length: {}\r\n\r\n",
            headers(count),
            body.len()
        )
    };
    // 64 header lines in all, Content-Length and Authorization included, is the most accepted.
    let (status, reply) = exchange(addr, request(62), &body);
    assert_eq!(status, 200);
    assert!(matches!(
        decode(&reply).unwrap(),
        Message::PublishOutcome(_)
    ));
    // One more line is a framing error.
    assert_eq!(exchange(addr, request(63), &body).0, 400);
    server.join().unwrap();
}

#[test]
fn a_slow_reader_cannot_hold_the_next_client_past_the_reply_deadline() {
    let dir = TempDir::new().unwrap();
    let (mut log, expected) = seeded(&dir);
    let id = log.repository().put_blob(&vec![0x5a; MAX_BLOB]).unwrap();
    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut log = log;
        serve(
            &listener,
            "demo",
            &mut log,
            &wire_support::gate("demo"),
            Some(2),
        )
        .unwrap();
    });

    let get = encode(&Message::Get(vec![(Kind::Blob, id)])).unwrap();
    let request = format!(
        "POST /v2/r/demo HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
        get.len()
    );
    // Reads 64 KiB a second. Each write makes progress and stalls for less than the
    // per-write timeout, so only a deadline over the whole reply can end it.
    let mut slow = TcpStream::connect(addr).unwrap();
    slow.write_all(request.as_bytes()).unwrap();
    slow.write_all(&get).unwrap();
    let stop = slow.try_clone().unwrap();
    let drip = thread::spawn(move || {
        let mut buf = vec![0; 64 << 10];
        loop {
            thread::sleep(Duration::from_secs(1));
            match slow.read(&mut buf) {
                Ok(n) if n > 0 => {}
                _ => break,
            }
        }
    });
    thread::sleep(Duration::from_millis(500));

    let body = publish_request(expected);
    let request = format!(
        "POST /v2/r/demo HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let started = Instant::now();
    let (status, reply) = exchange(addr, &request, &body);
    assert!(started.elapsed() < Duration::from_secs(15));
    // The node may already have closed its side, so the shutdown can report not connected.
    let _ = stop.shutdown(Shutdown::Both);
    drip.join().unwrap();
    server.join().unwrap();

    assert_eq!(status, 200);
    assert!(matches!(
        decode(&reply).unwrap(),
        Message::PublishOutcome(_)
    ));
}

#[test]
fn malformed_lines_get_a_bad_request() {
    let dir = TempDir::new().unwrap();
    let (log, _) = seeded(&dir);
    let listener = bind_loopback("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut log = log;
        serve(
            &listener,
            "demo",
            &mut log,
            &wire_support::gate("demo"),
            Some(2),
        )
        .unwrap();
    });

    let overlong = format!(
        "POST /v2/r/demo HTTP/1.1\r\nX: {}\r\n\r\n",
        "a".repeat(9000)
    );
    assert_eq!(exchange(addr, overlong, &[]).0, 400);
    assert_eq!(
        exchange(addr, b"POST /v2/r/demo HTTP/1.1\r\nX: \xff\r\n\r\n", &[]).0,
        400
    );
    server.join().unwrap();
}
