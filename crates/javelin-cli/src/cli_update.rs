//! `javelin layer update ID [--as NEW]` moves a Layer's work onto the attached node's current
//! World, and the retirement of Layers whose request the node published.
//!
//! A Layer's base is immutable (`docs/build-contract.md`): every checkpoint names it, and the
//! node holds the same rule. So an update makes a successor Layer over the node's head, holding
//! the three-way merge of the Layer's work onto that World (`javelin-merge`), and retires the
//! old Layer. A region both sides changed is written into the successor as text between
//! markers ([`javelin_merge::conflict_text`]); other conflicts keep the Layer's side. The owner
//! resolves them by editing the directory, which the next command captures. `request open` and
//! `request revise` refuse while a marked file still holds markers, and a revise with no
//! `--layer` follows the request's Layer to its latest successor.
//!
//! Each step is durable before the next, and each repeats safely. The update is recorded in
//! `.javelin/updates.json` before the successor exists; a retry finishes it from that record
//! against the same World, and the old Layer, with its directory, is retired only after the
//! successor's directory is open. A crash anywhere loses nothing.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use javelin::{
    Choice, Error, FileEntry, LayerId, LayerState, LayerUpdate, Manifest, ObjectId, OperationId,
    RemoteHead, RepoPath, Repository, SparseChanges, VersionNumber,
};
use javelin_merge::{ConflictKind, Labels, Merged, Resource};
use javelin_protocol::{
    Message,
    codec::{RequestId, RequestStatus, RequestSummary},
};
use serde_json::{Value, json};

use crate::{
    CliError,
    cli_remote::{LOCAL_WORLD_DIVERGED, Link, RemoteError, parse_target, recover_attachment},
    cli_request::{Options, Outcome},
    cli_sync::follow,
};

/// Where this clone records the updates it made: old Layer -> [`Update`].
const UPDATES: &str = "updates.json";
/// Refusal for proposing a Layer whose update left markers in a file.
pub(crate) const UNRESOLVED: &str = "unresolved_conflict";

/// One update, recorded before its successor is made, so a retry finishes the same one.
struct Update {
    successor: LayerId,
    /// The old Layer's head checkpoint the merge took.
    from: ObjectId,
    /// The remote World the old Layer was over.
    was: RemoteHead,
    /// The node's head the successor is over, and the local World that holds it.
    base: RemoteHead,
    local: VersionNumber,
}

/// A path the merge could not resolve, as written into the successor.
struct Conflicted {
    path: RepoPath,
    kind: ConflictKind,
    /// Written as text between markers; otherwise the Layer's side was kept.
    marked: bool,
}

