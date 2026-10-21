//! `CheckReport` (0x60) and `CheckOutcome` (0x61): the encodings and bounds of one completed
//! check result. The tags are dispatched in `codec.rs`; the field layouts live here.

use super::{
    MAX_DETAILS_URL, MAX_SUMMARY,
    hosting::{CheckOutcome, CheckReport, Conclusion},
    wire::*,
};
use crate::ProtocolError;

/// Writes the body of a report, after its tag.
pub(super) fn put_report(out: &mut Vec<u8>, report: &CheckReport) {
    put_short(out, report.operation.as_str());
    put_short(out, &report.repository);
    put_id(out, &report.subject_tree);
    put_optional_id(out, report.subject_checkpoint.as_ref());
    put_short(out, &report.name);
    out.push(report.conclusion.code());
    put_optional_text(out, report.details_url.as_deref());
    put_optional_text(out, report.summary.as_deref());
    out.extend_from_slice(&report.started_at.to_be_bytes());
    out.extend_from_slice(&report.completed_at.to_be_bytes());
}

/// Writes the body of an outcome, after its tag: a sub-tag, then the fields.
pub(super) fn put_outcome(out: &mut Vec<u8>, outcome: &CheckOutcome) {
    match outcome {
        CheckOutcome::Accepted { id, seq } => {
            out.push(0);
            put_id(out, id);
            out.extend_from_slice(&seq.to_be_bytes());
        }
        CheckOutcome::Refused { code } => {
            out.push(1);
            put_short(out, code);
        }
    }
}

/// Encoded size of a report, tag included. Refuses what cannot be encoded.
pub(super) fn report_len(report: &CheckReport) -> Result<usize, ProtocolError> {
    if report.details_url.as_deref() == Some("") || report.summary.as_deref() == Some("") {
        return Err(malformed("optional text must be absent or nonempty"));
    }
    let url = report.details_url.as_deref().map_or(0, str::len);
    let summary = report.summary.as_deref().map_or(0, str::len);
    if url > MAX_DETAILS_URL {
        return Err(limit("details URL exceeds limit"));
    }
    if summary > MAX_SUMMARY {
        return Err(limit("summary exceeds limit"));
    }
    Ok(1 + short(report.operation.as_str())?
        + short(&report.repository)?
        + 32
        + 33
        + short(&report.name)?
        + 1
        + 5
        + url
        + 5
        + summary
        + 16)
}

/// Encoded size of an outcome, tag included.
pub(super) fn outcome_len(outcome: &CheckOutcome) -> Result<usize, ProtocolError> {
    Ok(match outcome {
        CheckOutcome::Accepted { .. } => 42,
        CheckOutcome::Refused { code } => 2 + short(code)?,
    })
}

/// Reads a report body, after its tag.
pub(super) fn read_report(r: &mut Reader) -> Result<CheckReport, ProtocolError> {
    let operation = r.operation()?;
    let repository = r.short_str()?.to_owned();
    let subject_tree = r.id()?;
    let subject_checkpoint = r.optional_id()?;
    let name = r.short_str()?.to_owned();
    let conclusion =
        Conclusion::from_code(r.u8()?).ok_or_else(|| malformed("unknown conclusion"))?;
    Ok(CheckReport {
        operation,
        repository,
        subject_tree,
        subject_checkpoint,
        name,
        conclusion,
        details_url: r.optional_text(MAX_DETAILS_URL)?,
        summary: r.optional_text(MAX_SUMMARY)?,
        started_at: r.u64()?,
        completed_at: r.u64()?,
    })
}

/// Reads an outcome body, after its tag.
pub(super) fn read_outcome(r: &mut Reader) -> Result<CheckOutcome, ProtocolError> {
    Ok(match r.u8()? {
        0 => CheckOutcome::Accepted {
            id: r.id()?,
            seq: r.u64()?,
        },
        1 => CheckOutcome::Refused {
            code: r.short_str()?.to_owned(),
        },
        _ => return Err(malformed("unknown check outcome")),
    })
}

