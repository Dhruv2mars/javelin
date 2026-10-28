//! Streaming checkpoints, U5's CLI call site (`docs/online-design.md` section 4.2): every
//! capture that advances a Layer of an attached repository queues the Layer's checkpoint for
//! the node in the outbox, then delivers the queue within a bounded budget (D16). A node that
//! is down or slow leaves the records queued for the next capture, `javelin sync` or `javelin
//! outbox replay` (D7); the capture itself never fails or waits longer for it.
//!
//! An append names the node's head as its parent. This clone keeps what it knows of each
//! Layer on the node in `.javelin/uploads.json`: the node's head as the queued appends will
//! leave it ([`Repository::foreign_capture`] names it before the node is reachable), and the
//! journal position it was learned at. The head is read from the node the first time, and
//! again after an append the node did not accept; only then is the writer lease read, so a
//! device that does not hold it queues nothing (`layer_stale`) until `javelin layer take`
//! moves the lease here. Appends already queued when another device took the lease replay
//! into `layer_stale` and stay in the journal, and the local checkpoints stay as they are.
//!
//! A Layer never holds a hidden entry (`docs/architecture.md`, Hidden entries), so an upload
//! names plain content only, at paths this store can see. The node checks that the caller may
//! write every path an append changes, refusing the whole append with `forbidden` otherwise,
//! and serves the uploaded blobs only to readers of their paths (#99). A refused append keeps
//! the work local and is reported; the next capture tries again from the node's head.

use std::{
    cell::RefCell,
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use javelin::{
    Attachment, CheckpointSink, Error, LayerHead, LayerId, LayerState, ObjectId, OperationId,
    RemoteHead, Repository, SparseChanges,
    outbox::{Entry, Fault, Kind as Record, Outbox, Transport},
};
use javelin_protocol::{
    Kind, Message,
    codec::{LayerOutcome, LayerStatus, LeaseHolder, code, decode, encode},
    sender,
};
use serde_json::{Map, Value, json};

use crate::{
    CliError,
    cli_remote::{Link, NodeTransport, RemoteError, Session, parse_target_within, remote},
    cli_request::{Options, Outcome, remote_error},
    cli_upload::{self, create_layer, derived, node_checkpoint},
};

/// Where this clone keeps what it knows of its Layers on the node.
const UPLOADS: &str = "uploads.json";
/// How long a capture spends reaching the node (D16: 2 s, to measure).
const BUDGET: Duration = Duration::from_secs(2);
/// Overrides [`BUDGET`], in milliseconds, for measurement.
const BUDGET_VAR: &str = "JAVELIN_UPLOAD_BUDGET_MS";

/// Makes every capture through `repo` stream to the node, when `root` is attached to one.
pub(crate) fn stream_captures(repo: &mut Repository, root: &Path) -> javelin::Result<()> {
    if let Some(attachment) = repo.attachment()? {
        repo.set_checkpoint_sink(Box::new(Streaming {
            root: root.to_path_buf(),
            attachment,
        }));
    }
    Ok(())
}

/// The capture sink. `Ok` means the node holds the capture; `Err` says, as `status: detail`,
/// why it is still only here: `queued`, `layer_stale`, `local`, or the node's refusal.
struct Streaming {
    root: PathBuf,
    attachment: Attachment,
}

impl CheckpointSink for Streaming {
    fn upload(
        &mut self,
        source: &Repository,
        _: &OperationId,
        before: &LayerHead,
        _: &SparseChanges,
    ) -> std::result::Result<(), String> {
        let budget = std::env::var(BUDGET_VAR)
            .ok()
            .and_then(|ms| ms.parse().ok())
            .map_or(BUDGET, Duration::from_millis);
        let streamed = Node::new(&self.attachment, budget, true).and_then(|mut node| {
            let mut outbox = Outbox::open(&self.root, &self.attachment.repository)?;
            let streamed = stream(&self.root, &mut node, source, &mut outbox, before.layer())?;
            let fault = node.flush(&mut outbox)?;
            settle(&self.root, &outbox, source, before.layer(), streamed, fault)
        });
        match streamed {
            Ok(Streamed::Uploaded) => Ok(()),
            Ok(other) => Err(other.to_string()),
            Err(error) => Err(format!(
                "not_queued: {error}; the next capture or `javelin sync` uploads it"
            )),
        }
    }
}

/// Where one Layer's latest checkpoint is, as far as this clone knows.
#[derive(Debug)]
pub(crate) enum Streamed {
    /// The node accepted it.
    Uploaded,
    /// `pending` appends wait in the outbox, held up by `fault`.
    Queued {
        pending: usize,
        fault: Option<String>,
    },
    /// Nothing is queued: `code` says why, and `detail` what to do.
    Held { code: String, detail: String },
}

impl Streamed {
    fn held(code: &str, detail: String) -> Self {
        Self::Held {
            code: code.into(),
            detail,
        }
    }

    pub(crate) fn status(&self) -> &str {
        match self {
            Self::Uploaded => "uploaded",
            Self::Queued { .. } => "queued",
            Self::Held { code, .. } => code,
        }
    }

    pub(crate) fn value(&self, layer: &LayerId) -> Value {
        let mut value =
            json!({"layer": layer, "status": self.status(), "detail": self.to_string()});
        if let Self::Queued { pending, fault } = self {
            value["queued"] = json!(pending);
            value["fault"] = json!(fault);
        }
        value
    }
}

impl std::fmt::Display for Streamed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Uploaded => write!(f, "uploaded: the node holds the latest checkpoint"),
            Self::Queued { pending, fault } => {
                write!(
                    f,
                    "queued: {pending} checkpoint upload(s) wait in the outbox"
                )?;
                if let Some(fault) = fault {
                    write!(f, " ({fault})")?;
                }
                write!(
                    f,
                    "; the next capture, `javelin sync` or `javelin outbox replay` delivers them"
                )
            }
            Self::Held { code, detail } => write!(f, "{code}: {detail}"),
        }
    }
}

