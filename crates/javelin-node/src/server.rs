//! Loopback HTTP/1.1 transport for protocol version 2. One message per request:
//! `POST /v2/r/{name}`, with the message body (tag first, no length prefix) and
//! `Content-Length`. The response body is one message, with status 200 whether the
//! message is an outcome or an `Error`. HTTP status codes only report transport faults:
//! 400 bad framing, 404 wrong path, 405 wrong method, 411 no length, 413 too large.
//!
//! The server has no authentication yet, so it binds loopback addresses only.

use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{IpAddr, SocketAddr, TcpListener, TcpStream},
};

use javelin_protocol::{
    Message, ProtocolError,
    codec::{decode, encode},
    handle_request,
    limits::MAX_FRAME,
};

use crate::FileWorldLog;

/// Longest request line or header line, in bytes.
const MAX_LINE: usize = 8 << 10;
/// Most header lines before the body.
const MAX_HEADERS: usize = 64;

/// Binds `addr`, which must be a loopback address.
pub fn bind_loopback(addr: &str) -> io::Result<TcpListener> {
    let parsed: SocketAddr = addr
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "address must be IP:port"))?;
    if !is_loopback(parsed.ip()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the node refuses non-loopback addresses until authentication exists",
        ));
    }
    TcpListener::bind(parsed)
}

fn is_loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

/// Serves `name` from `log`, one connection at a time, which makes every request
/// ordered at the single coordinator. Stops after `connections` connections, or runs
/// until the listener fails when that is `None`. A connection that fails mid-request
/// is dropped; the peer's retry of the same operation is the recovery.
pub fn serve(
    listener: &TcpListener,
    name: &str,
    log: &mut FileWorldLog,
    connections: Option<usize>,
) -> io::Result<()> {
    let mut served = 0;
    for stream in listener.incoming() {
        let _ = handle_connection(stream?, name, log);
        served += 1;
        if connections == Some(served) {
            break;
        }
    }
    Ok(())
}

fn handle_connection(stream: TcpStream, name: &str, log: &mut FileWorldLog) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let request = read_line(&mut reader)?;
    let mut parts = request.split(' ');
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let mut length = None;
    for _ in 0..MAX_HEADERS {
        let line = read_line(&mut reader)?;
        if line.is_empty() {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            return respond(&mut writer, 400, &[]);
        };
        if key.eq_ignore_ascii_case("transfer-encoding") {
            return respond(&mut writer, 400, &[]);
        }
        if key.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    if method != "POST" {
        return respond(&mut writer, 405, &[]);
    }
    if path != format!("/v2/r/{name}") {
        return respond(&mut writer, 404, &[]);
    }
    let Some(length) = length else {
        return respond(&mut writer, 411, &[]);
    };
    if length > MAX_FRAME {
        return respond(&mut writer, 413, &[]);
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let reply = match decode(&body).and_then(|message| handle_request(log, message)) {
        Ok(reply) => reply,
        Err(ProtocolError { code, detail }) => Message::Error { code, detail },
    };
    let bytes = match encode(&reply) {
        Ok(bytes) => bytes,
        Err(ProtocolError { code, detail }) => encode(&Message::Error { code, detail })
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.detail))?,
    };
    respond(&mut writer, 200, &bytes)
}

/// One line without its terminator, bounded by `MAX_LINE`.
fn read_line(reader: &mut impl BufRead) -> io::Result<String> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "request ended early",
            ));
        }
        let (done, used) = match available.iter().position(|&b| b == b'\n') {
            Some(at) => (true, at + 1),
            None => (false, available.len()),
        };
        line.extend_from_slice(&available[..used]);
        reader.consume(used);
        if line.len() > MAX_LINE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request line too long",
            ));
        }
        if done {
            break;
        }
    }
    let text = String::from_utf8(line)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "request is not UTF-8"))?;
    Ok(text.trim_end_matches(['\r', '\n']).to_string())
}

fn respond(writer: &mut impl Write, status: u16, body: &[u8]) -> io::Result<()> {
    let text = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        _ => "Payload Too Large",
    };
    write!(
        writer,
        "HTTP/1.1 {status} {text}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    writer.write_all(body)?;
    writer.flush()
}
