//! Wire primitives. `Reader` decodes and bounds every length before allocating; the
//! `put_*` helpers encode. Both are private to the codec.

use javelin::{
    LayerId, MAX_PATH, ObjectId, OperationId, PublishAttribution, RepoPath, VersionNumber,
};

use super::code;
use crate::{Kind, ProtocolError};

use super::hosting::{LayerStatus, RequestId, RequestStatus, Role, Visibility};

pub(super) struct Reader<'a>(pub(super) &'a [u8]);

impl<'a> Reader<'a> {
    pub(super) fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        if n > self.0.len() {
            return Err(malformed("message ends inside a field"));
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }
    pub(super) fn u8(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.take(1)?[0])
    }
    pub(super) fn u16(&mut self) -> Result<u16, ProtocolError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub(super) fn u32(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub(super) fn u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub(super) fn version(&mut self) -> Result<VersionNumber, ProtocolError> {
        let value = self.u64()?;
        Ok(VersionNumber::new(value)?)
    }
    pub(super) fn id(&mut self) -> Result<ObjectId, ProtocolError> {
        Ok(ObjectId::from_bytes(self.take(32)?.try_into().unwrap()))
    }
    pub(super) fn kind(&mut self) -> Result<Kind, ProtocolError> {
        match self.u8()? {
            1 => Ok(Kind::Blob),
            3 => Ok(Kind::Checkpoint),
            4 => Ok(Kind::Tree),
            _ => Err(malformed("unknown object kind")),
        }
    }
    /// Checks a count against its fixed limit. Collections grow only as elements are
    /// actually decoded, so memory stays proportional to the received frame.
    pub(super) fn count(&mut self, max: usize) -> Result<usize, ProtocolError> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(limit("count exceeds limit"));
        }
        Ok(n)
    }
    pub(super) fn short_str(&mut self) -> Result<&'a str, ProtocolError> {
        let len = self.u8()? as usize;
        std::str::from_utf8(self.take(len)?).map_err(|_| malformed("text is not UTF-8"))
    }
    pub(super) fn operation(&mut self) -> Result<OperationId, ProtocolError> {
        Ok(OperationId::parse(self.short_str()?)?)
    }
    pub(super) fn layer(&mut self) -> Result<LayerId, ProtocolError> {
        LayerId::parse(self.short_str()?).map_err(|_| malformed("invalid Layer ID"))
    }
    pub(super) fn request(&mut self) -> Result<RequestId, ProtocolError> {
        RequestId::parse(self.short_str()?).map_err(|_| malformed("invalid request ID"))
    }
    /// One canonical flag byte: 0 or 1, nothing else.
    pub(super) fn flag(&mut self) -> Result<bool, ProtocolError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(malformed("invalid flag")),
        }
    }
    pub(super) fn optional_id(&mut self) -> Result<Option<ObjectId>, ProtocolError> {
        let present = self.flag()?;
        let id = self.id()?;
        match present {
            true => Ok(Some(id)),
            false if id == ABSENT_ID => Ok(None),
            false => Err(malformed("absent ID must be zeroed")),
        }
    }
    pub(super) fn optional_u32(&mut self) -> Result<Option<u32>, ProtocolError> {
        let present = self.flag()?;
        let value = self.u32()?;
        match present {
            true => Ok(Some(value)),
            false if value == 0 => Ok(None),
            false => Err(malformed("absent number must be zero")),
        }
    }
    pub(super) fn role(&mut self) -> Result<Role, ProtocolError> {
        match self.u8()? {
            1 => Ok(Role::Read),
            2 => Ok(Role::Review),
            3 => Ok(Role::Maintain),
            _ => Err(malformed("unknown role")),
        }
    }
    pub(super) fn visibility(&mut self) -> Result<Visibility, ProtocolError> {
        match self.u8()? {
            0 => Ok(Visibility::Public),
            1 => Ok(Visibility::Private),
            _ => Err(malformed("unknown visibility")),
        }
    }
    pub(super) fn layer_status(&mut self) -> Result<LayerStatus, ProtocolError> {
        match self.u8()? {
            0 => Ok(LayerStatus::Active),
            1 => Ok(LayerStatus::Published),
            2 => Ok(LayerStatus::Discarded),
            _ => Err(malformed("unknown Layer state")),
        }
    }
    pub(super) fn request_status(&mut self) -> Result<RequestStatus, ProtocolError> {
        match self.u8()? {
            0 => Ok(RequestStatus::Open),
            1 => Ok(RequestStatus::Published),
            2 => Ok(RequestStatus::Closed),
            _ => Err(malformed("unknown request state")),
        }
    }
    pub(super) fn attribution(&mut self) -> Result<PublishAttribution, ProtocolError> {
        match self.u8()? {
            0 => Ok(PublishAttribution::Human),
            1 => Ok(PublishAttribution::Agent {
                context: self.optional_id()?,
                allow_missing_trace: self.flag()?,
            }),
            _ => Err(malformed("unknown attribution")),
        }
    }
    /// A repository path: `u16` length, then UTF-8 that parses as a `RepoPath`.
    pub(super) fn path(&mut self) -> Result<RepoPath, ProtocolError> {
        let len = self.u16()? as usize;
        if len > MAX_PATH {
            return Err(limit("path exceeds limit"));
        }
        let text =
            std::str::from_utf8(self.take(len)?).map_err(|_| malformed("path is not UTF-8"))?;
        RepoPath::parse(text).map_err(|_| malformed("invalid repository path"))
    }
    /// A comment body: `u32` length, bounded by `max`, then UTF-8.
    pub(super) fn text32(&mut self, max: usize) -> Result<String, ProtocolError> {
        let len = self.u32()? as usize;
        if len > max {
            return Err(limit("comment exceeds limit"));
        }
        std::str::from_utf8(self.take(len)?)
            .map(str::to_owned)
            .map_err(|_| malformed("comment is not UTF-8"))
    }
}