/// The node, reached with a bounded budget. After it fails once, nothing more is sent.
struct Node {
    link: RefCell<Link>,
    transport: NodeTransport,
    until: Option<Instant>,
    down: Option<String>,
}

impl Node {
    /// `bounded` stops sending once `budget` has passed; each request gives up after it.
    fn new(attachment: &Attachment, budget: Duration, bounded: bool) -> Result<Self, CliError> {
        let target = format!("{}/{}", attachment.node, attachment.repository);
        Ok(Self {
            link: Link::writing(parse_target_within(&target, budget)?),
            transport: NodeTransport::within(&attachment.node, &attachment.repository, budget)?,
            until: bounded.then(|| Instant::now() + budget),
            down: None,
        })
    }

    fn online(&self) -> bool {
        self.down.is_none() && self.until.is_none_or(|until| Instant::now() < until)
    }

    /// Notes a failure to reach the node, so later steps go straight to the queue.
    fn failed(&mut self, error: &RemoteError) {
        if error.code == code::NODE_UNAVAILABLE {
            self.down = Some(format!("{}: {}", error.code, error.detail));
        }
    }

    /// Delivers queued uploads and appends in journal order. A queued Publish or revision
    /// stops it: `javelin sync`, `outbox replay` and `request` deliver those and report their
    /// outcomes. Returns why records are still pending, if they are.
    fn flush(&mut self, outbox: &mut Outbox) -> Result<Option<String>, CliError> {
        if outbox.pending().count() == 0 {
            return Ok(None);
        }
        let replay = outbox.replay(self)?;
        Ok(replay
            .stalled
            .map(|stall| format!("{}: {}", stall.fault.code(), stall.fault.detail())))
    }
}

impl Transport for Node {
    fn deliver(&mut self, entry: &Entry) -> std::result::Result<Vec<u8>, Fault> {
        if !matches!(entry.kind, Record::Upload | Record::Append) {
            return Err(Fault::Refused {
                code: "deferred".into(),
                detail: format!(
                    "a queued {} goes first; `javelin sync` delivers it and reports its outcome",
                    entry.kind.as_str()
                ),
            });
        }
        if let Some(down) = &self.down {
            return Err(Fault::NodeUnavailable(down.clone()));
        }
        if !self.online() {
            return Err(Fault::NodeUnavailable(
                "the upload budget is spent; the rest stays queued".into(),
            ));
        }
        let delivered = self.transport.deliver(entry);
        if let Err(Fault::NodeUnavailable(detail)) = &delivered {
            self.down = Some(format!("{}: {detail}", code::NODE_UNAVAILABLE));
        }
        delivered
    }
}

