//! Loopback HTTP/1.1 transport for protocol version 2. One message per request:
//! `POST /v2/r/{name}`, with the message body (tag first, no length prefix) and
//! `Content-Length`. The response body is one message, with status 200 whether the
//! message is an outcome or an `Error`. HTTP status codes only report transport faults:
//! 400 bad framing, 404 wrong path, 405 wrong method, 411 no length, 413 too large.
//! A request that is not received within its deadline is dropped without a reply, and a
//! reply that cannot be sent within its deadline is abandoned.
//!
//! The server has no authentication yet, so it binds loopback addresses only.

use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
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
/// Most header lines before the body. A blank line must follow them.
const MAX_HEADERS: usize = 64;
/// Time to receive one whole request, counted from accept. A peer that trickles bytes
/// cannot hold the node longer than this.
const REQUEST_DEADLINE: Duration = Duration::from_secs(5);
/// Time to send one reply.
const RESPONSE_DEADLINE: Duration = Duration::from_secs(5);

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
/// until the listener fails when that is `None`. Each connection is bounded by
/// `REQUEST_DEADLINE` and `RESPONSE_DEADLINE`, so a stalled peer delays the next one by
/// at most that long. A connection that fails mid-request is dropped; the peer's retry
/// of the same operation is the recovery.
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

/// A request that gets a status reply, or an I/O failure that ends the connection
/// without one.
enum Fault {
    Status(u16),
    Io(io::Error),
}

impl From<io::Error> for Fault {
    fn from(error: io::Error) -> Self {
        Fault::Io(error)
    }
}

fn handle_connection(stream: TcpStream, name: &str, log: &mut FileWorldLog) -> io::Result<()> {
    // Nonblocking, so every read and write returns to `Deadline`, which checks the clock.
    // Socket timeouts are not used: they do not bound a call that keeps making progress.
    stream.set_nonblocking(true)?;
    let replies = stream.try_clone()?;
    let mut reader = BufReader::new(Deadline::new(stream, REQUEST_DEADLINE));
    match read_request(&mut reader, name, log) {
        Ok(body) => respond(&mut Deadline::new(replies, RESPONSE_DEADLINE), 200, &body),
        Err(Fault::Status(status)) => {
            let mut writer = Deadline::new(replies, RESPONSE_DEADLINE);
            respond(&mut writer, status, &[])?;
            // Read what the peer is still sending, so closing does not reset the reply.
            writer.stream.shutdown(Shutdown::Write)?;
            let _ = io::copy(&mut (&mut reader).take(MAX_FRAME as u64), &mut io::sink());
            Ok(())
        }
        Err(Fault::Io(error)) => Err(error),
    }
}

/// Reads one request and returns the encoded reply, or the status to reply with.
fn read_request(
    reader: &mut impl BufRead,
    name: &str,
    log: &mut FileWorldLog,
) -> Result<Vec<u8>, Fault> {
    let request = read_line(reader)?;
    let mut parts = request.split(' ');
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let mut length = None;
    let mut headers = 0;
    loop {
        let line = read_line(reader)?;
        if line.is_empty() {
            break;
        }
        headers += 1;
        if headers > MAX_HEADERS {
            return Err(Fault::Status(400));
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(Fault::Status(400));
        };
        if key.eq_ignore_ascii_case("transfer-encoding") {
            return Err(Fault::Status(400));
        }
        if key.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    if method != "POST" {
        return Err(Fault::Status(405));
    }
    if path != format!("/v2/r/{name}") {
        return Err(Fault::Status(404));
    }
    let Some(length) = length else {
        return Err(Fault::Status(411));
    };
    if length > MAX_FRAME {
        return Err(Fault::Status(413));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let reply = match decode(&body).and_then(|message| serve_one(log, message)) {
        Ok(reply) => reply,
        Err(ProtocolError { code, detail }) => Message::Error { code, detail },
    };
    Ok(match encode(&reply) {
        Ok(bytes) => bytes,
        Err(ProtocolError { code, detail }) => encode(&Message::Error { code, detail })
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.detail))?,
    })
}

/// One message through the trace guard, then the protocol handler.
fn serve_one(log: &mut FileWorldLog, message: Message) -> Result<Message, ProtocolError> {
    log.traces().refuse_wire_fetch(&message)?;
    match message {
        Message::LayerStack {
            operation,
            target,
            target_head,
            source,
        } => Ok(Message::LayerOutcome(crate::stack::stack(
            log.parts().0,
            &operation,
            &target,
            target_head,
            source,
        )?)),
        Message::Get(items) if crate::stack::all_checkpoints(&items) => {
            crate::stack::get_checkpoints(log.repo(), items)
        }
        message => handle_request(log, message),
    }
}

/// One line without its terminator, bounded by `MAX_LINE`. An overlong or non-UTF-8
/// line is a framing error and gets 400.
fn read_line(reader: &mut impl BufRead) -> Result<String, Fault> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err(Fault::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "request ended early",
            )));
        }
        let (done, used) = match available.iter().position(|&b| b == b'\n') {
            Some(at) => (true, at + 1),
            None => (false, available.len()),
        };
        line.extend_from_slice(&available[..used]);
        reader.consume(used);
        if line.len() > MAX_LINE {
            return Err(Fault::Status(400));
        }
        if done {
            break;
        }
    }
    let text = String::from_utf8(line).map_err(|_| Fault::Status(400))?;
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

/// A stream whose reads, or writes, stop at one instant. Each call waits only for what is
/// left of the deadline, so trickled bytes or a slow reader cannot extend it.
struct Deadline {
    stream: TcpStream,
    until: Instant,
}

impl Deadline {
    fn new(stream: TcpStream, limit: Duration) -> Self {
        Self {
            stream,
            until: Instant::now() + limit,
        }
    }

    fn left(&self) -> io::Result<Duration> {
        self.until
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "deadline passed"))
    }

    /// Waits one poll interval, or fails once the deadline has passed.
    fn wait(&self) -> io::Result<()> {
        self.left()?;
        thread::sleep(POLL.min(self.left()?));
        Ok(())
    }
}

/// How often a blocked read or write re-checks the deadline.
const POLL: Duration = Duration::from_millis(5);

impl Read for Deadline {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.stream.read(buf) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => self.wait()?,
                done => return done,
            }
        }
    }
}

impl Write for Deadline {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            match self.stream.write(buf) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => self.wait()?,
                done => return done,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
