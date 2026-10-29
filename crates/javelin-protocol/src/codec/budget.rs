//! Budget messages (0x4d-0x4e): a node's use against the budget its owner set, so a write
//! that would cross a provider's free tier is refused with `quota_exceeded` instead of
//! failing however the provider fails. The tags are dispatched in `codec.rs`; the field
//! layouts live here.

use super::wire::*;
use crate::ProtocolError;

/// Most meters in one `Usage`.
pub const MAX_METERS: usize = 16;

/// A node's use against each budget, for a maintainer: `day` is the UTC day (Unix seconds
/// divided by 86,400) that daily meters count.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Usage {
    pub day: u64,
    pub meters: Vec<Meter>,
}

/// One budget: its name, the use the node counts against it, when it counts one, and the
/// limit. A meter without use, such as the largest object, is a bound on each write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Meter {
    pub budget: String,
    pub used: Option<u64>,
    pub limit: u64,
}

/// Writes the body of a `Usage`, after its tag.
pub(super) fn put_usage(out: &mut Vec<u8>, usage: &Usage) {
    out.extend_from_slice(&usage.day.to_be_bytes());
    put_u32(out, usage.meters.len());
    for meter in &usage.meters {
        put_short(out, &meter.budget);
        super::disclosure::put_optional_u64(out, meter.used);
        out.extend_from_slice(&meter.limit.to_be_bytes());
    }
}

/// Encoded size of a `Usage`, tag included.
pub(super) fn usage_len(usage: &Usage) -> Result<usize, ProtocolError> {
    if usage.meters.len() > MAX_METERS {
        return Err(limit("count exceeds limit"));
    }
    let mut len = 1 + 8 + 4;
    for meter in &usage.meters {
        len += short(&meter.budget)? + 9 + 8;
    }
    Ok(len)
}

pub(super) fn read_usage(r: &mut Reader<'_>) -> Result<Usage, ProtocolError> {
    let day = r.u64()?;
    let n = r.count(MAX_METERS)?;
    let mut meters = Vec::new();
    for _ in 0..n {
        meters.push(Meter {
            budget: r.short_str()?.to_owned(),
            used: super::disclosure::read_optional_u64(r)?,
            limit: r.u64()?,
        });
    }
    Ok(Usage { day, meters })
}
