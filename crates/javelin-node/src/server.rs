//! Loopback HTTP/1.1 transport for protocol version 2. One message per request:
//! `POST /v2/r/{name}`, with the message body (tag first, no length prefix) and
//! `Content-Length`. The response body is one message, with status 200 whether the
//! message is an outcome or an `Error`. HTTP status codes only report transport faults:
//! 400 bad framing, 404 wrong path, 405 wrong method, 411 no length, 413 too large.
//! A request that is not received within its deadline is dropped without a reply, and a
//! reply that cannot be sent within its deadline is abandoned.
//!
//! Every message passes the node's gate (`auth.rs`) with the request's
//! `Authorization: Bearer` token, or as an anonymous caller without one, so an anonymous
//! caller can only read a public repository. Refusals are `Error` replies with status 200.
//! The transport has no TLS, so the server binds loopback addresses only.
//!
//! Browsers may read: `OPTIONS` answers a CORS preflight, and replies to the read
//! messages (`Resolve`, `Get`, `Changeset`, `Attribution`) allow any origin. Any other
//! message that arrives with an `Origin` header is refused with `forbidden` before it
//! is served, so a web page can never write through a visitor's browser.

use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};

use javelin::LayerState;
use javelin_identity::{Need, Scope};
use javelin_protocol::{
    Message, ProtocolError,
    codec::{code, decode, encode},
    handle_request_as,
    limits::MAX_FRAME,
};

use crate::{
    FileWorldLog,
    auth::{self, Gate},
    checks, review,
};

/// Longest request line or header line, in bytes.
const MAX_LINE: usize = 8 << 10;
/// Most header lines before the body. A blank line must follow them.
const MAX_HEADERS: usize = 64;
/// Time to receive one whole request, counted from accept. A peer that trickles bytes
/// cannot hold the node longer than this.
const REQUEST_DEADLINE: Duration = Duration::from_secs(5);
/// Time to send one reply.
const RESPONSE_DEADLINE: Duration = Duration::from_secs(5);
/// Time to wait, after the reply and our FIN, for the peer to close. Closing while the peer's
/// bytes sit unread makes the OS reset the connection, which can destroy the reply in flight.
const LINGER: Duration = Duration::from_secs(1);

/// Binds `addr`, which must be a loopback address.
pub fn bind_loopback(addr: &str) -> io::Result<TcpListener> {
    let parsed: SocketAddr = addr
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "address must be IP:port"))?;
    if !is_loopback(parsed.ip()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the node refuses non-loopback addresses until its transport has TLS",
        ));
    }
    TcpListener::bind(parsed)
}

fn is_loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

/// Serves `name` from `log`, one connection at a time, which makes every request
/// ordered at the single coordinator. `gate` decides every message. Stops after
/// `connections` connections, or runs until the listener fails when that is `None`. Each
/// connection is bounded by `REQUEST_DEADLINE`, `RESPONSE_DEADLINE` and `LINGER`, so a
/// stalled peer delays the next one by at most that long. A connection that fails
/// mid-request is dropped and reported on stderr; the peer's retry of the same operation is
/// the recovery. A peer that gives up while queued fails only its own accept, so the
/// listener, and every connection queued behind it, stays up.
pub fn serve(
    listener: &TcpListener,
    name: &str,
    log: &mut FileWorldLog,
    gate: &Gate,
    connections: Option<usize>,
) -> io::Result<()> {
    let mut served = 0;
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) if transient(&error) => {
                eprintln!("javelin-node: accept failed: {error}");
                continue;
            }
            Err(error) => return Err(error),
        };
        if let Err(error) = handle_connection(stream, name, log, gate) {
            eprintln!("javelin-node: connection dropped: {error}");
        }
        served += 1;
        if connections == Some(served) {
            break;
        }
    }
    Ok(())
}

/// An accept failure that belongs to one queued connection, not to the listener.
fn transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::Interrupted
    )
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

