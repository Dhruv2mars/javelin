//! Version 2 wire format: `u32` big-endian body length, then a body whose first
//! byte is the message tag. Integers are big-endian; IDs are raw 32-byte values.
//! Snapshots travel as content-addressed tree chunks and blobs, root first.

use std::io::{self, Read, Write};

use javelin::{ObjectId, OperationId, VersionNumber, tree::Chunk};

use crate::{ProtocolError, limits::*};

pub const MAGIC: &[u8; 4] = b"JVLP";
pub const VERSION: u16 = 2;

const HELLO: u8 = 0x01;
const HELLO_OK: u8 = 0x02;
const WANT: u8 = 0x10;
const MISSING: u8 = 0x11;
const GET: u8 = 0x12;
const OBJECTS: u8 = 0x20;
const STORED: u8 = 0x22;
const PUBLISH: u8 = 0x30;
const OUTCOME: u8 = 0x31;
const RESOLVE: u8 = 0x40;
const RESOLVED: u8 = 0x41;
const ERROR: u8 = 0x7f;

/// Object IDs are kind-separated; every reference names the expected kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Kind {
    Blob = 1,
    Tree = 4,
}

impl Kind {
    /// Largest encoded object of this kind.
    pub fn limit(self) -> usize {
        match self {
            Self::Blob => MAX_BLOB,
            Self::Tree => MAX_TREE,
        }
    }
    /// The object ID of `bytes` as this kind.
    pub fn id(self, bytes: &[u8]) -> ObjectId {
        match self {
            Self::Blob => ObjectId::for_blob(bytes),
            Self::Tree => ObjectId::for_tree(bytes),
        }
    }
}

/// Wire DTO for the native `AdvanceOutcome`. Built only from validated wire values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    Accepted {
        number: VersionNumber,
        tree: ObjectId,
        operation: OperationId,
    },
    Stale {
        actual: VersionNumber,
    },
}