/// `javelin layer update ID [--as NEW] [--repo DIR]`
pub fn layer_update(args: Vec<OsString>) -> Outcome {
    let usage = "javelin layer update ID [--as NEW] [--repo DIR] [--json]";
    let options = Options::parse(args, &["--as", "--repo"], usage)?;
    let [id] = options.positionals.as_slice() else {
        return Err(Error::InvalidInput(format!("expected: {usage}")).into());
    };
    let old = LayerId::parse(id)?;
    let named = options.get("--as").map(LayerId::parse).transpose()?;
    let root = options.root()?;
    let mut repo = Repository::open(&root)?;
    let attached = repo.attachment()?.ok_or_else(|| {
        Error::InvalidInput("this repository is not attached to a node; attach first".into())
    })?;
    repo.set_provenance(crate::cli::provenance(None)?);
    let mut updates = read_json(&root, UPDATES)?;
    if let Some(update) = updates.get(old.as_str()).map(decode_update).transpose()? {
        if named
            .as_ref()
            .is_some_and(|named| *named != update.successor)
        {
            return Err(Error::InvalidInput(format!(
                "Layer {} was already updated as Layer {}",
                old.as_str(),
                update.successor.as_str()
            ))
            .into());
        }
        return finish(&mut repo, &root, &old, &update, None);
    }

    let capture = repo.capture_workspace(&old)?;
    if capture.as_ref().is_some_and(|capture| capture.behind) {
        return Err(Error::InvalidInput(format!(
            "Layer {0}'s directory lacks its stacked head; run `javelin workspace refresh {0}` \
             first",
            old.as_str()
        ))
        .into());
    }
    let head = active_head(&repo, &old)?;
    let unresolved = unresolved(&repo, &root, &old)?;
    if !unresolved.is_empty() {
        return Err(unresolved_error(&old, &unresolved));
    }
    let checkpoint = repo.read_checkpoint(head.checkpoint())?;
    let was = repo
        .remote_world(checkpoint.base().version())?
        .ok_or_else(|| {
            Error::InvalidInput(format!(
                "Layer {}'s base was not fetched from the node; only a Layer over a World that \
                 attach or sync brought here can be updated",
                old.as_str()
            ))
        })?;

    // The local World follows the node's head first, as `javelin sync` does.
    let link = Link::new(parse_target(&format!(
        "{}/{}",
        attached.node, attached.repository
    ))?);
    let attached = recover_attachment(&link, &mut repo, attached)?;
    if !repo.follows_attachment(&attached)? {
        let local = repo.head()?;
        return Err(CliError::Remote(RemoteError::new(
            LOCAL_WORLD_DIVERGED,
            format!(
                "local World {} is not the attached remote World {}: it was published here, \
                 not on the node, so this repository cannot follow the node. Update the Layer \
                 in a fresh `javelin attach` directory",
                local.number, attached.head.number
            ),
        )));
    }
    let local = repo.head()?;
    let node = follow(&link, &root, &mut repo, &attached, &local, None)?;
    if node.number != attached.head.number {
        repo.warm_base()?;
    }
    if node.id == was.id {
        let value = json!({
            "ok": true,
            "command": "layer update",
            "layer": old,
            "status": "current",
            "base": was,
        });
        let text = format!(
            "Layer {} is already over the node's head, remote World {}. Nothing changed.",
            old.as_str(),
            was.number
        );
        return Ok(crate::cli::captured(capture, (value, text, 0)));
    }
    let successor = match named {
        Some(named) => named,
        None => next_name(&repo, &old)?,
    };
    if repo.layer_state(&successor).is_ok() {
        return Err(Error::InvalidInput(format!(
            "Layer {} already exists; name the updated Layer with --as NEW",
            successor.as_str()
        ))
        .into());
    }
    let update = Update {
        successor,
        from: head.checkpoint().object(),
        was,
        base: node,
        local: repo.head()?.number,
    };
    updates.insert(old.as_str().into(), encode_update(&update));
    write_json(&root, UPDATES, updates)?;
    finish(&mut repo, &root, &old, &update, capture)
}