fn handle_connection(
    stream: TcpStream,
    name: &str,
    log: &mut FileWorldLog,
    gate: &Gate,
) -> io::Result<()> {
    // Nonblocking, so every read and write returns to `Deadline`, which checks the clock.
    // Socket timeouts are not used: they do not bound a call that keeps making progress.
    // One handle reads and writes: a cloned handle is a second socket descriptor on Windows.
    stream.set_nonblocking(true)?;
    let mut reader = BufReader::new(Deadline::new(stream, REQUEST_DEADLINE));
    let (status, extra, body) = match read_request(&mut reader, name, log, gate) {
        Ok(Reply::Message { body, cors }) => (200, if cors { ALLOW_ORIGIN } else { "" }, body),
        Ok(Reply::Preflight) => (204, PREFLIGHT, Vec::new()),
        Err(Fault::Status(status)) => (status, "", Vec::new()),
        Err(Fault::Io(error)) => return Err(error),
    };
    let connection = reader.get_mut();
    connection.restart(RESPONSE_DEADLINE);
    respond(connection, status, extra, &body)?;
    // Close gracefully: our FIN goes after the reply, then whatever the peer still sends is
    // read until it closes, so closing never discards unread bytes, which would reset the
    // connection and could destroy the reply before the peer reads it.
    connection.stream.shutdown(Shutdown::Write)?;
    connection.restart(LINGER);
    let _ = io::copy(&mut reader.take(MAX_FRAME as u64), &mut io::sink());
    Ok(())
}

/// A reply to a well-formed request: one encoded message, which a browser may read
/// when `cors` is set, or the answer to a CORS preflight.
enum Reply {
    Message { body: Vec<u8>, cors: bool },
    Preflight,
}

const ALLOW_ORIGIN: &str = "Access-Control-Allow-Origin: *\r\n";
const PREFLIGHT: &str = "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: POST\r\nAccess-Control-Allow-Headers: content-type\r\nAccess-Control-Max-Age: 600\r\n";

/// Messages that only read the repository, which a browser may send.
fn is_read(message: &Message) -> bool {
    matches!(
        message,
        Message::Resolve { .. }
            | Message::Get(_)
            | Message::Changeset { .. }
            | Message::Attribution { .. }
    )
}

/// Reads one request and returns the reply, or the status to reply with.
fn read_request(
    reader: &mut impl BufRead,
    name: &str,
    log: &mut FileWorldLog,
    gate: &Gate,
) -> Result<Reply, Fault> {
    let request = read_line(reader)?;
    let mut parts = request.split(' ');
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let mut length = None;
    let mut token = None;
    let mut origin = false;
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
        if key.eq_ignore_ascii_case("authorization") {
            // Only the Bearer scheme carries a token. Two credentials are a framing error.
            let value = value.trim();
            let bearer = value
                .split_once(' ')
                .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
                .map(|(_, token)| token.trim().to_string())
                .filter(|token| !token.is_empty())
                .ok_or(Fault::Status(400))?;
            if token.replace(bearer).is_some() {
                return Err(Fault::Status(400));
            }
        }
        if key.eq_ignore_ascii_case("origin") {
            origin = true;
        }
    }
    if method != "POST" && method != "OPTIONS" {
        return Err(Fault::Status(405));
    }
    if path != format!("/v2/r/{name}") {
        return Err(Fault::Status(404));
    }
    if method == "OPTIONS" {
        return Ok(Reply::Preflight);
    }
    let Some(length) = length else {
        return Err(Fault::Status(411));
    };
    if length > MAX_FRAME {
        return Err(Fault::Status(413));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let mut cors = false;
    let reply = match decode(&body).and_then(|message| {
        cors = is_read(&message);
        // Refused before the gate or the handler sees it: a page in a visitor's browser
        // never writes, whatever credential the browser holds.
        if origin && !cors {
            return Err(ProtocolError::new(
                code::FORBIDDEN,
                "a browser origin may only send read messages",
            ));
        }
        serve_one(log, gate, token.as_deref(), now, message)
    }) {
        Ok(reply) => reply,
        Err(ProtocolError { code, detail }) => Message::Error { code, detail },
    };
    let body = match encode(&reply) {
        Ok(bytes) => bytes,
        Err(ProtocolError { code, detail }) => encode(&Message::Error { code, detail })
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.detail))?,
    };
    Ok(Reply::Message { body, cors })
}