/// The all-zero ID that stands for absence in an optional ID field.
pub(super) const ABSENT_ID: ObjectId = ObjectId::from_bytes([0; 32]);

pub(super) fn put_u32(out: &mut Vec<u8>, n: usize) {
    out.extend_from_slice(&(n as u32).to_be_bytes());
}

/// One-byte length, then text. Callers check the length first with `short`.
pub(super) fn put_short(out: &mut Vec<u8>, text: &str) {
    out.push(text.len() as u8);
    out.extend_from_slice(text.as_bytes());
}

/// `u16` length, then text. Used for paths.
pub(super) fn put_long(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u16).to_be_bytes());
    out.extend_from_slice(text.as_bytes());
}

pub(super) fn put_id(out: &mut Vec<u8>, id: &ObjectId) {
    out.extend_from_slice(id.as_bytes());
}

pub(super) fn put_optional_id(out: &mut Vec<u8>, id: Option<&ObjectId>) {
    out.push(u8::from(id.is_some()));
    put_id(out, id.unwrap_or(&ABSENT_ID));
}

pub(super) fn put_optional_u32(out: &mut Vec<u8>, value: Option<u32>) {
    out.push(u8::from(value.is_some()));
    out.extend_from_slice(&value.unwrap_or(0).to_be_bytes());
}

pub(super) fn put_attribution(out: &mut Vec<u8>, attribution: &PublishAttribution) {
    match attribution {
        PublishAttribution::Human => out.push(0),
        PublishAttribution::Agent {
            context,
            allow_missing_trace,
        } => {
            out.push(1);
            put_optional_id(out, context.as_ref());
            out.push(u8::from(*allow_missing_trace));
        }
    }
}

/// Encoded size of a one-byte-length text field, refusing text that does not fit.
pub(super) fn short(text: &str) -> Result<usize, ProtocolError> {
    if text.len() > u8::MAX as usize {
        return Err(malformed("text exceeds 255 bytes"));
    }
    Ok(1 + text.len())
}

pub(super) fn truncate_utf8(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

pub(super) fn malformed(detail: &str) -> ProtocolError {
    ProtocolError::new(code::MALFORMED, detail)
}

pub(super) fn limit(detail: &str) -> ProtocolError {
    ProtocolError::new(code::LIMIT_EXCEEDED, detail)
}

pub(super) fn truncated() -> ProtocolError {
    ProtocolError::new(code::TRUNCATED, "stream ended inside a frame")
}
