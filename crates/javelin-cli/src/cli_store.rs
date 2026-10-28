//! What this clone keeps under `.javelin/` for its hosted repository, as JSON objects replaced
//! atomically: the requests it authored (`requests.json`, in `cli_request`), its Layer updates
//! (`updates.json`, in `cli_update`), and the base of each request revision it has seen
//! (`request-bases.json`, here), which lets a Publish queue without `--base`.

use std::{fs, io::Write, path::Path};

use javelin::Error;
use javelin_protocol::{
    Message,
    codec::{RequestId, RequestSummary, decode},
};
use serde_json::{Value, json};

use crate::CliError;

/// A JSON object kept under `.javelin/`, empty when the file is missing.
pub(crate) fn read_json(
    root: &Path,
    name: &str,
) -> Result<serde_json::Map<String, Value>, CliError> {
    match fs::read(root.join(".javelin").join(name)) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(Value::Object(map)) => Ok(map),
            _ => Err(Error::CorruptRepository(format!("{name} is not a JSON object")).into()),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(error) => Err(Error::from(error).into()),
    }
}

/// Replaces `.javelin/NAME` atomically: a temporary file, synced, then renamed.
pub(crate) fn write_json(
    root: &Path,
    name: &str,
    map: serde_json::Map<String, Value>,
) -> Result<(), CliError> {
    let path = root.join(".javelin").join(name);
    let temporary = path.with_extension("json.tmp");
    let write = || -> std::io::Result<()> {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(
            &serde_json::to_vec_pretty(&Value::Object(map)).map_err(std::io::Error::other)?,
        )?;
        file.sync_all()?;
        fs::rename(&temporary, &path)
    };
    write().map_err(|error| CliError::from(Error::from(error)))
}

/// Where this clone keeps the base of each request revision it has seen.
const BASES: &str = "request-bases.json";

/// Records the remote World each of `revisions` of `request` is over, so a Publish of it can
/// queue while the node is down without `--base`.
pub(crate) fn remember_bases(
    root: &Path,
    request: &RequestId,
    revisions: impl IntoIterator<Item = (u32, u64)>,
) -> Result<(), CliError> {
    let mut bases = read_json(root, BASES)?;
    let entry = bases
        .entry(request.as_str().to_owned())
        .or_insert_with(|| json!({}));
    let before = entry.clone();
    for (revision, base) in revisions {
        if revision > 0 {
            entry[revision.to_string()] = json!(base);
        }
    }
    if *entry == before {
        return Ok(());
    }
    write_json(root, BASES, bases)
}

/// Records each listed request's current revision base.
pub(crate) fn remember_listed(root: &Path, requests: &[RequestSummary]) -> Result<(), CliError> {
    let mut bases = read_json(root, BASES)?;
    let mut changed = false;
    for r in requests.iter().filter(|r| r.revision > 0) {
        let entry = bases
            .entry(r.request.as_str().to_owned())
            .or_insert_with(|| json!({}));
        let key = r.revision.to_string();
        if entry[&key] != json!(r.base.get()) {
            entry[&key] = json!(r.base.get());
            changed = true;
        }
    }
    match changed {
        true => write_json(root, BASES, bases),
        false => Ok(()),
    }
}

pub(crate) fn remembered_base(
    root: &Path,
    request: &RequestId,
    revision: u32,
) -> Result<Option<u64>, CliError> {
    Ok(read_json(root, BASES)?
        .get(request.as_str())
        .and_then(|revisions| revisions[revision.to_string()].as_u64()))
}

/// The base a queued revise of `request` names for `revision`: it becomes revision
/// `expected + 1` when the node takes it.
pub(crate) fn queued_base(
    outbox: &javelin::outbox::Outbox,
    request: &RequestId,
    revision: u32,
) -> Option<u64> {
    outbox
        .pending()
        .find_map(|entry| match decode(&entry.payload).ok()? {
            Message::Revise {
                request: queued,
                expected_revision,
                base,
                ..
            } if queued == *request && expected_revision.checked_add(1) == Some(revision) => {
                Some(base.get())
            }
            _ => None,
        })
}