/// Makes the successor from the recorded update, opens its directory, then retires the old
/// Layer. Every step replays: the successor's create and capture under IDs derived from the
/// update, the directory open, and the old Layer's discard.
fn finish(
    repo: &mut Repository,
    root: &Path,
    old: &LayerId,
    update: &Update,
    capture: Option<javelin::Capture>,
) -> Outcome {
    let successor = &update.successor;
    let retire = match repo.layer_state(old)? {
        LayerState::Active { .. } => {
            repo.capture_workspace(old)?;
            let head = active_head(repo, old)?;
            if head.checkpoint().object() != update.from {
                return Err(Error::InvalidInput(format!(
                    "Layer {} changed after its update to Layer {} began; that update holds \
                     checkpoint {}. Continue in Layer {1}, or discard it and update again",
                    old.as_str(),
                    successor.as_str(),
                    update.from
                ))
                .into());
            }
            Some(head)
        }
        _ => None,
    };
    let (changes, conflicts) = merge(repo, old, update)?;
    // The marked files, for the guard on `request open` and `revise`. Rewritten on a retry,
    // with the same paths.
    let mut updates = read_json(root, UPDATES)?;
    let mut record = encode_update(update);
    record["marked"] = conflicts
        .iter()
        .filter(|c| c.marked)
        .map(|c| json!(c.path))
        .collect();
    updates.insert(old.as_str().into(), record);
    write_json(root, UPDATES, updates)?;
    let base = repo.layer_base(update.local)?;
    let start = repo.create_layer(successor, &base)?;
    let operation = derived("update", successor, update.from);
    if !changes.entries().is_empty() && repo.layer_attempt(&operation, successor)?.is_none() {
        match repo.capture_layer(&operation, &start, &changes)? {
            LayerUpdate::Applied { .. } => {}
            LayerUpdate::Stale { .. } => {
                return Err(Error::InvalidInput(format!(
                    "Layer {} moved while the update was writing it; retry",
                    successor.as_str()
                ))
                .into());
            }
        }
    }
    failpoint("update_after_successor");
    let opened = repo.open_workspace(successor)?;
    let mut retired = false;
    if let Some(head) = retire {
        let discard = derived("update-retire", old, update.from);
        retired = repo
            .discard_workspace(&discard, &head)
            .map(|discarded| matches!(discarded.update, LayerUpdate::Applied { .. }))?;
    }
    let head = active_head(repo, successor)?;
    let clean = conflicts.is_empty();
    let listed: Vec<Value> = conflicts
        .iter()
        .map(|c| {
            json!({
                "path": c.path,
                "kind": kind(c.kind),
                "written": if c.marked { "markers" } else { "layer_side" },
            })
        })
        .collect();
    let value = json!({
        "ok": clean,
        "command": "layer update",
        "layer": old,
        "status": "updated",
        "successor": successor,
        "from": update.was,
        "base": update.base,
        "head": head,
        "root": opened.root,
        "retired": retired,
        "conflicts": listed,
        "code": if clean { Value::Null } else { json!("conflict") },
    });
    let mut text = format!(
        "Layer {} moved from remote World {} to remote World {} as Layer {} (revision {}); \
         Layer {0} is retired.\nWorkspace: {}",
        old.as_str(),
        update.was.number,
        update.base.number,
        successor.as_str(),
        head.revision(),
        opened.root.display()
    );
    if !clean {
        text.push_str(
            "\nConflicts: edit these files, then capture (any javelin command for the Layer \
             does); `request revise` refuses while markers remain:",
        );
        for c in &conflicts {
            let written = if c.marked {
                "both sides between markers"
            } else {
                "kept the Layer's side"
            };
            text.push_str(&format!(
                "\n  {} ({}): {written}",
                serde_json::to_string(c.path.as_str()).expect("a path is JSON"),
                kind(c.kind)
            ));
        }
    }
    text.push_str(&format!(
        "\nA request opened from Layer {} is revised from Layer {}: `javelin request revise \
         ID --revision N --operation OP`.",
        old.as_str(),
        successor.as_str()
    ));
    let exit = if clean { 0 } else { 3 };
    Ok(crate::cli::captured(capture, (value, text, exit)))
}

