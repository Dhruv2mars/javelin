//! `javelin frontier`, `javelin delay` and `javelin embargo`: the public frontier of a hosted
//! repository (`docs/direction.md`, Superposition). Each sends one disclosure message to the
//! node at `--node https://HOST[:PORT]` for `--repository ID`, with this device's credential
//! for the repository or `JAVELIN_TOKEN`. Reading goes out signed out when there is no
//! credential; a caller outside the repository sees the frontier and whether the public
//! view is delayed, and nothing else. Changes need Maintain.

use std::ffi::OsString;

use javelin::{Error, OperationId, VersionNumber};
use javelin_protocol::{
    Message,
    codec::{Disclosure, Embargoed},
};
use serde_json::{Value, json};

use crate::{CliError, cli::Output, cli_remote};

const FRONTIER_USAGE: &str = "usage: javelin frontier --node URL --repository ID";
const DELAY_USAGE: &str = "usage: javelin delay [DURATION] --node URL --repository ID \
(DURATION: SECONDS or N followed by s, m, h or d; 0 turns the delay off)";
const EMBARGO_USAGE: &str = "usage: javelin embargo --operation ID [--until UNIX] --node URL --repository ID\n       \
javelin embargo release VERSION --node URL --repository ID";

/// `javelin frontier`: the newest version readers outside the repository see, and for a
/// member the delay and unreleased embargoes behind it.
pub fn frontier(args: Vec<OsString>) -> Result<Output, CliError> {
    let (target, rest) = Target::parse(args, FRONTIER_USAGE)?;
    if !rest.is_empty() {
        return Err(usage(FRONTIER_USAGE));
    }
    let disclosure = target.send(&Message::ReadDisclosure, false)?;
    Ok(render("frontier", &disclosure))
}

/// `javelin delay`: shows the disclosure delay, or sets it (Maintain).
pub fn delay(args: Vec<OsString>) -> Result<Output, CliError> {
    let (target, rest) = Target::parse(args, DELAY_USAGE)?;
    let disclosure = match rest.as_slice() {
        [] => target.send(&Message::ReadDisclosure, false)?,
        [duration] => target.send(
            &Message::SetDelay {
                delay: parse_duration(duration)?,
            },
            true,
        )?,
        _ => return Err(usage(DELAY_USAGE)),
    };
    Ok(render("delay", &disclosure))
}

/// `javelin embargo`: embargoes a Publish before it is sent, or releases an embargoed version
/// (Maintain). The Publish carries the embargo through its operation ID: send this first,
/// then the Publish with the same ID.
pub fn embargo(args: Vec<OsString>) -> Result<Output, CliError> {
    let (target, rest) = Target::parse(args, EMBARGO_USAGE)?;
    let message = match rest.as_slice() {
        [release, version] if release == "release" => Message::Release {
            version: version
                .parse::<u64>()
                .ok()
                .and_then(|n| VersionNumber::new(n).ok())
                .ok_or_else(|| usage(EMBARGO_USAGE))?,
        },
        _ => {
            let mut operation = None;
            let mut until = None;
            let mut rest = rest.iter();
            while let Some(flag) = rest.next() {
                let value = rest.next().ok_or_else(|| usage(EMBARGO_USAGE))?;
                match flag.as_str() {
                    "--operation" if operation.is_none() => {
                        operation = Some(OperationId::parse(value)?);
                    }
                    "--until" if until.is_none() => {
                        until = Some(value.parse::<u64>().map_err(|_| usage(EMBARGO_USAGE))?);
                    }
                    _ => return Err(usage(EMBARGO_USAGE)),
                }
            }
            Message::Embargo {
                operation: operation.ok_or_else(|| usage(EMBARGO_USAGE))?,
                until,
            }
        }
    };
    let disclosure = target.send(&message, true)?;
    Ok(render("embargo", &disclosure))
}

/// The node and repository from `--node` and `--repository`, and the other arguments.
struct Target {
    node: String,
    repository: String,
}