#[cfg(test)]
mod tests {
    use javelin::{ObjectId, OperationId};

    use super::super::{
        CHECK_OUTCOME, CHECK_REPORT, MAX_DETAILS_URL, MAX_SUMMARY, Message, code, decode, encode,
    };
    use super::*;

    fn id(text: &str) -> ObjectId {
        ObjectId::for_blob(text.as_bytes())
    }

    fn report() -> CheckReport {
        CheckReport {
            operation: OperationId::parse("report-1").unwrap(),
            repository: "repo-1".into(),
            subject_tree: id("tree"),
            subject_checkpoint: Some(id("checkpoint")),
            name: "ci/unit".into(),
            conclusion: Conclusion::Failure,
            details_url: Some("https://ci.example/run/1".into()),
            summary: None,
            started_at: 10,
            completed_at: 20,
        }
    }

    #[test]
    fn check_report_round_trips_with_and_without_optional_fields() {
        for message in [
            Message::CheckReport(report()),
            Message::CheckReport(CheckReport {
                subject_checkpoint: None,
                details_url: None,
                summary: Some("2 tests failed".into()),
                ..report()
            }),
        ] {
            let bytes = encode(&message).unwrap();
            assert_eq!(bytes[0], CHECK_REPORT);
            assert_eq!(decode(&bytes).unwrap(), message);
        }
    }

    #[test]
    fn check_outcomes_round_trip() {
        for message in [
            Message::CheckOutcome(CheckOutcome::Accepted {
                id: id("result"),
                seq: 7,
            }),
            Message::CheckOutcome(CheckOutcome::Refused {
                code: code::TREE_MISMATCH.into(),
            }),
        ] {
            let bytes = encode(&message).unwrap();
            assert_eq!(bytes[0], CHECK_OUTCOME);
            assert_eq!(decode(&bytes).unwrap(), message);
        }
    }

    #[test]
    fn empty_optional_text_is_refused_both_ways() {
        let empty = Message::CheckReport(CheckReport {
            summary: Some(String::new()),
            ..report()
        });
        assert_eq!(encode(&empty).unwrap_err().code, code::MALFORMED);

        // A present flag over an empty field is not a canonical encoding.
        let mut bytes = encode(&Message::CheckReport(CheckReport {
            details_url: None,
            summary: None,
            ..report()
        }))
        .unwrap();
        let summary_flag = bytes.len() - 16 - 5;
        bytes[summary_flag] = 1;
        assert_eq!(decode(&bytes).unwrap_err().code, code::MALFORMED);
    }

    #[test]
    fn oversize_text_is_refused_before_encoding() {
        let long_url = Message::CheckReport(CheckReport {
            details_url: Some("u".repeat(MAX_DETAILS_URL + 1)),
            ..report()
        });
        assert_eq!(encode(&long_url).unwrap_err().code, code::LIMIT_EXCEEDED);
        let long_summary = Message::CheckReport(CheckReport {
            summary: Some("s".repeat(MAX_SUMMARY + 1)),
            ..report()
        });
        assert_eq!(
            encode(&long_summary).unwrap_err().code,
            code::LIMIT_EXCEEDED
        );
    }

    #[test]
    fn conclusions_use_the_github_names() {
        for conclusion in [
            Conclusion::Success,
            Conclusion::Failure,
            Conclusion::Neutral,
            Conclusion::Cancelled,
            Conclusion::TimedOut,
            Conclusion::Skipped,
        ] {
            assert_eq!(Conclusion::parse(conclusion.as_str()), Some(conclusion));
            assert_eq!(Conclusion::from_code(conclusion.code()), Some(conclusion));
        }
        assert_eq!(Conclusion::parse("action_required"), None);
        assert_eq!(Conclusion::from_code(6), None);
    }
}