impl Outcome {
    /// Adapts a native outcome of advancing to `tree`. Accepted native versions
    /// always carry their operation and root.
    pub fn from_native(outcome: &javelin::AdvanceOutcome) -> Result<Self, ProtocolError> {
        match outcome {
            javelin::AdvanceOutcome::Accepted { version } => Ok(Self::Accepted {
                number: version.number,
                tree: version.tree,
                operation: version.operation.clone().ok_or_else(|| {
                    ProtocolError::new("invalid_input", "accepted World lacks operation")
                })?,
            }),
            javelin::AdvanceOutcome::Stale { actual } => Ok(Self::Stale { actual: *actual }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    Hello {
        min: u16,
        max: u16,
    },
    HelloOk {
        version: u16,
    },
    /// Push: which of these does the receiver lack?
    Want(Vec<(Kind, ObjectId)>),
    Missing(Vec<(Kind, ObjectId)>),
    /// Fetch: send these objects.
    Get(Vec<(Kind, ObjectId)>),
    /// Object bytes, each bounded by its kind's limit. Push sends children before
    /// parents; a fetch reply answers a prefix of its `Get`.
    Objects(Vec<(Kind, ObjectId, Vec<u8>)>),
    Stored {
        count: u32,
    },
    Publish {
        operation: OperationId,
        expected: VersionNumber,
        tree: ObjectId,
    },
    Outcome(Outcome),
    /// Fetch: the version to read, or the head.
    Resolve {
        version: Option<VersionNumber>,
    },
    Resolved {
        number: VersionNumber,
        id: ObjectId,
        tree: ObjectId,
    },
    Error {
        code: String,
        detail: String,
    },
}

/// Encodes one body after checking every count, length and the total against the
/// version 2 limits, so an oversize message is refused before its buffer exists.
pub fn encode(message: &Message) -> Result<Vec<u8>, ProtocolError> {
    let mut out = Vec::with_capacity(encoded_len(message)?);
    match message {
        Message::Hello { min, max } => {
            out.push(HELLO);
            out.extend_from_slice(MAGIC);
            out.extend_from_slice(&min.to_be_bytes());
            out.extend_from_slice(&max.to_be_bytes());
        }
        Message::HelloOk { version } => {
            out.push(HELLO_OK);
            out.extend_from_slice(&version.to_be_bytes());
        }
        Message::Want(items) | Message::Missing(items) | Message::Get(items) => {
            out.push(match message {
                Message::Want(_) => WANT,
                Message::Missing(_) => MISSING,
                _ => GET,
            });
            put_u32(&mut out, items.len());
            for (kind, id) in items {
                out.push(*kind as u8);
                put_id(&mut out, id);
            }
        }
        Message::Objects(objects) => {
            out.push(OBJECTS);
            put_u32(&mut out, objects.len());
            for (kind, id, bytes) in objects {
                out.push(*kind as u8);
                put_id(&mut out, id);
                put_u32(&mut out, bytes.len());
                out.extend_from_slice(bytes);
            }
        }
        Message::Stored { count } => {
            out.push(STORED);
            out.extend_from_slice(&count.to_be_bytes());
        }
        Message::Publish {
            operation,
            expected,
            tree,
        } => {
            out.push(PUBLISH);
            put_short(&mut out, operation.as_str());
            out.extend_from_slice(&expected.get().to_be_bytes());
            put_id(&mut out, tree);
        }
        Message::Outcome(Outcome::Accepted {
            number,
            tree,
            operation,
        }) => {
            out.extend_from_slice(&[OUTCOME, 0]);
            out.extend_from_slice(&number.get().to_be_bytes());
            put_id(&mut out, tree);
            put_short(&mut out, operation.as_str());
        }
        Message::Outcome(Outcome::Stale { actual }) => {
            out.extend_from_slice(&[OUTCOME, 1]);
            out.extend_from_slice(&actual.get().to_be_bytes());
        }
        Message::Resolve { version } => {
            out.push(RESOLVE);
            out.push(u8::from(version.is_some()));
            out.extend_from_slice(&version.map_or(0, VersionNumber::get).to_be_bytes());
        }
        Message::Resolved { number, id, tree } => {
            out.push(RESOLVED);
            out.extend_from_slice(&number.get().to_be_bytes());
            put_id(&mut out, id);
            put_id(&mut out, tree);
        }
        Message::Error { code, detail } => {
            out.push(ERROR);
            put_short(&mut out, code);
            let detail = truncate_utf8(detail, MAX_DETAIL);
            out.extend_from_slice(&(detail.len() as u16).to_be_bytes());
            out.extend_from_slice(detail.as_bytes());
        }
    }
    Ok(out)
}

fn encoded_len(message: &Message) -> Result<usize, ProtocolError> {
    let len = match message {
        Message::Hello { .. } => 9,
        Message::HelloOk { .. } => 3,
        Message::Want(items) | Message::Missing(items) | Message::Get(items) => {
            if items.len() > MAX_WANT {
                return Err(limit("count exceeds limit"));
            }
            5 + items.len() * 33
        }
        Message::Objects(objects) => {
            if objects.len() > MAX_OBJECTS {
                return Err(limit("count exceeds limit"));
            }
            let mut len = 5usize;
            for (kind, _, bytes) in objects {
                if bytes.len() > kind.limit() {
                    return Err(limit("object exceeds size limit"));
                }
                len = len.saturating_add(OBJECT_OVERHEAD + bytes.len());
            }
            len
        }
        Message::Stored { .. } => 5,
        Message::Publish { operation, .. } => 42 + operation.as_str().len(),
        Message::Outcome(Outcome::Accepted { operation, .. }) => 43 + operation.as_str().len(),
        Message::Outcome(Outcome::Stale { .. }) => 10,
        Message::Resolve { .. } => 10,
        Message::Resolved { .. } => 73,
        Message::Error { code, .. } => {
            if code.len() > u8::MAX as usize {
                return Err(malformed("error code too long"));
            }
            4 + code.len() + MAX_DETAIL
        }
    };
    if len > MAX_FRAME {
        return Err(limit("frame exceeds size limit"));
    }
    Ok(len)
}

/// Encoded bytes of one `Objects` item besides its payload: kind, ID and length.
pub const OBJECT_OVERHEAD: usize = 37;

/// Decodes one complete frame body. Every count and length is checked against its
/// fixed limit, and every field against the remaining bytes, before it is copied.
/// Tree chunks must be canonically encoded.
pub fn decode(body: &[u8]) -> Result<Message, ProtocolError> {
    if body.len() > MAX_FRAME {
        return Err(limit("frame exceeds size limit"));
    }
    let mut r = Reader(body);
    let message = match r.u8()? {
        HELLO => {
            if r.take(4)? != MAGIC {
                return Err(malformed("bad protocol magic"));
            }
            let (min, max) = (r.u16()?, r.u16()?);
            if min > max {
                return Err(malformed("empty version range"));
            }
            Message::Hello { min, max }
        }
        HELLO_OK => Message::HelloOk { version: r.u16()? },
        tag @ (WANT | MISSING | GET) => {
            let n = r.count(MAX_WANT)?;
            let mut items = Vec::new();
            for _ in 0..n {
                items.push((r.kind()?, r.id()?));
            }
            match tag {
                WANT => Message::Want(items),
                MISSING => Message::Missing(items),
                _ => Message::Get(items),
            }
        }
        OBJECTS => {
            let n = r.count(MAX_OBJECTS)?;
            let mut objects = Vec::new();
            for _ in 0..n {
                let kind = r.kind()?;
                let id = r.id()?;
                let len = r.u32()? as usize;
                if len > kind.limit() {
                    return Err(limit("object exceeds size limit"));
                }
                let bytes = r.take(len)?;
                if kind == Kind::Tree {
                    Chunk::decode(bytes).map_err(|e| malformed(&e.to_string()))?;
                }
                objects.push((kind, id, bytes.to_vec()));
            }
            Message::Objects(objects)
        }
        STORED => Message::Stored { count: r.u32()? },
        PUBLISH => Message::Publish {
            operation: r.operation()?,
            expected: r.version()?,
            tree: r.id()?,
        },
        OUTCOME => match r.u8()? {
            0 => Message::Outcome(Outcome::Accepted {
                number: r.version()?,
                tree: r.id()?,
                operation: r.operation()?,
            }),
            1 => Message::Outcome(Outcome::Stale {
                actual: r.version()?,
            }),
            _ => return Err(malformed("unknown outcome")),
        },
        RESOLVE => {
            let flag = r.u8()?;
            let version = r.version()?;
            Message::Resolve {
                version: match flag {
                    0 if version.get() == 0 => None,
                    1 => Some(version),
                    _ => return Err(malformed("invalid version selector")),
                },
            }
        }
        RESOLVED => Message::Resolved {
            number: r.version()?,
            id: r.id()?,
            tree: r.id()?,
        },
        ERROR => {
            let code = r.short_str()?.to_owned();
            let len = r.u16()? as usize;
            if len > MAX_DETAIL {
                return Err(limit("error detail exceeds limit"));
            }
            let detail = std::str::from_utf8(r.take(len)?)
                .map_err(|_| malformed("error detail is not UTF-8"))?
                .to_owned();
            Message::Error { code, detail }
        }
        _ => return Err(malformed("unknown message tag")),
    };
    if !r.0.is_empty() {
        return Err(malformed("trailing bytes after message"));
    }
    Ok(message)
}

/// Reads one frame. `Ok(None)` is a clean end of stream at a frame boundary.
pub fn read_frame(r: &mut impl Read) -> Result<Option<Message>, ProtocolError> {
    let mut header = [0; 4];
    let mut filled = 0;
    while filled < 4 {
        match r.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(truncated()),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let len = u32::from_be_bytes(header) as usize;
    if len == 0 {
        return Err(malformed("empty frame"));
    }
    if len > MAX_FRAME {
        return Err(limit("frame exceeds size limit"));
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            truncated()
        } else {
            e.into()
        }
    })?;
    decode(&body).map(Some)
}

/// Writes one frame. Oversize messages are refused before encoding.
pub fn write_frame(w: &mut impl Write, message: &Message) -> Result<(), ProtocolError> {
    let body = encode(message)?;
    w.write_all(&(body.len() as u32).to_be_bytes())?;
    w.write_all(&body)?;
    w.flush()?;
    Ok(())
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        if n > self.0.len() {
            return Err(malformed("message ends inside a field"));
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, ProtocolError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn version(&mut self) -> Result<VersionNumber, ProtocolError> {
        let value = u64::from_be_bytes(self.take(8)?.try_into().unwrap());
        Ok(VersionNumber::new(value)?)
    }
    fn id(&mut self) -> Result<ObjectId, ProtocolError> {
        Ok(ObjectId::from_bytes(self.take(32)?.try_into().unwrap()))
    }
    fn kind(&mut self) -> Result<Kind, ProtocolError> {
        match self.u8()? {
            1 => Ok(Kind::Blob),
            4 => Ok(Kind::Tree),
            _ => Err(malformed("unknown object kind")),
        }
    }
    /// Checks a count against its fixed limit. Collections grow only as elements are
    /// actually decoded, so memory stays proportional to the received frame.
    fn count(&mut self, max: usize) -> Result<usize, ProtocolError> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(limit("count exceeds limit"));
        }
        Ok(n)
    }
    fn short_str(&mut self) -> Result<&'a str, ProtocolError> {
        let len = self.u8()? as usize;
        std::str::from_utf8(self.take(len)?).map_err(|_| malformed("text is not UTF-8"))
    }
    fn operation(&mut self) -> Result<OperationId, ProtocolError> {
        Ok(OperationId::parse(self.short_str()?)?)
    }
}

fn put_u32(out: &mut Vec<u8>, n: usize) {
    out.extend_from_slice(&(n as u32).to_be_bytes());
}

fn put_short(out: &mut Vec<u8>, text: &str) {
    let text = truncate_utf8(text, u8::MAX as usize);
    out.push(text.len() as u8);
    out.extend_from_slice(text.as_bytes());
}

fn put_id(out: &mut Vec<u8>, id: &ObjectId) {
    out.extend_from_slice(id.as_bytes());
}

fn truncate_utf8(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn malformed(detail: &str) -> ProtocolError {
    ProtocolError::new("malformed", detail)
}

fn limit(detail: &str) -> ProtocolError {
    ProtocolError::new("limit_exceeded", detail)
}

fn truncated() -> ProtocolError {
    ProtocolError::new("truncated", "stream ended inside a frame")
}
