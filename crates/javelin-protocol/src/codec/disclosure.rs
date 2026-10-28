//! Disclosure messages (0x48-0x4c): the delay and embargoes that hold readers outside a
//! repository at its public frontier. The tags are dispatched in `codec.rs`; the field
//! layouts live here.

use javelin::VersionNumber;

use super::wire::*;
use crate::{ProtocolError, limits::MAX_WANT};

/// How a repository discloses its versions, as one caller may know it. A caller outside
/// the repository learns only whether the public view is delayed and where its frontier
/// is: `delay` is `None` and `embargoes` empty for it, whatever the repository holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Disclosure {
    /// Whether readers outside the repository see the World as it was some time ago.
    pub delayed: bool,
    /// The delay, in seconds, for a member. `None` for a caller outside the repository.
    pub delay: Option<u64>,
    /// The newest version a reader outside the repository sees.
    pub frontier: VersionNumber,
    /// Unreleased embargoes on published versions, oldest first, for a member.
    pub embargoes: Vec<Embargoed>,
}

/// One embargoed version, and the Unix time it is released at, or `None` until a
/// maintainer releases it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Embargoed {
    pub version: VersionNumber,
    pub until: Option<u64>,
}

/// Writes the body of a `Disclosure`, after its tag.
pub(super) fn put_disclosure(out: &mut Vec<u8>, disclosure: &Disclosure) {
    out.push(u8::from(disclosure.delayed));
    put_optional_u64(out, disclosure.delay);
    out.extend_from_slice(&disclosure.frontier.get().to_be_bytes());
    put_u32(out, disclosure.embargoes.len());
    for embargo in &disclosure.embargoes {
        out.extend_from_slice(&embargo.version.get().to_be_bytes());
        put_optional_u64(out, embargo.until);
    }
}

/// Encoded size of a `Disclosure`, tag included.
pub(super) fn disclosure_len(disclosure: &Disclosure) -> Result<usize, ProtocolError> {
    if disclosure.embargoes.len() > MAX_WANT {
        return Err(ProtocolError::new(
            super::code::LIMIT_EXCEEDED,
            "count exceeds limit",
        ));
    }
    Ok(1 + 1 + 9 + 8 + 4 + disclosure.embargoes.len() * 17)
}

pub(super) fn read_disclosure(r: &mut Reader<'_>) -> Result<Disclosure, ProtocolError> {
    let delayed = r.flag()?;
    let delay = read_optional_u64(r)?;
    let frontier = r.version()?;
    let n = r.count(MAX_WANT)?;
    let mut embargoes = Vec::new();
    for _ in 0..n {
        embargoes.push(Embargoed {
            version: r.version()?,
            until: read_optional_u64(r)?,
        });
    }
    Ok(Disclosure {
        delayed,
        delay,
        frontier,
        embargoes,
    })
}

/// A flag, then the value, zero when absent.
pub(super) fn put_optional_u64(out: &mut Vec<u8>, value: Option<u64>) {
    out.push(u8::from(value.is_some()));
    out.extend_from_slice(&value.unwrap_or(0).to_be_bytes());
}

pub(super) fn read_optional_u64(r: &mut Reader<'_>) -> Result<Option<u64>, ProtocolError> {
    let present = r.flag()?;
    let value = r.u64()?;
    match present {
        true => Ok(Some(value)),
        false if value == 0 => Ok(None),
        false => Err(malformed("absent number must be zero")),
    }
}