impl Target {
    fn parse(args: Vec<OsString>, help: &str) -> Result<(Self, Vec<String>), CliError> {
        let mut node = None;
        let mut repository = None;
        let mut rest = Vec::new();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let arg = arg.into_string().map_err(|_| usage(help))?;
            let slot = match arg.as_str() {
                "--node" => &mut node,
                "--repository" => &mut repository,
                _ => {
                    rest.push(arg);
                    continue;
                }
            };
            let value = args.next().and_then(|v| v.into_string().ok());
            if slot.is_some() || value.is_none() {
                return Err(usage(help));
            }
            *slot = value;
        }
        match (node, repository) {
            (Some(node), Some(repository)) => Ok((Self { node, repository }, rest)),
            _ => Err(usage(help)),
        }
    }

    /// One disclosure message and its `Disclosure` reply. A change needs a credential.
    fn send(&self, message: &Message, write: bool) -> Result<Disclosure, CliError> {
        let reply = if write {
            cli_remote::call_node(&self.node, &self.repository, message)
        } else {
            cli_remote::read_node(&self.node, &self.repository, message)
        }
        .map_err(CliError::Remote)?;
        match reply {
            Message::Disclosure(disclosure) => Ok(disclosure),
            _ => Err(CliError::Remote(cli_remote::RemoteError {
                code: "unexpected_message".into(),
                detail: "the node did not answer with its disclosure".into(),
            })),
        }
    }
}

/// `SECONDS`, or a count followed by `s`, `m`, `h` or `d`.
fn parse_duration(text: &str) -> Result<u64, CliError> {
    let (digits, unit) = match text.char_indices().last() {
        Some((at, 's')) => (&text[..at], 1),
        Some((at, 'm')) => (&text[..at], 60),
        Some((at, 'h')) => (&text[..at], 3600),
        Some((at, 'd')) => (&text[..at], 86_400),
        _ => (text, 1),
    };
    digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(unit))
        .filter(|seconds| *seconds <= i64::MAX as u64)
        .ok_or_else(|| usage(DELAY_USAGE))
}

fn usage(text: &str) -> CliError {
    CliError::Native(Error::InvalidInput(text.into()))
}

fn render(command: &str, disclosure: &Disclosure) -> Output {
    let embargoes: Vec<Value> = disclosure
        .embargoes
        .iter()
        .map(|Embargoed { version, until }| json!({"version": version.get(), "until": until}))
        .collect();
    let mut text = format!("Public frontier: World {}.", disclosure.frontier.get());
    match disclosure.delay {
        Some(0) => text.push_str(
            "\nNo disclosure delay: outsiders see each version once it is not embargoed.",
        ),
        Some(delay) => text.push_str(&format!(
            "\nDisclosure delay: {}; outsiders see each version that long after it is published.",
            describe(delay)
        )),
        None if disclosure.delayed => text.push_str("\nThe public view is delayed."),
        None => {}
    }
    for Embargoed { version, until } in &disclosure.embargoes {
        text.push_str(&match until {
            Some(until) => format!("\nWorld {version} is embargoed until Unix time {until}."),
            None => format!("\nWorld {version} is embargoed until a maintainer releases it."),
        });
    }
    (
        json!({
            "ok": true,
            "command": command,
            "frontier": disclosure.frontier.get(),
            "delayed": disclosure.delayed,
            "delay": disclosure.delay,
            "embargoes": embargoes,
        }),
        text,
        0,
    )
}

/// A delay in the largest whole unit.
fn describe(seconds: u64) -> String {
    for (unit, size) in [("d", 86_400), ("h", 3600), ("m", 60)] {
        if seconds % size == 0 {
            return format!("{}{unit}", seconds / size);
        }
    }
    format!("{seconds}s")
}

#[cfg(test)]
mod tests {
    use super::{describe, parse_duration};

    #[test]
    fn durations_parse_in_seconds_and_print_in_the_largest_unit() {
        assert_eq!(parse_duration("90").unwrap(), 90);
        assert_eq!(parse_duration("7d").unwrap(), 7 * 86_400);
        assert_eq!(parse_duration("12h").unwrap(), 12 * 3600);
        assert_eq!(parse_duration("0").unwrap(), 0);
        assert!(parse_duration("d").is_err());
        assert!(parse_duration("-1").is_err());
        assert!(parse_duration("9999999999999999999d").is_err());
        assert_eq!(describe(7 * 86_400), "7d");
        assert_eq!(describe(90), "90s");
        assert_eq!(describe(120), "2m");
    }
}