/// One message through the gate and the trace guard, then its handler. The review
/// partition and check reports authorize inside their handlers, because what they need
/// depends on stored state, such as the Layer a request belongs to.
fn serve_one(
    log: &mut FileWorldLog,
    gate: &Gate,
    token: Option<&str>,
    now: u64,
    message: Message,
) -> Result<Message, ProtocolError> {
    match &message {
        Message::OpenRequest { .. }
        | Message::Revise { .. }
        | Message::Approve { .. }
        | Message::Comment { .. }
        | Message::PublishRequest { .. }
        | Message::ReadRequest { .. } => return review::handle(log, gate, token, now, &message),
        Message::CheckReport(report) => {
            return checks::report(log, gate, token, now, report).map(Message::CheckOutcome);
        }
        _ => {}
    }
    // Version negotiation reveals nothing about the repository, so it needs no grant.
    let grant = match &message {
        Message::Hello { .. } => None,
        other => Some(gate.check(token, now, need(other))?),
    };
    // Hidden entries follow the caller's role: anonymous is an outsider.
    let reader = grant
        .as_ref()
        .map_or_else(javelin::Reader::outsider, |grant| gate.reader(grant));
    if grant.is_some() {
        check_writes(log, &reader, &message)?;
    }
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
            crate::stack::get_checkpoints(log.repo(), items, &reader)
        }
        message => handle_request_as(log, message, &reader),
    }
}

/// Refuses a write that would change a path outside the caller's audiences, before it
/// reaches its handler: an append's staged changes over the Layer's base, the source of a
/// Stack over the target's base, and the whole Layer for a Publish.
fn check_writes(
    log: &mut FileWorldLog,
    writer: &javelin::Reader,
    message: &Message,
) -> Result<(), ProtocolError> {
    match message {
        Message::LayerAppend {
            layer, checkpoint, ..
        } => {
            let Some(changes) = crate::layer::load_changes(log.records(), *checkpoint)? else {
                return Ok(());
            };
            let Ok(LayerState::Active { head }) = log.repo().layer_state(layer) else {
                return Ok(());
            };
            let base = log
                .repo()
                .read_checkpoint(head.checkpoint())?
                .base()
                .version();
            auth::check_write(log.repo(), writer, &changes, base)
        }
        Message::LayerStack {
            target_head,
            source,
            ..
        } => {
            let base = javelin::CheckpointId::parse(&target_head.to_string())
                .ok()
                .and_then(|id| log.repo().read_checkpoint(id).ok())
                .map(|target| target.base().version());
            auth::check_checkpoint_write(log.repo(), writer, *source, base)
        }
        Message::PublishLayer { expected, .. } => {
            auth::check_checkpoint_write(log.repo(), writer, *expected, None)
        }
        _ => Ok(()),
    }
}

/// What one message needs from the caller's token. Messages that name a Layer carry it, so a
/// Layer-limited token reaches only its own Layer there. Object uploads (`Want`, `Objects`)
/// name none: they need `layer:write`, and change no reference, since objects are addressed
/// by content. Everything else, replies sent as requests included, needs `read`.
fn need(message: &Message) -> Need<'_> {
    let (scope, layer) = match message {
        Message::LayerAppend { layer, .. } => (Scope::LayerWrite, Some(layer)),
        Message::LayerStack { target, .. } => (Scope::LayerWrite, Some(target)),
        Message::PublishLayer { layer, .. } => (Scope::Publish, Some(layer)),
        Message::Publish { .. } => (Scope::Publish, None),
        Message::Want(_) | Message::Objects(_) => (Scope::LayerWrite, None),
        Message::LayerHead { layer } => (Scope::Read, Some(layer)),
        _ => (Scope::Read, None),
    };
    Need {
        scope,
        layer: layer.map(|layer| layer.as_str()),
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

/// Writes one response. `extra` is zero or more whole header lines.
fn respond(writer: &mut impl Write, status: u16, extra: &str, body: &[u8]) -> io::Result<()> {
    let text = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        _ => "Payload Too Large",
    };
    write!(
        writer,
        "HTTP/1.1 {status} {text}\r\nContent-Type: application/octet-stream\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
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

    /// Starts a new deadline, `limit` from now, for the next phase of the connection.
    fn restart(&mut self, limit: Duration) {
        self.until = Instant::now() + limit;
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