/// Queues the upload of `layer`'s local head, unless the node holds it or will once the
/// queue is delivered. Learns the node's head first when this clone does not know it.
fn stream(
    root: &Path,
    node: &mut Node,
    repo: &Repository,
    outbox: &mut Outbox,
    layer: &LayerId,
) -> Result<Streamed, CliError> {
    let LayerState::Active { head: local } = repo.layer_state(layer)? else {
        return Ok(Streamed::held(
            "local",
            format!("Layer {} is not active", layer.as_str()),
        ));
    };
    let checkpoint = repo.read_checkpoint(local.checkpoint())?;
    let Some(base) = repo.remote_world(checkpoint.base().version())? else {
        return Ok(Streamed::held(
            "local",
            format!(
                "Layer {}'s base is not a World this repository fetched from the node, so it \
                 stays on this device",
                layer.as_str()
            ),
        ));
    };
    let changes = checkpoint.changes();
    let mut uploads = Uploads::load(root)?;
    let known = uploads.get(layer, outbox)?;
    if let Some(stopped) = known.stopped.as_deref().filter(|code| finished(code)) {
        return Ok(Streamed::held(stopped, stopped_detail(stopped, layer)));
    }
    let (head, since) = match known.head {
        Some(head) => (head, known.since),
        None => {
            // Records already queued move the node's head; learn it after they land.
            if let Some(fault) = node.flush(outbox)? {
                return Ok(Streamed::held(
                    "not_queued",
                    format!(
                        "earlier records wait in the outbox ({fault}); Layer {}'s head on the \
                         node is read, and its checkpoint queued, once they are delivered",
                        layer.as_str()
                    ),
                ));
            }
            match learn(node, layer, &base) {
                Ok(head) => (head, last_seq(outbox)),
                Err(Learned::Stale) => {
                    uploads.stop(layer, code::LAYER_STALE, outbox);
                    uploads.save(root)?;
                    return Ok(Streamed::held(
                        code::LAYER_STALE,
                        stopped_detail(code::LAYER_STALE, layer),
                    ));
                }
                Err(Learned::Stopped(code)) => {
                    uploads.stop(layer, &code, outbox);
                    uploads.save(root)?;
                    return Ok(Streamed::held(&code, stopped_detail(&code, layer)));
                }
                Err(Learned::Failed(error)) => {
                    node.failed(&error);
                    let detail = if error.code == code::NODE_UNAVAILABLE {
                        format!(
                            "Layer {} has not reached the node yet ({}); the next capture or \
                             `javelin sync` uploads it",
                            layer.as_str(),
                            error.detail
                        )
                    } else {
                        error.detail
                    };
                    return Ok(Streamed::held(&error.code, detail));
                }
            }
        }
    };
    if Repository::foreign_checkpoint_changes(&head)? != *changes {
        let changes_id = Kind::Checkpoint.id(&changes.canonical_bytes());
        let parent = ObjectId::for_checkpoint(&head);
        let seed = format!(
            "javelin-capture\0{}\0{parent}\0{changes_id}",
            layer.as_str()
        );
        let operation =
            OperationId::parse(&format!("capture-{}", ObjectId::for_blob(seed.as_bytes())))?;
        let link = node.online().then_some(&node.link);
        let mut records = cli_upload::uploads(link, repo, changes, layer, &operation);
        if let Err(CliError::Remote(error)) = &records {
            node.failed(error);
            records = cli_upload::uploads(None, repo, changes, layer, &operation);
        }
        let mut records = records?;
        let append = derived("append", &operation);
        records.push((
            Record::Append,
            layer.as_str().to_owned(),
            append.clone(),
            encode(&Message::LayerAppend {
                operation: append,
                layer: layer.clone(),
                parent: Some(parent),
                checkpoint: changes_id,
            })
            .map_err(remote_error)?,
        ));
        for (kind, scope, operation, payload) in &records {
            outbox.enqueue(*kind, scope, operation, payload)?;
        }
        uploads.set(layer, &Repository::foreign_capture(&head, changes)?, since);
    } else {
        uploads.set(layer, &head, since);
    }
    uploads.save(root)?;
    Ok(Streamed::Queued {
        pending: pending_appends(outbox, layer),
        fault: node.down.clone(),
    })
}

