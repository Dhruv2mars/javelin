//! `javelin node usage`: the node's use against the budget its owner set
//! (`crates/javelin-node/src/budget.rs`), read from the node at `--node https://HOST[:PORT]`
//! for `--repository ID` with this device's credential or `JAVELIN_TOKEN`. Only Maintain
//! reads it. A write the budget refuses is `quota_exceeded`; `quota_hint` says what the
//! owner can do about it.

use std::ffi::OsString;

use javelin::Error;
use javelin_protocol::{
    Message,
    codec::{Usage, code},
};
use serde_json::{Value, json};

use crate::{CliError, cli::Output, cli_disclosure::Target, cli_remote};

const USAGE: &str = "usage: javelin node usage --node URL --repository ID";

/// `javelin node usage`.
pub fn node(args: Vec<OsString>) -> Result<Output, CliError> {
    let mut args = args.into_iter();
    if args.next().is_none_or(|command| command != "usage") {
        return Err(usage());
    }
    let (target, rest) = Target::parse(args.collect(), USAGE)?;
    if !rest.is_empty() {
        return Err(usage());
    }
    let reply = cli_remote::call_node(&target.node, &target.repository, &Message::ReadUsage)
        .map_err(CliError::Remote)?;
    match reply {
        Message::Usage(usage) => Ok(render(&target.repository, &usage)),
        _ => Err(CliError::Remote(cli_remote::RemoteError::new(
            "unexpected_message",
            "the node did not answer with its usage",
        ))),
    }
}

fn usage() -> CliError {
    CliError::Native(Error::InvalidInput(USAGE.into()))
}

fn render(repository: &str, usage: &Usage) -> Output {
    let mut text = format!("Budget of {repository} (UTC day {}):", usage.day);
    let mut meters = serde_json::Map::new();
    for meter in &usage.meters {
        let line = match meter.used {
            Some(used) => {
                let share = if meter.limit == 0 {
                    100.0
                } else {
                    used as f64 * 100.0 / meter.limit as f64
                };
                format!(
                    "{} of {} ({share:.1}%)",
                    grouped(used),
                    grouped(meter.limit)
                )
            }
            None => format!("at most {} per write", grouped(meter.limit)),
        };
        text.push_str(&format!("\n  {:<22} {line}", meter.budget));
        meters.insert(
            meter.budget.clone(),
            json!({"used": meter.used, "limit": meter.limit}),
        );
    }
    (
        json!({"ok": true, "command": "node usage", "day": usage.day, "budgets": Value::Object(meters)}),
        text,
        0,
    )
}

/// `1234567` as `1,234,567`.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What to do about a `quota_exceeded` refusal whose detail is `detail`: which budget ran
/// out, that nothing was stored, and how the node's owner raises it.
pub fn quota_hint(detail: &str) -> String {
    let budget = detail.split(':').next().unwrap_or(detail).trim();
    let what = match budget {
        "storage_bytes" => {
            "The repository's storage budget is spent. Nothing was stored; retry once the node's owner raises storage_bytes."
        }
        "rows_written_per_day" => {
            "Today's write budget is spent. Nothing was stored; retry after 00:00 UTC, or once the node's owner raises rows_written_per_day."
        }
        "largest_object" => {
            "An object is larger than the node accepts. Nothing was stored; retry once the node's owner raises largest_object."
        }
        _ => {
            "A budget the node's owner set is spent. Nothing was stored; retry once the owner raises it."
        }
    };
    format!(
        "{what}\nThe owner raises a budget in budget.json in a self-hosted node's data directory, then restarts the node, or with PUT /admin/r/REPOSITORY/budget on a Cloudflare node. A maintainer sees the use with `javelin node usage --node URL --repository ID`."
    )
}

/// The stall line's text for a refusal `code` with `detail`, with the hint for a budget.
pub fn refusal_text(code_: &str, detail: &str) -> String {
    if code_ == code::QUOTA_EXCEEDED {
        format!("{code_}: {detail}\n{}", quota_hint(detail))
    } else {
        format!("{code_}: {detail}")
    }
}

#[cfg(test)]
mod tests {
    use super::{grouped, quota_hint};

    #[test]
    fn numbers_group_by_thousands_and_hints_name_the_budget() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
        assert_eq!(grouped(1_000_000_000), "1,000,000,000");
        assert!(quota_hint("rows_written_per_day").contains("00:00 UTC"));
        assert!(quota_hint("storage_bytes: 1 of 2 bytes stored").contains("raises storage_bytes"));
    }
}