/// The successor's changes over its base: the old Layer's work merged onto the node's World,
/// with every conflict written out, and the conflicts.
fn merge(
    repo: &mut Repository,
    old: &LayerId,
    update: &Update,
) -> Result<(SparseChanges, Vec<Conflicted>), CliError> {
    let from = javelin::CheckpointId::parse(&update.from.to_string())?;
    let checkpoint = repo.read_checkpoint(from)?;
    let base = repo.manifest_at(checkpoint.base().version())?;
    let ours = repo.checkpoint_manifest(from)?;
    let theirs = repo.manifest_at(update.local)?;
    let labels = (
        format!("Layer {}", old.as_str()),
        format!("remote World {}", update.was.number),
        format!("remote World {}", update.base.number),
    );
    let labels = Labels {
        ours: &labels.0,
        base: &labels.1,
        theirs: &labels.2,
    };
    let load =
        |repo: &Repository, id| repo.read_blob_bounded(id, Resource::BlobBytes.limit() as usize);
    let first = javelin_merge::merge_resolved(
        &base,
        &ours,
        &theirs,
        &BTreeMap::new(),
        |paths| repo.aliased_paths(paths),
        |id| load(repo, id),
    )
    .map_err(merge_error)?;
    let mut choices = BTreeMap::new();
    let mut conflicts = Vec::new();
    for conflict in &first.conflicts {
        let text = |entry: &Option<FileEntry>| -> Result<Option<Vec<u8>>, CliError> {
            match entry {
                None => Ok(Some(Vec::new())),
                Some(FileEntry::Regular { content, .. }) => {
                    let bytes = load(repo, *content)?;
                    Ok((!bytes.contains(&0)).then_some(bytes))
                }
                Some(_) => Ok(None),
            }
        };
        let marked = match conflict.kind {
            ConflictKind::Content | ConflictKind::AddAdd => {
                match (
                    text(&conflict.base)?,
                    text(&conflict.ours)?,
                    text(&conflict.theirs)?,
                ) {
                    (Some(b), Some(o), Some(t)) if conflict.ours.is_some() => {
                        Some(javelin_merge::conflict_text(&b, &o, &t, labels))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        conflicts.push(Conflicted {
            path: conflict.path.clone(),
            kind: conflict.kind,
            marked: marked.is_some(),
        });
        let choice = match marked {
            Some(bytes) => Choice::Content {
                bytes,
                executable: None,
            },
            None => Choice::Ours,
        };
        choices.insert(conflict.path.clone(), choice);
    }
    let merged = match choices.is_empty() {
        true => first,
        false => javelin_merge::merge_resolved(
            &base,
            &ours,
            &theirs,
            &choices,
            |paths| repo.aliased_paths(paths),
            |id| load(repo, id),
        )
        .map_err(merge_error)?,
    };
    if !merged.conflicts.is_empty() {
        let paths: Vec<_> = merged.conflicts.iter().map(|c| c.path.as_str()).collect();
        return Err(Error::InvalidInput(format!(
            "Layer {}'s work cannot be written over remote World {}: these paths would collide \
             as a file and a directory, or as two spellings of one name, whichever side is kept: \
             {}. Nothing was changed; move the work by hand into a new Layer",
            old.as_str(),
            update.base.number,
            paths.join(", ")
        ))
        .into());
    }
    let mut entries = Vec::new();
    for (path, result) in merged.merged {
        let entry = match result {
            Merged::Retained(entry) => entry,
            Merged::Content { bytes, executable } => FileEntry::Regular {
                content: repo.put_blob(&bytes)?,
                executable,
            },
        };
        entries.push((path, entry));
    }
    let merged = Manifest::new(entries)?;
    let paths: BTreeSet<_> = theirs
        .entries()
        .keys()
        .chain(merged.entries().keys())
        .collect();
    let changes = SparseChanges::new(paths.into_iter().filter_map(|path| {
        let after = merged.entries().get(path);
        (theirs.entries().get(path) != after).then(|| (path.clone(), after.cloned()))
    }))?;
    Ok((changes, conflicts))
}

fn merge_error(error: javelin_merge::MergeError<Error>) -> CliError {
    match error {
        javelin_merge::MergeError::Load(error) => error.into(),
        other => CliError::Remote(RemoteError::new("merge_limit", other.to_string())),
    }
}

fn kind(kind: ConflictKind) -> &'static str {
    match kind {
        ConflictKind::Content => "content",
        ConflictKind::Binary => "binary",
        ConflictKind::ModifyDelete => "modify_delete",
        ConflictKind::AddAdd => "add_add",
        ConflictKind::EntryType => "entry_type",
        ConflictKind::PathPrefix => "path_prefix",
        ConflictKind::RenameRename => "rename_rename",
        ConflictKind::Alias => "alias",
    }
}

fn active_head(repo: &Repository, layer: &LayerId) -> Result<javelin::LayerHead, CliError> {
    match repo.layer_state(layer)? {
        LayerState::Active { head } => Ok(head),
        _ => Err(Error::InvalidInput(format!("Layer {} is not active", layer.as_str())).into()),
    }
}

/// `ID.2` for `ID`, `ID.3` for `ID.2`, and so on: the first one this repository lacks.
fn next_name(repo: &Repository, old: &LayerId) -> Result<LayerId, CliError> {
    let name = old.as_str();
    let (stem, mut n) = match name.rsplit_once('.') {
        Some((stem, digits))
            if !stem.is_empty()
                && !digits.is_empty()
                && digits.len() < 9
                && digits.bytes().all(|b| b.is_ascii_digit()) =>
        {
            (stem, digits.parse::<u32>().unwrap_or(1) + 1)
        }
        _ => (name, 2),
    };
    loop {
        let candidate = LayerId::parse(&format!("{stem}.{n}")).map_err(|_| {
            Error::InvalidInput(format!(
                "Layer {name} has no room for a successor's name; pass --as NEW"
            ))
        })?;
        if repo.layer_state(&candidate).is_err() {
            return Ok(candidate);
        }
        n += 1;
    }
}

/// An operation ID for one step of an update, the same on every retry.
fn derived(step: &str, layer: &LayerId, from: ObjectId) -> OperationId {
    let seed = format!("javelin-update\0{step}\0{}\0{from}", layer.as_str());
    OperationId::parse(&format!("{step}-{}", ObjectId::for_blob(seed.as_bytes())))
        .expect("a derived operation ID is valid")
}

/// The Layer that holds `layer`'s work now: itself, or the last successor updates made.
pub(crate) fn latest(root: &Path, layer: &LayerId) -> Result<LayerId, CliError> {
    let updates = read_json(root, UPDATES)?;
    let mut current = layer.clone();
    for _ in 0..=updates.len() {
        match updates
            .get(current.as_str())
            .map(decode_update)
            .transpose()?
        {
            Some(update) => current = update.successor,
            None => return Ok(current),
        }
    }
    Err(Error::CorruptRepository(format!("{UPDATES} names a cycle of successors")).into())
}

/// The files an update wrote between markers into `layer` that its head still holds
/// markers in. Read from the head, so the caller captures first.
pub(crate) fn unresolved(
    repo: &Repository,
    root: &Path,
    layer: &LayerId,
) -> Result<Vec<RepoPath>, CliError> {
    let updates = read_json(root, UPDATES)?;
    let marked: Vec<RepoPath> = updates
        .values()
        .filter(|update| update["successor"] == layer.as_str())
        .flat_map(|update| update["marked"].as_array().cloned().unwrap_or_default())
        .filter_map(|path| RepoPath::parse(path.as_str()?).ok())
        .collect();
    if marked.is_empty() {
        return Ok(marked);
    }
    let LayerState::Active { head } = repo.layer_state(layer)? else {
        return Ok(Vec::new());
    };
    let manifest = repo.checkpoint_manifest(head.checkpoint())?;
    let mut left = Vec::new();
    for path in marked {
        if let Some(FileEntry::Regular { content, .. }) = manifest.entries().get(&path) {
            let bytes = repo.read_blob_bounded(*content, Resource::BlobBytes.limit() as usize)?;
            if javelin_merge::has_markers(&bytes) {
                left.push(path);
            }
        }
    }
    Ok(left)
}

pub(crate) fn unresolved_error(layer: &LayerId, paths: &[RepoPath]) -> CliError {
    let names: Vec<_> = paths
        .iter()
        .map(|path| serde_json::to_string(path.as_str()).expect("a path is JSON"))
        .collect();
    CliError::Remote(RemoteError::new(
        UNRESOLVED,
        format!(
            "Layer {} still has conflict markers from `javelin layer update` in {}; edit them \
             out, then retry",
            layer.as_str(),
            names.join(", ")
        ),
    ))
}

/// Retires the Layers here that proposed `requests` the node has published: each request's
/// own Layer, the Layer this clone last revised it from (`requests.json`), and the successors
/// `layer update` moved their work to. The node publishes a copy of the revision, not the
/// author's Layer, so the request's state is what says the work is in the World. Each active
/// Layer's workspace is captured first, so later edits stay in its history, then it is
/// discarded and its directory trashed and reclaimed, as `workspace discard` does. Returns
/// what was retired; a request still open, or a Layer not active here, changes nothing.
pub(crate) fn retire_published(
    link: &std::cell::RefCell<Link>,
    repo: &mut Repository,
    root: &Path,
    requests: &[RequestSummary],
) -> Result<Vec<Value>, CliError> {
    let records = read_json(root, "requests.json")?;
    let mut retired = Vec::new();
    for request in requests
        .iter()
        .filter(|r| r.state == RequestStatus::Published)
    {
        let mut layers = BTreeSet::from([request.layer.clone()]);
        if let Some(layer) = records
            .get(request.request.as_str())
            .and_then(|record| LayerId::parse(record["layer"].as_str()?).ok())
        {
            layers.insert(layer);
        }
        let layers: BTreeSet<LayerId> = layers
            .iter()
            .flat_map(|layer| [Ok(layer.clone()), latest(root, layer)])
            .collect::<Result<_, _>>()?;
        let active: Vec<LayerId> = layers
            .into_iter()
            .filter(|layer| matches!(repo.layer_state(layer), Ok(LayerState::Active { .. })))
            .collect();
        if active.is_empty() {
            continue;
        }
        // Layer IDs are not unique across people, so a Layer counts only when its history
        // holds one of the request's revisions.
        let revisions = revision_changes(link, &request.request)?;
        for layer in active {
            repo.capture_workspace(&layer)?;
            let head = active_head(repo, &layer)?;
            let mut proposed = false;
            for state in repo.layer_history(&layer)? {
                if let LayerState::Active { head } = state {
                    let changes = repo.read_checkpoint(head.checkpoint())?;
                    proposed |= revisions.contains(changes.changes());
                }
            }
            if !proposed {
                continue;
            }
            let later = repo.read_checkpoint(head.checkpoint())?.changes()
                != revisions.last().expect("a proposed revision");
            let operation = derived("retire", &layer, head.checkpoint().object());
            let discarded = repo.discard_workspace(&operation, &head)?;
            if !matches!(discarded.update, LayerUpdate::Applied { .. }) {
                continue;
            }
            retired.push(json!({
                "layer": layer,
                "request": request.request.as_str(),
                "head": head.checkpoint(),
                "later_work": later,
                "trashed": discarded.trashed,
            }));
        }
    }
    Ok(retired)
}

/// The changes of each of `request`'s revisions this token may read, in revision order.
fn revision_changes(
    link: &std::cell::RefCell<Link>,
    request: &RequestId,
) -> Result<Vec<SparseChanges>, CliError> {
    let detail = match crate::cli_remote::call(
        link,
        &Message::ReadReview {
            request: request.clone(),
            comments_from: u32::MAX,
        },
    )
    .map_err(CliError::Remote)?
    {
        Message::ReviewDetail(detail) => detail,
        _ => return Err(CliError::Remote(crate::cli_remote::unexpected())),
    };
    let mut changes = Vec::new();
    for checkpoint in detail.revisions.iter().filter_map(|r| r.checkpoint) {
        let bytes = crate::cli_upload::node_checkpoint(link, checkpoint)?;
        changes.push(Repository::foreign_checkpoint_changes(&bytes)?);
    }
    Ok(changes)
}

/// Text lines for `retire_published`.
pub(crate) fn retired_lines(retired: &[Value]) -> Vec<String> {
    retired
        .iter()
        .map(|record| {
            let layer = record["layer"].as_str().unwrap_or_default();
            let keep = match (record["later_work"].as_bool(), record["head"].as_str()) {
                (Some(true), Some(head)) => format!(
                    " It held work after the published checkpoint: `javelin layer clone NEW \
                     --checkpoint {head}` keeps it."
                ),
                _ => String::new(),
            };
            format!(
                "Layer {layer} was published on the node, so it is retired here: its directory \
                 is trashed and its checkpoints stay in `javelin layer history {layer}`.{keep}"
            )
        })
        .collect()
}

fn encode_update(update: &Update) -> Value {
    json!({
        "successor": update.successor,
        "from": update.from,
        "was": update.was,
        "base": update.base,
        "local": update.local.get(),
    })
}

fn decode_update(value: &Value) -> Result<Update, CliError> {
    let corrupt = || Error::CorruptRepository(format!("{UPDATES}: bad record"));
    Ok(Update {
        successor: LayerId::parse(value["successor"].as_str().ok_or_else(corrupt)?)
            .map_err(|_| corrupt())?,
        from: ObjectId::parse(value["from"].as_str().ok_or_else(corrupt)?)
            .map_err(|_| corrupt())?,
        was: serde_json::from_value(value["was"].clone()).map_err(|_| corrupt())?,
        base: serde_json::from_value(value["base"].clone()).map_err(|_| corrupt())?,
        local: VersionNumber::new(value["local"].as_u64().ok_or_else(corrupt)?)
            .map_err(|_| corrupt())?,
    })
}

/// A JSON object kept under `.javelin/`, empty when the file is missing.
pub(crate) fn read_json(
    root: &Path,
    name: &str,
) -> Result<serde_json::Map<String, Value>, CliError> {
    match fs::read(store(root).join(name)) {
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
    let path = store(root).join(name);
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

fn store(root: &Path) -> PathBuf {
    root.join(".javelin")
}

/// Stops the process here when `JAVELIN_FAILPOINT` names this point, as the library's
/// failpoints do, so tests can crash a command between two durable steps. Debug builds only.
#[cfg(debug_assertions)]
fn failpoint(name: &str) {
    if std::env::var_os("JAVELIN_FAILPOINT").is_some_and(|set| set == name) {
        std::process::exit(86);
    }
}

#[cfg(not(debug_assertions))]
fn failpoint(_: &str) {}