/// What became of `layer`'s queued appends after a delivery: an append the node did not
/// accept means this clone no longer knows the node's head.
fn settle(
    root: &Path,
    outbox: &Outbox,
    repo: &Repository,
    layer: &LayerId,
    streamed: Streamed,
    fault: Option<String>,
) -> Result<Streamed, CliError> {
    if !matches!(streamed, Streamed::Queued { .. }) {
        return Ok(streamed);
    }
    let mut uploads = Uploads::load(root)?;
    let known = uploads.get(layer, outbox)?;
    if let Some(stopped) = known.stopped {
        uploads.save(root)?;
        let mut detail = stopped_detail(&stopped, layer);
        if stopped == code::LAYER_STALE
            && let LayerState::Active { head } = repo.layer_state(layer)?
        {
            detail.push_str(&format!(
                "; keep its work with `javelin layer clone NEW --checkpoint {}`",
                head.checkpoint().object()
            ));
        }
        return Ok(Streamed::held(&stopped, detail));
    }
    Ok(match pending_appends(outbox, layer) {
        0 => Streamed::Uploaded,
        pending => Streamed::Queued { pending, fault },
    })
}

/// Whether uploads of a Layer stopped for good: the node's Layer takes no more appends.
fn finished(code: &str) -> bool {
    code == code::LAYER_PUBLISHED || code == LAYER_DISCARDED
}

const LAYER_DISCARDED: &str = "layer_discarded";

fn stopped_detail(code: &str, layer: &LayerId) -> String {
    match code {
        code::LAYER_STALE => format!(
            "another device holds Layer {0}'s writer lease, so its checkpoints stay on this \
             device and nothing more is uploaded; `javelin layer take {0}` moves the lease here",
            layer.as_str()
        ),
        code if finished(code) => format!(
            "Layer {} is {} on the node, so its checkpoints stay on this device",
            layer.as_str(),
            code.trim_start_matches("layer_")
        ),
        code => format!(
            "the node refused Layer {}'s last upload ({code}); its checkpoints stay on this \
             device, and the next capture tries again from the node's head",
            layer.as_str()
        ),
    }
}

/// Why the node's head could not be learned.
enum Learned {
    /// Another device holds the writer lease.
    Stale,
    /// The node's Layer is published or discarded: nothing more goes to it.
    Stopped(String),
    Failed(RemoteError),
}

/// The node's head checkpoint for `layer`, created over remote World `base` if the node
/// lacks it, when this device may write to it.
fn learn(node: &Node, layer: &LayerId, base: &RemoteHead) -> Result<Vec<u8>, Learned> {
    create_layer(&node.link, layer, base).map_err(Learned::Failed)?;
    let state = sender::layer_head(&mut Session(&node.link), layer)
        .map_err(|e| Learned::Failed(remote(&node.link, e)))?;
    match state.status {
        LayerStatus::Active => {}
        LayerStatus::Published => return Err(Learned::Stopped(code::LAYER_PUBLISHED.into())),
        LayerStatus::Discarded => return Err(Learned::Stopped(LAYER_DISCARDED.into())),
    }
    if state.lease == LeaseHolder::Other {
        return Err(Learned::Stale);
    }
    node_checkpoint(&node.link, state.head).map_err(|error| match error {
        CliError::Remote(error) => Learned::Failed(error),
        other => Learned::Failed(RemoteError::new(other.code(), other.to_string())),
    })
}

/// Pending appends of `layer`: one per checkpoint still to upload.
fn pending_appends(outbox: &Outbox, layer: &LayerId) -> usize {
    outbox
        .pending()
        .filter(|entry| entry.kind == Record::Append && entry.scope == layer.as_str())
        .count()
}

/// The last journal position, so later records can be told from earlier ones.
fn last_seq(outbox: &Outbox) -> u64 {
    outbox.entries().map(|entry| entry.seq).max().unwrap_or(0)
}

/// What this clone knows of one Layer on the node. A stopped Layer has no known head.
#[derive(Default)]
struct Known {
    /// The node's head once the queued appends land, as checkpoint bytes.
    head: Option<Vec<u8>>,
    /// Appends journaled after this position build on `head`.
    since: u64,
    /// Why uploads stopped: `layer_stale`, `layer_published`, `layer_discarded` or a refusal.
    stopped: Option<String>,
}

/// `.javelin/uploads.json`: Layer ID to `{head, since, stopped}`.
pub(crate) struct Uploads(Map<String, Value>);

impl Uploads {
    fn path(root: &Path) -> PathBuf {
        root.join(".javelin").join(UPLOADS)
    }

