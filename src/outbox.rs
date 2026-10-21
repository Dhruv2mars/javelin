//! Client outbox: a durable, append-only journal of requests that wait for a node, and the
//! replay engine that delivers them in order. Records are never rewritten. A delivered
//! record is acknowledged by appending the node's reply, so the journal keeps every
//! outcome it has seen. Each record is `length (u32 LE) | SHA-256 of body | body`; a torn
//! or damaged tail is ignored and reported, and its bytes are kept beside the journal
//! before the next record overwrites them. See `docs/online-design.md` sections 4.4 and 8 (U3).

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use sha2::{Digest, Sha256};

use crate::{Error, OperationId, Result};

const MAGIC: &[u8; 8] = b"JVLOUTB1";
const HEADER: usize = 4 + 32;
/// Largest record body. A damaged length cannot make replay allocate without bound, and
/// the bound fits one protocol frame plus its fields.
const MAX_BODY: usize = 32 << 20;
const QUEUED: u8 = 1;
const ACKED: u8 = 2;

/// What a record asks the node to do. Each kind is one wire message, so replay can
/// resend it unchanged under the same operation ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Upload,
    Append,
    Publish,
    Revise,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Upload => "upload",
            Self::Append => "append",
            Self::Publish => "publish",
            Self::Revise => "revise",
        }
    }

    fn tag(self) -> u8 {
        match self {
            Self::Upload => 1,
            Self::Append => 2,
            Self::Publish => 3,
            Self::Revise => 4,
        }
    }

    fn from_tag(tag: u8) -> Option<Self> {
        Some(match tag {
            1 => Self::Upload,
            2 => Self::Append,
            3 => Self::Publish,
            4 => Self::Revise,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub seq: u64,
    pub kind: Kind,
    /// A Layer ID or request ID. Records in one scope replay in journal order.
    pub scope: String,
    pub operation: OperationId,
    /// The encoded request. The outbox does not interpret it.
    pub payload: Vec<u8>,
    /// The node's reply, once the record is acknowledged.
    pub outcome: Option<Vec<u8>>,
}

/// Why the node did not decide a record. The record stays queued in both cases.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Fault {
    /// The node is unreachable, failed, or could not decide (`node_unavailable`).
    NodeUnavailable(String),
    /// The credential has expired and cannot be refreshed (`control_plane_unavailable`).
    ControlPlaneUnavailable(String),
}

impl Fault {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NodeUnavailable(_) => "node_unavailable",
            Self::ControlPlaneUnavailable(_) => "control_plane_unavailable",
        }
    }
}

/// Delivers one record to the node. `Ok` is the node's reply and is recorded as the
/// outcome. `Err` means no decision: replay stops, and the same operation is retried later.
pub trait Transport {
    fn deliver(&mut self, entry: &Entry) -> std::result::Result<Vec<u8>, Fault>;
}

/// A journal tail that was ignored when the journal was opened.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Damage {
    pub offset: u64,
    pub bytes: u64,
    pub reason: &'static str,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct Replay {
    pub delivered: Vec<Delivered>,
    pub stalled: Option<Stall>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct Delivered {
    pub seq: u64,
    pub outcome: Vec<u8>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct Stall {
    pub seq: u64,
    pub fault: Fault,
}

/// The journal path for hosted repository `name` under repository root `root`.
pub fn journal_path(root: impl AsRef<Path>, name: &str) -> Result<PathBuf> {
    if name.is_empty()
        || name.starts_with('.')
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(Error::InvalidInput(
            "outbox name must contain ASCII letters, digits, '-', '_' or '.'".into(),
        ));
    }
    Ok(root
        .as_ref()
        .join(".javelin")
        .join("outbox")
        .join(format!("{name}.journal")))
}

pub struct Outbox {
    path: PathBuf,
    file: File,
    entries: BTreeMap<u64, Entry>,
    next_seq: u64,
    /// End of the last valid record. Appends start here.
    valid_len: u64,
    damage: Option<Damage>,
    repaired: bool,
}

