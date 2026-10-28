//! The Publish Request reads a client lists and shows requests with: `ListRequests` (0x57)
//! and `RequestList` (0x58), `ReadReview` (0x59) and `ReviewDetail` (0x5a). The tags are
//! dispatched in `codec.rs`; the field layouts and their bounds live here.

use javelin::{LayerId, ObjectId, VersionNumber};

use super::{
    hosting::{RequestId, RequestStatus},
    wire::*,
};
use crate::{
    ProtocolError,
    limits::{MAX_COMMENT, MAX_WANT},
};

/// Most requests in one `RequestList`. A longer list continues from the last one listed.
pub const MAX_REQUESTS: usize = 1024;

/// One Publish Request as `ListRequests` lists it: its Layer, author, state and latest
/// revision number (0 before the first).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestSummary {
    pub request: RequestId,
    pub layer: LayerId,
    pub author: String,
    pub state: RequestStatus,
    pub revision: u32,
}

/// One immutable revision and the principals that approved exactly it. `checkpoint` is
/// `None` when the caller may not read the revision's checkpoint, which names paths and
/// content IDs, so its ID never confirms what a hidden entry holds. The revision's snapshot
/// tree is never sent: it names hidden content in plain form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevisionDetail {
    pub index: u32,
    pub base: VersionNumber,
    pub checkpoint: Option<ObjectId>,
    pub approvals: Vec<String>,
}

/// One comment. `revision: None` is on the request as a whole.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommentDetail {
    pub author: String,
    pub revision: Option<u32>,
    pub body: String,
}

/// Reply to `ReadReview`: the request, every revision with its approvals, the approvals
/// Publish needs now, and the comments from the one asked for. `comments_total` counts
/// them all, so a client asks again from where this reply stopped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewDetail {
    pub summary: RequestSummary,
    pub required_approvals: u32,
    pub revisions: Vec<RevisionDetail>,
    pub comments: Vec<CommentDetail>,
    pub comments_total: u32,
}

pub(super) fn put_summary(out: &mut Vec<u8>, summary: &RequestSummary) {
    put_short(out, summary.request.as_str());
    put_short(out, summary.layer.as_str());
    put_short(out, &summary.author);
    out.push(summary.state.code());
    out.extend_from_slice(&summary.revision.to_be_bytes());
}

pub(super) fn summary_len(summary: &RequestSummary) -> Result<usize, ProtocolError> {
    Ok(short(summary.request.as_str())?
        + short(summary.layer.as_str())?
        + short(&summary.author)?
        + 1
        + 4)
}

pub(super) fn read_summary(r: &mut Reader<'_>) -> Result<RequestSummary, ProtocolError> {
    Ok(RequestSummary {
        request: r.request()?,
        layer: r.layer()?,
        author: r.short_str()?.to_owned(),
        state: r.request_status()?,
        revision: r.u32()?,
    })
}

/// Writes a `RequestList` body after its tag.
pub(super) fn put_list(out: &mut Vec<u8>, requests: &[RequestSummary], more: bool) {
    put_u32(out, requests.len());
    for summary in requests {
        put_summary(out, summary);
    }
    out.push(u8::from(more));
}

pub(super) fn list_len(requests: &[RequestSummary]) -> Result<usize, ProtocolError> {
    if requests.len() > MAX_REQUESTS {
        return Err(limit("count exceeds limit"));
    }
    let mut len = 1 + 4 + 1;
    for summary in requests {
        len += summary_len(summary)?;
    }
    Ok(len)
}

pub(super) fn read_list(r: &mut Reader<'_>) -> Result<(Vec<RequestSummary>, bool), ProtocolError> {
    let n = r.count(MAX_REQUESTS)?;
    let mut requests = Vec::new();
    for _ in 0..n {
        requests.push(read_summary(r)?);
    }
    Ok((requests, r.flag()?))
}

/// Writes a `ReviewDetail` body after its tag.
pub(super) fn put_detail(out: &mut Vec<u8>, detail: &ReviewDetail) {
    put_summary(out, &detail.summary);
    out.extend_from_slice(&detail.required_approvals.to_be_bytes());
    put_u32(out, detail.revisions.len());
    for revision in &detail.revisions {
        out.extend_from_slice(&revision.index.to_be_bytes());
        out.extend_from_slice(&revision.base.get().to_be_bytes());
        put_optional_id(out, revision.checkpoint.as_ref());
        put_u32(out, revision.approvals.len());
        for approver in &revision.approvals {
            put_short(out, approver);
        }
    }
    put_u32(out, detail.comments.len());
    for comment in &detail.comments {
        put_short(out, &comment.author);
        put_optional_u32(out, comment.revision);
        put_u32(out, comment.body.len());
        out.extend_from_slice(comment.body.as_bytes());
    }
    out.extend_from_slice(&detail.comments_total.to_be_bytes());
}

/// Encoded size of a `ReviewDetail`, tag included. Refuses what cannot be encoded.
pub(super) fn detail_len(detail: &ReviewDetail) -> Result<usize, ProtocolError> {
    if detail.revisions.len() > MAX_WANT || detail.comments.len() > MAX_WANT {
        return Err(limit("count exceeds limit"));
    }
    let mut len = 1 + summary_len(&detail.summary)? + 4 + 4 + 4 + 4;
    for revision in &detail.revisions {
        if revision.approvals.len() > MAX_WANT {
            return Err(limit("count exceeds limit"));
        }
        len += 4 + 8 + 33 + 4;
        for approver in &revision.approvals {
            len += short(approver)?;
        }
    }
    for comment in &detail.comments {
        if comment.body.len() > MAX_COMMENT {
            return Err(limit("comment exceeds limit"));
        }
        len = len.saturating_add(short(&comment.author)? + 5 + 4 + comment.body.len());
    }
    Ok(len)
}

pub(super) fn read_detail(r: &mut Reader<'_>) -> Result<ReviewDetail, ProtocolError> {
    let summary = read_summary(r)?;
    let required_approvals = r.u32()?;
    let n = r.count(MAX_WANT)?;
    let mut revisions = Vec::new();
    for _ in 0..n {
        let index = r.u32()?;
        let base = r.version()?;
        let checkpoint = r.optional_id()?;
        let count = r.count(MAX_WANT)?;
        let mut approvals = Vec::new();
        for _ in 0..count {
            approvals.push(r.short_str()?.to_owned());
        }
        revisions.push(RevisionDetail {
            index,
            base,
            checkpoint,
            approvals,
        });
    }
    let n = r.count(MAX_WANT)?;
    let mut comments = Vec::new();
    for _ in 0..n {
        comments.push(CommentDetail {
            author: r.short_str()?.to_owned(),
            revision: r.optional_u32()?,
            body: r.text32(MAX_COMMENT)?,
        });
    }
    Ok(ReviewDetail {
        summary,
        required_approvals,
        revisions,
        comments,
        comments_total: r.u32()?,
    })
}