    pub(crate) fn load(root: &Path) -> Result<Self, CliError> {
        match fs::read(Self::path(root)) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(Value::Object(map)) => Ok(Self(map)),
                _ => {
                    Err(Error::CorruptRepository(format!("{UPLOADS} is not a JSON object")).into())
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self(Map::new())),
            Err(error) => Err(Error::from(error).into()),
        }
    }

    /// What is known of `layer`, after the outcomes `outbox` holds for its appends: one the
    /// node did not accept leaves its head unknown, and stale ones stop uploads.
    fn get(&mut self, layer: &LayerId, outbox: &Outbox) -> Result<Known, CliError> {
        let Some(entry) = self.0.get(layer.as_str()) else {
            return Ok(Known::default());
        };
        let corrupt = || Error::CorruptRepository(format!("{UPLOADS}: bad entry"));
        let mut known = Known {
            head: match &entry["head"] {
                Value::Null => None,
                Value::String(head) => Some(head.as_bytes().to_vec()),
                _ => return Err(corrupt().into()),
            },
            since: entry["since"].as_u64().ok_or_else(corrupt)?,
            stopped: entry["stopped"].as_str().map(str::to_owned),
        };
        if known.head.is_some()
            && let Some(refused) = refused_append(outbox, layer, known.since)
        {
            known.head = None;
            known.stopped = Some(refused);
            self.0.insert(
                layer.as_str().into(),
                json!({"head": null, "since": known.since, "stopped": known.stopped}),
            );
        }
        Ok(known)
    }

    /// Records the node's head for `layer` once the appends after `since` land.
    fn set(&mut self, layer: &LayerId, head: &[u8], since: u64) {
        let head = String::from_utf8_lossy(head);
        self.0.insert(
            layer.as_str().into(),
            json!({"head": head, "since": since, "stopped": null}),
        );
    }

    fn stop(&mut self, layer: &LayerId, code: &str, outbox: &Outbox) {
        self.0.insert(
            layer.as_str().into(),
            json!({"head": null, "since": last_seq(outbox), "stopped": code}),
        );
    }

    /// Writes the file atomically: a temporary file, synced, then renamed.
    pub(crate) fn save(&self, root: &Path) -> Result<(), CliError> {
        let path = Self::path(root);
        let temporary = path.with_extension("json.tmp");
        let write = || -> std::io::Result<()> {
            let mut file = fs::File::create(&temporary)?;
            file.write_all(
                &serde_json::to_vec_pretty(&Value::Object(self.0.clone()))
                    .map_err(std::io::Error::other)?,
            )?;
            file.sync_all()?;
            fs::rename(&temporary, &path)
        };
        write().map_err(|error| CliError::from(Error::from(error)))
    }
}

/// The code of the first append of `layer` after `since` that the node did not accept.
fn refused_append(outbox: &Outbox, layer: &LayerId, since: u64) -> Option<String> {
    outbox
        .entries()
        .filter(|entry| {
            entry.seq > since && entry.kind == Record::Append && entry.scope == layer.as_str()
        })
        .find_map(|entry| match decode(entry.outcome.as_deref()?) {
            Ok(Message::LayerOutcome(LayerOutcome::Stale { .. })) => {
                Some(code::LAYER_STALE.to_owned())
            }
            Ok(Message::LayerOutcome(LayerOutcome::Refused { code }))
            | Ok(Message::Error { code, .. }) => Some(code),
            _ => None,
        })
}

/// Records that the node's head for `layer` is `head` now, as `layer take`, `layer refresh`
/// and `request open` and `revise` see it, so the next capture appends after it. Nothing
/// is lost if this cannot be written: the next append is refused, and the head relearned.
pub(crate) fn remember(root: &Path, attachment: &Attachment, layer: &LayerId, head: &[u8]) {
    // The open outbox keeps another command from writing the file at the same time.
    if let Ok(outbox) = Outbox::open(root, &attachment.repository) {
        let _ = remember_in(root, last_seq(&outbox), layer, head);
    }
}

/// As [`remember`], for a caller holding the outbox: appends journaled after `since` build on
/// `head`.
pub(crate) fn remember_in(
    root: &Path,
    since: u64,
    layer: &LayerId,
    head: &[u8],
) -> Result<(), CliError> {
    let mut uploads = Uploads::load(root)?;
    uploads.set(layer, head, since);
    uploads.save(root)
}

/// The node's head for `layer` as this clone expects it, for a revision queued offline.
pub(crate) fn expected_head(root: &Path, outbox: &Outbox, layer: &LayerId) -> Option<Vec<u8>> {
    Uploads::load(root).ok()?.get(layer, outbox).ok()?.head
}