impl Outbox {
    /// Opens or creates the journal for `name`. Reading never changes the file: a
    /// damaged tail is only reported here and moved aside by the next append.
    pub fn open(root: impl AsRef<Path>, name: &str) -> Result<Self> {
        let path = journal_path(root, name)?;
        fs::create_dir_all(path.parent().expect("journal path has a parent"))?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if bytes.len() < MAGIC.len() {
            // Empty, or a header torn by a crash during creation.
            if !MAGIC.starts_with(&bytes) {
                return Err(Error::CorruptRepository(format!(
                    "{} is not an outbox journal",
                    path.display()
                )));
            }
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(MAGIC)?;
            file.sync_data()?;
            bytes = MAGIC.to_vec();
        } else if &bytes[..MAGIC.len()] != MAGIC {
            return Err(Error::CorruptRepository(format!(
                "{} is not an outbox journal",
                path.display()
            )));
        }

        let mut entries = BTreeMap::new();
        let mut offset = MAGIC.len();
        let mut damage = None;
        while offset < bytes.len() {
            let rest = &bytes[offset..];
            let parsed =
                read_frame(rest).and_then(|(body, used)| apply(body, &mut entries).map(|()| used));
            match parsed {
                Ok(used) => offset += used,
                Err(reason) => {
                    damage = Some(Damage {
                        offset: offset as u64,
                        bytes: rest.len() as u64,
                        reason,
                    });
                    break;
                }
            }
        }
        let next_seq = entries.keys().next_back().map_or(1, |seq| seq + 1);
        Ok(Self {
            path,
            file,
            entries,
            next_seq,
            valid_len: offset as u64,
            damage,
            repaired: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The tail ignored at open, if any.
    pub fn damage(&self) -> Option<&Damage> {
        self.damage.as_ref()
    }

    pub fn entry(&self, seq: u64) -> Option<&Entry> {
        self.entries.get(&seq)
    }

    /// Records not yet acknowledged, in journal order.
    pub fn pending(&self) -> impl Iterator<Item = &Entry> {
        self.entries
            .values()
            .filter(|entry| entry.outcome.is_none())
    }

    /// Durably queues one record and returns its sequence number. Returns only after
    /// the record is on disk, so a caller that got `Ok` may drop the request.
    pub fn enqueue(
        &mut self,
        kind: Kind,
        scope: &str,
        operation: &OperationId,
        payload: &[u8],
    ) -> Result<u64> {
        if scope.is_empty() {
            return Err(Error::InvalidInput("outbox scope must not be empty".into()));
        }
        let seq = self.next_seq;
        let mut body = vec![QUEUED];
        body.extend_from_slice(&seq.to_le_bytes());
        body.push(kind.tag());
        put(&mut body, scope.as_bytes());
        put(&mut body, operation.as_str().as_bytes());
        put(&mut body, payload);
        if body.len() > MAX_BODY {
            return Err(Error::InvalidInput(format!(
                "outbox record is {} bytes, over the {MAX_BODY}-byte limit",
                body.len()
            )));
        }
        self.append(&body)?;
        self.entries.insert(
            seq,
            Entry {
                seq,
                kind,
                scope: scope.into(),
                operation: operation.clone(),
                payload: payload.to_vec(),
                outcome: None,
            },
        );
        self.next_seq += 1;
        Ok(seq)
    }

    /// Delivers pending records in journal order until one faults. Each reply is made
    /// durable before the next record is sent. A crash between a reply and its
    /// acknowledgement replays the record, and the node returns the recorded outcome.
    pub fn replay(&mut self, transport: &mut impl Transport) -> Result<Replay> {
        let mut report = Replay::default();
        let due: Vec<u64> = self.pending().map(|entry| entry.seq).collect();
        for seq in due {
            let entry = &self.entries[&seq];
            match transport.deliver(entry) {
                Ok(outcome) => {
                    self.acknowledge(seq, &outcome)?;
                    report.delivered.push(Delivered { seq, outcome });
                }
                Err(fault) => {
                    report.stalled = Some(Stall { seq, fault });
                    break;
                }
            }
        }
        Ok(report)
    }

    fn acknowledge(&mut self, seq: u64, outcome: &[u8]) -> Result<()> {
        let mut body = vec![ACKED];
        body.extend_from_slice(&seq.to_le_bytes());
        put(&mut body, outcome);
        if body.len() > MAX_BODY {
            return Err(Error::InvalidInput(format!(
                "node reply for record {seq} is over the {MAX_BODY}-byte limit"
            )));
        }
        self.append(&body)?;
        if let Some(entry) = self.entries.get_mut(&seq) {
            entry.outcome = Some(outcome.to_vec());
        }
        Ok(())
    }

    fn append(&mut self, body: &[u8]) -> Result<()> {
        if self.damage.is_some() && !self.repaired {
            self.move_damage_aside()?;
        }
        let mut frame = Vec::with_capacity(HEADER + body.len());
        frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
        frame.extend_from_slice(Sha256::digest(body).as_slice());
        frame.extend_from_slice(body);
        self.file.seek(SeekFrom::Start(self.valid_len))?;
        self.file.write_all(&frame)?;
        self.file.sync_data()?;
        self.valid_len += frame.len() as u64;
        Ok(())
    }

    /// Keeps the ignored tail in a sibling file, then truncates the journal to its last
    /// valid record. Without the truncation, a new record would sit behind the damage
    /// and every later open would stop before it.
    fn move_damage_aside(&mut self) -> Result<()> {
        let Some(damage) = &self.damage else {
            return Ok(());
        };
        let mut tail = vec![0; damage.bytes as usize];
        self.file.seek(SeekFrom::Start(damage.offset))?;
        self.file.read_exact(&mut tail)?;
        let mut sibling = self
            .path
            .with_extension(format!("damaged-{}", damage.offset));
        let mut n = 1;
        while sibling.exists() {
            sibling = self
                .path
                .with_extension(format!("damaged-{}-{n}", damage.offset));
            n += 1;
        }
        fs::write(&sibling, &tail)?;
        self.file.set_len(self.valid_len)?;
        self.file.sync_data()?;
        self.repaired = true;
        Ok(())
    }
}

/// Exponential backoff with equal jitter, capped. `jitter` is in `0..=1` from the
/// caller's source of randomness; half the ceiling is always waited. The numbers are for
/// measurement (`docs/online-design.md` D16).
pub fn backoff(attempt: u32, jitter: f64) -> Duration {
    const BASE: Duration = Duration::from_millis(250);
    const CAP: Duration = Duration::from_secs(60);
    let ceiling = BASE.saturating_mul(1 << attempt.min(16)).min(CAP);
    ceiling / 2 + ceiling.mul_f64(jitter.clamp(0.0, 1.0)) / 2
}

/// The body of the first record in `rest`, and the bytes it occupies.
fn read_frame(rest: &[u8]) -> std::result::Result<(&[u8], usize), &'static str> {
    if rest.len() < HEADER {
        return Err("torn_record");
    }
    let length = u32::from_le_bytes(rest[..4].try_into().expect("four bytes")) as usize;
    if length > MAX_BODY {
        return Err("malformed_record");
    }
    if rest.len() < HEADER + length {
        return Err("torn_record");
    }
    let body = &rest[HEADER..HEADER + length];
    if Sha256::digest(body).as_slice() != &rest[4..HEADER] {
        return Err("checksum_mismatch");
    }
    Ok((body, HEADER + length))
}

fn apply(body: &[u8], entries: &mut BTreeMap<u64, Entry>) -> std::result::Result<(), &'static str> {
    let mut reader = Reader(body);
    let malformed = "malformed_record";
    match reader.u8().ok_or(malformed)? {
        QUEUED => {
            let seq = reader.u64().ok_or(malformed)?;
            let kind = Kind::from_tag(reader.u8().ok_or(malformed)?).ok_or(malformed)?;
            let scope = reader.text().ok_or(malformed)?;
            let operation =
                OperationId::parse(reader.text().ok_or(malformed)?).map_err(|_| malformed)?;
            let payload = reader.field().ok_or(malformed)?.to_vec();
            if scope.is_empty() || entries.contains_key(&seq) || !reader.0.is_empty() {
                return Err(malformed);
            }
            entries.insert(
                seq,
                Entry {
                    seq,
                    kind,
                    scope: scope.into(),
                    operation,
                    payload,
                    outcome: None,
                },
            );
        }
        ACKED => {
            let seq = reader.u64().ok_or(malformed)?;
            let outcome = reader.field().ok_or(malformed)?.to_vec();
            let entry = entries.get_mut(&seq).ok_or(malformed)?;
            if entry.outcome.is_some() || !reader.0.is_empty() {
                return Err(malformed);
            }
            entry.outcome = Some(outcome);
        }
        _ => return Err(malformed),
    }
    Ok(())
}

fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn field(&mut self) -> Option<&'a [u8]> {
        let length = u32::from_le_bytes(self.take(4)?.try_into().ok()?) as usize;
        self.take(length)
    }

    fn text(&mut self) -> Option<&'a str> {
        std::str::from_utf8(self.field()?).ok()
    }
}
