//! Version 1 wire format: `u32` big-endian body length, then a body whose first
//! byte is the message tag. Integers are big-endian; IDs are raw 32-byte values.

use std::io::{self, Read, Write};

use javelin::{FileEntry, Manifest, ObjectId, OperationId, RepoPath, VersionNumber};

use crate::{ProtocolError, limits::*};

pub const MAGIC: &[u8; 4] = b"JVLP";
pub const VERSION: u16 = 1;

const HELLO: u8 = 0x01;
const HELLO_OK: u8 = 0x02;
const WANT: u8 = 0x10;
const MISSING: u8 = 0x11;
const BLOBS: u8 = 0x20;
const MANIFESTS: u8 = 0x21;
const STORED: u8 = 0x22;
const PUBLISH: u8 = 0x30;
const OUTCOME: u8 = 0x31;
const ERROR: u8 = 0x7f;

/// Object IDs are kind-separated; every reference names the expected kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Kind {
    Blob = 1,
    Manifest = 2,
}

/// Wire DTO for the native `AdvanceOutcome`. Built only from validated wire values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    Accepted {
        number: VersionNumber,
        manifest: ObjectId,
        operation: OperationId,
    },
    Stale {
        actual: VersionNumber,
    },
}

impl Outcome {
    /// Adapts a native outcome. Accepted native versions always carry their operation.
    pub fn from_native(outcome: &javelin::AdvanceOutcome) -> Result<Self, ProtocolError> {
        match outcome {
            javelin::AdvanceOutcome::Accepted { version } => Ok(Self::Accepted {
                number: version.number,
                manifest: version.manifest,
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
    Want(Vec<(Kind, ObjectId)>),
    Missing(Vec<(Kind, ObjectId)>),
    Blobs(Vec<(ObjectId, Vec<u8>)>),
    Manifests(Vec<(ObjectId, Manifest)>),
    Stored {
        count: u32,
    },
    Publish {
        operation: OperationId,
        expected: VersionNumber,
        manifest: ObjectId,
    },
    Outcome(Outcome),
    Error {
        code: String,
        detail: String,
    },
}

pub fn encode(message: &Message) -> Vec<u8> {
    let mut out = Vec::new();
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
        Message::Want(items) | Message::Missing(items) => {
            out.push(if matches!(message, Message::Want(_)) {
                WANT
            } else {
                MISSING
            });
            put_u32(&mut out, items.len());
            for (kind, id) in items {
                out.push(*kind as u8);
                put_id(&mut out, id);
            }
        }
        Message::Blobs(blobs) => {
            out.push(BLOBS);
            put_u32(&mut out, blobs.len());
            for (id, bytes) in blobs {
                put_id(&mut out, id);
                put_u32(&mut out, bytes.len());
                out.extend_from_slice(bytes);
            }
        }
        Message::Manifests(manifests) => {
            out.push(MANIFESTS);
            put_u32(&mut out, manifests.len());
            for (id, manifest) in manifests {
                put_id(&mut out, id);
                put_u32(&mut out, manifest.entries().len());
                for (path, entry) in manifest.entries() {
                    let path = path.as_str().as_bytes();
                    out.extend_from_slice(&(path.len() as u16).to_be_bytes());
                    out.extend_from_slice(path);
                    let (tag, content) = match entry {
                        FileEntry::Regular {
                            content,
                            executable: false,
                        } => (0, content),
                        FileEntry::Regular {
                            content,
                            executable: true,
                        } => (1, content),
                        FileEntry::Symlink { target } => (2, target),
                    };
                    out.push(tag);
                    put_id(&mut out, content);
                }
            }
        }
        Message::Stored { count } => {
            out.push(STORED);
            out.extend_from_slice(&count.to_be_bytes());
        }
        Message::Publish {
            operation,
            expected,
            manifest,
        } => {
            out.push(PUBLISH);
            put_short(&mut out, operation.as_str());
            out.extend_from_slice(&expected.get().to_be_bytes());
            put_id(&mut out, manifest);
        }
        Message::Outcome(Outcome::Accepted {
            number,
            manifest,
            operation,
        }) => {
            out.extend_from_slice(&[OUTCOME, 0]);
            out.extend_from_slice(&number.get().to_be_bytes());
            put_id(&mut out, manifest);
            put_short(&mut out, operation.as_str());
        }
        Message::Outcome(Outcome::Stale { actual }) => {
            out.extend_from_slice(&[OUTCOME, 1]);
            out.extend_from_slice(&actual.get().to_be_bytes());
        }
        Message::Error { code, detail } => {
            out.push(ERROR);
            put_short(&mut out, code);
            let detail = truncate_utf8(detail, MAX_DETAIL);
            out.extend_from_slice(&(detail.len() as u16).to_be_bytes());
            out.extend_from_slice(detail.as_bytes());
        }
    }
    out
}

/// Decodes one complete frame body. Every count and length is checked against
/// both its fixed limit and the remaining bytes before anything is allocated.
pub fn decode(body: &[u8]) -> Result<Message, ProtocolError> {
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
        tag @ (WANT | MISSING) => {
            let n = r.count(MAX_WANT, 33)?;
            let mut items = Vec::with_capacity(n);
            for _ in 0..n {
                let kind = match r.u8()? {
                    1 => Kind::Blob,
                    2 => Kind::Manifest,
                    _ => return Err(malformed("unknown object kind")),
                };
                items.push((kind, r.id()?));
            }
            if tag == WANT {
                Message::Want(items)
            } else {
                Message::Missing(items)
            }
        }
        BLOBS => {
            let n = r.count(MAX_BLOBS, 36)?;
            let mut blobs = Vec::with_capacity(n);
            for _ in 0..n {
                let id = r.id()?;
                let len = r.u32()? as usize;
                if len > MAX_BLOB {
                    return Err(limit("blob exceeds size limit"));
                }
                blobs.push((id, r.take(len)?.to_vec()));
            }
            Message::Blobs(blobs)
        }
        MANIFESTS => {
            let n = r.count(MAX_MANIFESTS, 36)?;
            let mut manifests = Vec::with_capacity(n);
            for _ in 0..n {
                let id = r.id()?;
                manifests.push((id, r.manifest()?));
            }
            Message::Manifests(manifests)
        }
        STORED => Message::Stored { count: r.u32()? },
        PUBLISH => Message::Publish {
            operation: r.operation()?,
            expected: r.version()?,
            manifest: r.id()?,
        },
        OUTCOME => match r.u8()? {
            0 => Message::Outcome(Outcome::Accepted {
                number: r.version()?,
                manifest: r.id()?,
                operation: r.operation()?,
            }),
            1 => Message::Outcome(Outcome::Stale {
                actual: r.version()?,
            }),
            _ => return Err(malformed("unknown outcome")),
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

/// Writes one frame. Refuses to emit a body the peer would reject for size.
pub fn write_frame(w: &mut impl Write, message: &Message) -> Result<(), ProtocolError> {
    let body = encode(message);
    if body.len() > MAX_FRAME {
        return Err(limit("frame exceeds size limit"));
    }
    // Never emit a body the peer would refuse, including lengths that exceed a field.
    decode(&body)?;
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
    /// Raw IDs are parsed through the public hex parser; no private constructor exists.
    fn id(&mut self) -> Result<ObjectId, ProtocolError> {
        let hex: String = self.take(32)?.iter().map(|b| format!("{b:02x}")).collect();
        Ok(ObjectId::parse(&hex)?)
    }
    /// Checks a count against its limit and against the minimum encoded element size.
    fn count(&mut self, max: usize, min_size: usize) -> Result<usize, ProtocolError> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(limit("count exceeds limit"));
        }
        if n.saturating_mul(min_size) > self.0.len() {
            return Err(malformed("count exceeds message length"));
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
    fn manifest(&mut self) -> Result<Manifest, ProtocolError> {
        let n = self.count(MAX_ENTRIES, 35)?;
        let mut entries = Vec::with_capacity(n);
        let mut previous: Option<&[u8]> = None;
        for _ in 0..n {
            let len = self.u16()? as usize;
            if len > MAX_PATH {
                return Err(limit("path exceeds length limit"));
            }
            let raw = self.take(len)?;
            if raw.iter().filter(|&&b| b == b'/').count() >= MAX_DEPTH {
                return Err(limit("path exceeds depth limit"));
            }
            // Strict byte order matches the native manifest's sorted map and rejects duplicates.
            if previous.is_some_and(|p| p >= raw) {
                return Err(malformed("manifest paths are not strictly ascending"));
            }
            previous = Some(raw);
            let path = std::str::from_utf8(raw).map_err(|_| malformed("path is not UTF-8"))?;
            let path = RepoPath::parse(path)?;
            let tag = self.u8()?;
            let id = self.id()?;
            let entry = match tag {
                0 | 1 => FileEntry::Regular {
                    content: id,
                    executable: tag == 1,
                },
                2 => FileEntry::Symlink { target: id },
                _ => return Err(malformed("unknown file entry kind")),
            };
            entries.push((path, entry));
        }
        Ok(Manifest::new(entries)?)
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
    let hex = id.to_string();
    out.extend((0..32).map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap()));
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