/// Queues and delivers the upload of every active Layer whose local head the node lacks, as
/// a capture would, for `javelin sync` after its own replay.
pub(crate) fn catch_up(
    root: &Path,
    attachment: &Attachment,
    repo: &Repository,
    outbox: &mut Outbox,
) -> Result<Vec<Value>, CliError> {
    let mut node = Node::new(attachment, crate::cli_remote::TIMEOUT, false)?;
    let mut report = Vec::new();
    for layer in repo.layers()? {
        let LayerState::Active { head } = repo.layer_state(&layer)? else {
            continue;
        };
        let checkpoint = repo.read_checkpoint(head.checkpoint())?;
        if repo.remote_world(checkpoint.base().version())?.is_none() {
            continue;
        }
        let known = Uploads::load(root)?.get(&layer, outbox)?;
        let current = known.head.as_deref().is_some_and(|head| {
            Repository::foreign_checkpoint_changes(head).ok().as_ref() == Some(checkpoint.changes())
        });
        // An empty Layer the node never saw has nothing to upload.
        if current || (known.head.is_none() && checkpoint.changes().entries().is_empty()) {
            continue;
        }
        let streamed = stream(root, &mut node, repo, outbox, &layer)?;
        let fault = node.flush(outbox)?;
        report.push(settle(root, outbox, repo, &layer, streamed, fault)?.value(&layer));
    }
    Ok(report)
}

/// `javelin status [--repo DIR]`: where each Layer's checkpoints are, read locally: all on
/// the node, how many are queued, or why they stay on this device.
pub fn status(args: Vec<OsString>) -> Outcome {
    let usage = "javelin status [--repo DIR] [--json]";
    let options = Options::parse(args, &["--repo"], usage)?;
    if !options.positionals.is_empty() {
        return Err(Error::InvalidInput(format!("expected: {usage}")).into());
    }
    let root = options.root()?;
    let repo = Repository::open(&root)?;
    let attachment = repo.attachment()?;
    let outbox = match &attachment {
        Some(attachment) => Some(Outbox::open(&root, &attachment.repository)?),
        None => None,
    };
    let mut uploads = Uploads::load(&root)?;
    let mut layers = Vec::new();
    let mut lines = vec![match &attachment {
        Some(attachment) => format!(
            "Attached to {} at {} (remote World {}); {} records pending in the outbox.",
            attachment.repository,
            attachment.node,
            attachment.head.number,
            outbox.as_ref().map_or(0, |outbox| outbox.pending().count())
        ),
        None => "Not attached to a node: every Layer stays on this device.".into(),
    }];
    for layer in repo.layers()? {
        let LayerState::Active { head } = repo.layer_state(&layer)? else {
            continue;
        };
        let checkpoint = repo.read_checkpoint(head.checkpoint())?;
        let streamed = match (&outbox, repo.remote_world(checkpoint.base().version())?) {
            (Some(outbox), Some(_)) => {
                let known = uploads.get(&layer, outbox)?;
                let pending = pending_appends(outbox, &layer);
                match known.stopped {
                    Some(code) => Streamed::held(&code, stopped_detail(&code, &layer)),
                    None if pending > 0 => Streamed::Queued {
                        pending,
                        fault: None,
                    },
                    None if known.head.as_deref().is_some_and(|node| {
                        Repository::foreign_checkpoint_changes(node).ok().as_ref()
                            == Some(checkpoint.changes())
                    }) =>
                    {
                        Streamed::Uploaded
                    }
                    None => Streamed::held(
                        "not_uploaded",
                        "the node does not hold the latest checkpoint; the next capture or \
                         `javelin sync` uploads it"
                            .into(),
                    ),
                }
            }
            _ => Streamed::held(
                "local",
                "its base is not a World fetched from the node, so it stays on this device".into(),
            ),
        };
        lines.push(format!(
            "Layer {} (revision {}): {streamed}",
            layer.as_str(),
            head.revision()
        ));
        let mut value = streamed.value(&layer);
        value["revision"] = json!(head.revision());
        value["checkpoint"] = json!(head.checkpoint());
        layers.push(value);
    }
    let value = json!({
        "ok": true,
        "command": "status",
        "attachment": attachment,
        "outbox": {"pending": outbox.as_ref().map_or(0, |outbox| outbox.pending().count())},
        "layers": layers,
    });
    Ok((value, lines.join("\n"), 0))
}
