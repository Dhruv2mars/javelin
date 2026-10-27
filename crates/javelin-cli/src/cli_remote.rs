//! `javelin attach`: fetches a hosted repository's head into the local store over the
//! node's HTTP transport, one protocol frame per request, then records the attachment. The
//! transport is HTTPS to any host, and plain HTTP only to this machine (`Endpoint`), so a
//! token never crosses a network in clear. `javelin_protocol::fetch` runs unchanged on top
//! of `Link`. See `docs/online-design.md`
//! section 4.1 (U4). `javelin layer refresh` reads a Layer's head from the attached node
//! over the same transport (section 4.3, U6).
//!
//! Every request carries the credential from `cli_auth` as `Authorization: Bearer`. A read
//! goes out anonymously when no token can be had, so a public repository stays readable with
//! the control plane down (D24). `javelin outbox replay` delivers the outbox's queued records
//! through `NodeTransport`, which never sends a write without a credential. A node that does
//! not answer, or answers 5xx, is `node_unavailable` (section 4.4).

use std::{
    cell::RefCell,
    ffi::OsString,
    io::{self, Cursor, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use javelin::{
    AdvanceOutcome, Attachment, Error, LayerId, LayerState, ObjectId, OperationId, RemoteHead,
    Repository, Result, VersionNumber,
    outbox::{Entry, Fault, Outbox, Transport},
    tree::MemoryStore,
};
use javelin_control::Endpoint;
use javelin_protocol::{
    Message, ProtocolError,
    codec::{
        LayerOutcome, LeaseHolder, PublishOutcome, VERSION, code, decode, encode, read_frame,
        write_frame,
    },
    fetch::{Anchor, Blobs, fetch},
    sender,
};
use javelin_stack::remote::{self as stack_remote, Refresh};
use serde_json::{Value, json};

use crate::CliError;
use crate::cli::Output;
use crate::cli_auth::{Credential, now};

const PROVIDER: &str = "self-hosted";
/// Lock file inside a staging directory, held by the attach that owns it.
const STAGING_LOCK: &str = "attach.lock";
const TIMEOUT: Duration = Duration::from_secs(30);
/// One framed message.
const MAX_REPLY: u64 = javelin_protocol::limits::MAX_FRAME as u64;

/// A failure with a stable code, reported as the error code in output.
#[derive(Clone, Debug)]
pub struct RemoteError {
    pub code: String,
    pub detail: String,
}

impl RemoteError {
    fn new(code: &str, detail: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            detail: detail.into(),
        }
    }
}

impl From<ProtocolError> for RemoteError {
    fn from(error: ProtocolError) -> Self {
        Self {
            code: error.code,
            detail: error.detail,
        }
    }
}

struct Target {
    /// `https://HOST[:PORT]` (plain `http` only for this machine), as stored in `attach.json`.
    node: String,
    repository: String,
    agent: ureq::Agent,
}

fn parse_target(text: &str) -> Result<Target> {
    let usage =
        "expected a node URL https://HOST[:PORT]/REPOSITORY (plain http only for localhost)";
    let (scheme, rest) = text
        .split_once("://")
        .ok_or_else(|| Error::InvalidInput(usage.into()))?;
    let (authority, repository) = rest
        .split_once('/')
        .ok_or_else(|| Error::InvalidInput(usage.into()))?;
    let valid_name = !repository.is_empty()
        && repository
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
    if !valid_name {
        return Err(Error::InvalidInput(usage.into()));
    }
    let endpoint = Endpoint::parse(&format!("{scheme}://{authority}"))
        .map_err(|detail| Error::InvalidInput(format!("node URL: {detail}")))?;
    Ok(Target {
        node: endpoint.origin().into(),
        repository: repository.into(),
        agent: endpoint.agent(TIMEOUT),
    })
}

/// The transport state shared by both halves of one protocol session.
struct Link {
    target: Target,
    credential: Credential,
    /// Whether a request may go out without a token. Reads may, so a public repository stays
    /// readable signed out; a write never does.
    anonymous: bool,
    /// Why no token could be had, when requests went out anonymously instead.
    credential_fault: Option<Fault>,
    /// The frame the protocol is writing. It is sent when its reply is first read.
    outbound: Vec<u8>,
    inbox: Cursor<Vec<u8>>,
    /// The transport failure behind a protocol error, if any.
    failure: Option<RemoteError>,
}

impl Link {
    /// A session for reads, which go out anonymously when no token can be had.
    fn new(target: Target) -> RefCell<Self> {
        Self::with(target, true)
    }

    /// A session for writes, which are refused before sending when no token can be had.
    fn writing(target: Target) -> RefCell<Self> {
        Self::with(target, false)
    }

    fn with(target: Target, anonymous: bool) -> RefCell<Self> {
        let credential = Credential::for_repository(&target.repository);
        RefCell::new(Self {
            target,
            credential,
            anonymous,
            credential_fault: None,
            outbound: Vec::new(),
            inbox: Cursor::new(Vec::new()),
            failure: None,
        })
    }

    /// Sends one frame and returns the reply, framed the same way. The node frames each
    /// request by `Content-Length`, so the frame's own length prefix is dropped.
    fn post(&mut self, frame: &[u8]) -> io::Result<Vec<u8>> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "malformed outbound frame");
        let (prefix, body) = frame.split_first_chunk::<4>().ok_or_else(invalid)?;
        if u32::from_be_bytes(*prefix) as usize != body.len() {
            return Err(invalid());
        }
        // Each POST is a session of one message, so there is nothing to negotiate with the
        // node. The `/v2` path already names the version: answer `Hello` here.
        if let Ok(Message::Hello { min, max }) = decode(body) {
            let answer = if (min..=max).contains(&VERSION) {
                Message::HelloOk { version: VERSION }
            } else {
                Message::Error {
                    code: "unsupported_version".into(),
                    detail: format!("supported versions: {VERSION}..={VERSION}"),
                }
            };
            return Ok(framed(
                &encode(&answer).map_err(|e| io::Error::other(e.detail))?,
            ));
        }
        match self.round_trip(body) {
            Ok(reply) => Ok(framed(&reply)),
            Err(error) => {
                let detail = error.detail.clone();
                self.failure.get_or_insert(error);
                Err(io::Error::other(detail))
            }
        }
    }

    /// One read request. Without a token it goes out anonymously; if the node then refuses
    /// the caller, a fresh token is fetched once and the request repeats, which is safe for a
    /// read. When no token could be had, that reason is the error, not the node's refusal.
    fn round_trip(&mut self, body: &[u8]) -> std::result::Result<Vec<u8>, RemoteError> {
        let now = now();
        let token = match self.credential.bearer(now) {
            Ok(None) if !self.anonymous => {
                return Err(RemoteError::new(
                    code::UNAUTHORIZED,
                    "not signed in; run `javelin login`, or set JAVELIN_TOKEN",
                ));
            }
            Err(fault) if !self.anonymous => return Err(fault_error(&fault)),
            Ok(token) => token,
            Err(fault) => {
                self.credential_fault = Some(fault);
                None
            }
        };
        let reply = post(&self.target, body, token.as_deref(), true)?;
        if refused_caller(&reply).is_none() {
            return Ok(reply);
        }
        if let Some(fault) = self.credential_fault.take() {
            return Err(fault_error(&fault));
        }
        let token = self
            .credential
            .renew(now)
            .map_err(|fault| fault_error(&fault))?;
        post(&self.target, body, Some(&token), true)
    }
}

/// Sends one message body to the node and returns the reply body. `retry_reset` repeats the
/// request once on a reset connection, which only a read may do: Windows can reset a
/// connection the node has just answered.
fn post(
    target: &Target,
    body: &[u8],
    token: Option<&str>,
    retry_reset: bool,
) -> std::result::Result<Vec<u8>, RemoteError> {
    let mut reply = match exchange(target, body, token) {
        Err(ureq::Error::Io(error)) if retry_reset && is_reset(&error) => {
            exchange(target, body, token)
        }
        done => done,
    }
    .map_err(|e| RemoteError::new(code::NODE_UNAVAILABLE, e.to_string()))?;
    let status = reply.status().as_u16();
    // Bounded, so a node cannot make the client buffer without limit.
    let bytes = match reply
        .body_mut()
        .with_config()
        .limit(MAX_REPLY)
        .read_to_vec()
    {
        Ok(bytes) => bytes,
        Err(ureq::Error::BodyExceedsLimit(_)) => {
            return Err(RemoteError::new(
                "limit_exceeded",
                "the node's reply is larger than one frame",
            ));
        }
        Err(error) => return Err(RemoteError::new(code::NODE_UNAVAILABLE, error.to_string())),
    };
    match status {
        200 => Ok(bytes),
        404 => Err(RemoteError::new(
            "unknown_repository",
            format!("{} has no repository {}", target.node, target.repository),
        )),
        // The node failed or is overloaded; it did not decide (section 4.4).
        500..=599 => Err(RemoteError::new(
            code::NODE_UNAVAILABLE,
            format!("the node answered HTTP {status}"),
        )),
        other => Err(RemoteError::new(
            "node_error",
            format!("the node answered HTTP {other}"),
        )),
    }
}

fn exchange(
    target: &Target,
    body: &[u8],
    token: Option<&str>,
) -> std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    let mut request = target
        .agent
        .post(format!("{}/v2/r/{}", target.node, target.repository))
        .header("content-type", "application/octet-stream");
    if let Some(token) = token {
        request = request.header("authorization", &format!("Bearer {token}"));
    }
    request.send(body)
}

/// The node's refusal of the caller's identity, if the reply is one. A missing, expired or
/// revoked token gets this; a token without the right role or scope gets `forbidden`.
fn refused_caller(reply: &[u8]) -> Option<String> {
    match decode(reply) {
        Ok(Message::Error { code, detail }) if code == code::UNAUTHORIZED => Some(detail),
        _ => None,
    }
}

fn fault_error(fault: &Fault) -> RemoteError {
    RemoteError::new(fault.code(), fault.detail())
}

/// A message body with its length prefix, as `read_frame` expects it.
fn framed(body: &[u8]) -> Vec<u8> {
    let mut framed = (body.len() as u32).to_be_bytes().to_vec();
    framed.extend_from_slice(body);
    framed
}

/// The write half of a protocol session.
struct Up<'a>(&'a RefCell<Link>);

impl Write for Up<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().outbound.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The read half of a protocol session. A read with no reply left sends the buffered
/// frame first, so a reply is never read before its request has gone out.
struct Down<'a>(&'a RefCell<Link>);

impl Read for Down<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut link = self.0.borrow_mut();
        if link.inbox.position() as usize >= link.inbox.get_ref().len() {
            if link.outbound.is_empty() {
                return Ok(0);
            }
            let frame = std::mem::take(&mut link.outbound);
            link.inbox = Cursor::new(link.post(&frame)?);
        }
        link.inbox.read(buf)
    }
}

/// The transport's own failure, when there was one, is more specific than the protocol
/// error it caused.
fn remote(link: &RefCell<Link>, error: ProtocolError) -> RemoteError {
    link.borrow()
        .failure
        .clone()
        .unwrap_or_else(|| error.into())
}

/// One request and its reply. A node's `Error` reply becomes the returned error.
fn call(link: &RefCell<Link>, message: &Message) -> std::result::Result<Message, RemoteError> {
    write_frame(&mut Up(link), message).map_err(|e| remote(link, e))?;
    match read_frame(&mut Down(link)) {
        Ok(Some(Message::Error { code, detail })) => Err(RemoteError { code, detail }),
        Ok(Some(reply)) => Ok(reply),
        Ok(None) => Err(RemoteError::new(
            "truncated",
            "the node closed without replying",
        )),
        Err(e) => Err(remote(link, e)),
    }
}

fn unexpected() -> RemoteError {
    RemoteError::new("unexpected_message", "unexpected reply from the node")
}

/// The node's current head, from `Hello` and `Resolve`. Writes nothing locally.
/// A connection the peer reset or dropped; the request may be sent again.
fn is_reset(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
    )
}

fn resolve(link: &RefCell<Link>) -> std::result::Result<RemoteHead, RemoteError> {
    match call(
        link,
        &Message::Hello {
            min: VERSION,
            max: VERSION,
        },
    )? {
        Message::HelloOk { version } if version == VERSION => {}
        _ => return Err(unexpected()),
    }
    match call(link, &Message::Resolve { version: None })? {
        Message::Resolved { number, id, tree } => Ok(RemoteHead {
            number: number.get(),
            id,
            tree,
        }),
        _ => Err(unexpected()),
    }
}

/// Blobs go straight into the local store, which keys them by content.
struct RepoBlobs<'a>(&'a mut Repository);

impl Blobs for RepoBlobs<'_> {
    fn has(&self, id: ObjectId) -> bool {
        self.0.has_blob(id).unwrap_or(false)
    }
    fn put(&mut self, id: ObjectId, bytes: Vec<u8>) -> std::result::Result<(), ProtocolError> {
        let stored = self.0.put_blob(&bytes)?;
        if stored != id {
            return Err(ProtocolError::new(
                "id_mismatch",
                format!("blob {id} has ID {stored}"),
            ));
        }
        Ok(())
    }
}

/// `javelin attach https://HOST[:PORT]/REPOSITORY [project-directory]`
pub fn attach(args: Vec<OsString>) -> std::result::Result<Output, CliError> {
    let usage =
        "expected: javelin attach https://HOST[:PORT]/REPOSITORY [project-directory] [--json]";
    let mut args = args.into_iter();
    let url = args
        .next()
        .ok_or_else(|| Error::InvalidInput(usage.into()))?;
    let root = match args.next() {
        Some(path) => PathBuf::from(path),
        None => std::env::current_dir().map_err(Error::from)?,
    };
    if args.next().is_some() {
        return Err(Error::InvalidInput(usage.into()).into());
    }
    let url = url
        .to_str()
        .ok_or_else(|| Error::InvalidInput("the node URL must be UTF-8".into()))?;
    let target = parse_target(url)?;
    let node = target.node.clone();
    let repository_name = target.repository.clone();

    // Nothing local is written until the node has answered.
    let link = Link::new(target);
    let head = resolve(&link).map_err(CliError::Remote)?;

    let opened = match Repository::open(&root) {
        Ok(repo) => Some(repo),
        Err(Error::NotInitialized(_)) => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(attached) = opened
        .as_ref()
        .map(|repo| repo.attachment())
        .transpose()?
        .flatten()
    {
        if attached.node != node || attached.repository != repository_name {
            return Err(Error::InvalidInput(format!(
                "this repository is attached to {} {}",
                attached.node, attached.repository
            ))
            .into());
        }
        return Ok(already_attached(&attached, &head));
    }
    // An existing store is attached in place. Its fetch only adds content-addressed objects,
    // and its World moves only through the checked advance, so nothing is deleted on failure.
    if let Some(repo) = opened {
        let (mut repo, fetched) = stage(repo, &link, &head)?;
        return complete(&mut repo, &fetched, &node, &repository_name);
    }

    // A new store is built and completed in a private directory, then installed by a rename
    // that never replaces anything. The installed store is therefore never partial: a failure
    // before the rename leaves no World history at the project directory, so a retry sees an
    // empty repository even if the remote has moved on.
    // An unopenable `.javelin` is an interrupted `init`. The rename below would fail after the
    // whole fetch, so refuse it up front.
    if root.join(".javelin").exists() {
        return Err(Error::InvalidInput(
            "this directory has an incomplete .javelin; remove it, then attach".into(),
        )
        .into());
    }
    sweep_staging(&root);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let staging = root.join(format!(".javelin-attach-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&staging).map_err(Error::from)?;
    // Held until the staging is removed, so a concurrent attach's sweep leaves it alone.
    let lock = std::fs::File::create(staging.join(STAGING_LOCK)).map_err(Error::from)?;
    lock.lock().map_err(Error::from)?;
    let built = Repository::init(&staging)
        .map_err(CliError::from)
        .and_then(|repo| stage(repo, &link, &head))
        .and_then(|(mut repo, fetched)| {
            let output = complete(&mut repo, &fetched, &node, &repository_name)?;
            drop(repo);
            rename_new(&staging.join(".javelin"), &root.join(".javelin")).map_err(|e| {
                match e.kind() {
                    io::ErrorKind::AlreadyExists => CliError::from(Error::InvalidInput(
                        "a store appeared in this directory during attach; nothing was changed"
                            .into(),
                    )),
                    _ => CliError::from(Error::from(e)),
                }
            })?;
            Ok(output)
        });
    // Released before removal: Windows cannot delete a file that is still open.
    drop(lock);
    let cleanup = remove_staging(&staging);
    match (built, cleanup) {
        (Ok(output), Ok(())) => Ok(output),
        (Ok(_), Err(e)) => Err(Error::InvalidInput(format!(
            "attached, but could not remove {}: {e}",
            staging.display()
        ))
        .into()),
        // The original failure is the one to report; the private directory holds only
        // objects this call fetched.
        (Err(error), _) => Err(error),
    }
}

/// Removes staging directories that an attach killed mid-way left in the project directory.
/// They hold only objects fetched by that attach, never a store, so removing them is safe.
/// A live attach holds its directory's lock, so only a killed attach's directory is free to
/// remove. Best effort: a directory that cannot be removed is left for the next attach.
fn sweep_staging(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if !is_staging_name(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let path = entry.path();
        let Ok(lock) = std::fs::File::open(path.join(STAGING_LOCK)) else {
            continue;
        };
        if lock.try_lock().is_err() {
            continue;
        }
        drop(lock);
        let _ = remove_staging(&path);
    }
}

/// Whether `name` is a staging directory this module generates: `.javelin-attach-<pid>-<nanos>`.
fn is_staging_name(name: &str) -> bool {
    let Some((pid, nanos)) = name
        .strip_prefix(".javelin-attach-")
        .and_then(|rest| rest.split_once('-'))
    else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(pid) && digits(nanos)
}

/// Removes a private staging directory. A warmed rolling base is sealed (mode 000), so its
/// root is opened first: the directory only ever holds what this call built.
fn remove_staging(staging: &Path) -> io::Result<()> {
    let base = staging.join(".javelin").join("bases").join("rolling");
    if base.try_exists()? {
        open_for_removal(&base)?;
    }
    std::fs::remove_dir_all(staging)
}

#[cfg(unix)]
fn open_for_removal(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn open_for_removal(_: &Path) -> io::Result<()> {
    Ok(())
}

/// Checks, fetches and imports `head` into `repo`, then advances World 0 to it. Nothing is
/// attached yet: the rolling base and `attach.json` come from [`complete`].
fn stage(
    mut repo: Repository,
    link: &RefCell<Link>,
    head: &RemoteHead,
) -> std::result::Result<(Repository, RemoteHead), CliError> {
    repo.check_attach()?;

    // Pinned to the number resolved above: World versions are immutable, so the fetch
    // reads exactly the head this attach reports.
    let trees = MemoryStore::default();
    let fetched = {
        let mut blobs = RepoBlobs(&mut repo);
        let version = Some(VersionNumber::new(head.number)?);
        fetch(
            Down(link),
            Up(link),
            &trees,
            &mut blobs,
            None,
            version,
            None,
        )
        .map_err(|e| CliError::Remote(remote(link, e)))?
    };
    let fetched_head = RemoteHead {
        number: fetched.number.get(),
        id: fetched.id,
        tree: fetched.tree,
    };
    repo.import_tree(&trees, fetched.tree)?;
    repo.attach_head(&fetched_head)?;
    Ok((repo, fetched_head))
}

/// Warms the rolling base, then records the attachment. `attach.json` is written last.
fn complete(
    repo: &mut Repository,
    fetched: &RemoteHead,
    node: &str,
    repository_name: &str,
) -> std::result::Result<Output, CliError> {
    let base = repo.warm_base()?;
    let attachment = Attachment {
        node: node.into(),
        repository: repository_name.into(),
        provider: PROVIDER.into(),
        head: fetched.clone(),
    };
    repo.write_attachment(&attachment)?;

    let local = repo.head()?;
    let value = json!({
        "ok": true,
        "command": "attach",
        "attached": true,
        "node": node,
        "repository": repository_name,
        "head": fetched,
        "local_head": local,
        "base": base,
    });
    let text = format!(
        "Attached {repository_name} at {node}: remote World {} (tree {}). Rolling base holds {} paths.",
        fetched.number, fetched.tree, base.changed_paths
    );
    Ok((value, text, 0))
}

/// Renames `from` to `to`, failing with `AlreadyExists` rather than replacing `to`. The
/// check and the rename are one system call, so no other process can create `to` between.
#[cfg(target_os = "linux")]
fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::{c_char, c_int, c_uint};

    const AT_FDCWD: c_int = -100;
    const RENAME_NOREPLACE: c_uint = 1;
    unsafe extern "C" {
        fn renameat2(
            olddirfd: c_int,
            old: *const c_char,
            newdirfd: c_int,
            new: *const c_char,
            flags: c_uint,
        ) -> c_int;
    }
    let from = c_path(from)?;
    let to = c_path(to)?;
    let renamed = unsafe {
        renameat2(
            AT_FDCWD,
            from.as_ptr(),
            AT_FDCWD,
            to.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if renamed == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::{c_char, c_int, c_uint};

    const RENAME_EXCL: c_uint = 0x0000_0004;
    unsafe extern "C" {
        fn renamex_np(from: *const c_char, to: *const c_char, flags: c_uint) -> c_int;
    }
    let from = c_path(from)?;
    let to = c_path(to)?;
    if unsafe { renamex_np(from.as_ptr(), to.as_ptr(), RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let wide = |path: &Path| -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let (from, to) = (wide(from), wide(to));
    // Without MOVEFILE_REPLACE_EXISTING, an existing destination fails the move.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn rename_new(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "attach needs a rename that does not replace on this platform",
    ))
}

/// A path as a NUL-terminated C string, for the platform rename calls.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

fn already_attached(attached: &Attachment, head: &RemoteHead) -> Output {
    let value = json!({
        "ok": true,
        "command": "attach",
        "attached": false,
        "already_attached": true,
        "node": attached.node,
        "repository": attached.repository,
        "attached_head": attached.head,
        "remote_head": head,
    });
    let text = format!(
        "Already attached to {} at {}: attached at World {}, remote head is World {}. Nothing changed.",
        attached.repository, attached.node, attached.head.number, head.number
    );
    (value, text, 0)
}

/// Both halves of one session, for protocol code that reads and writes one link.
struct Session<'a>(&'a RefCell<Link>);

impl Write for Session<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Up(self.0).write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for Session<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Down(self.0).read(buf)
    }
}

/// `javelin layer refresh ID [--repo DIR]`: brings the Layer to the head the attached node
/// holds for it, such as one a Stack on the node produced, then brings its workspace there
/// too. The directory is not captured first: its edits are kept by the workspace refresh,
/// which plans against its sync record. Every request is a read, so a retry is safe.
pub fn layer_refresh(args: Vec<OsString>) -> std::result::Result<Output, CliError> {
    let usage = "expected: javelin layer refresh ID [--repo DIR] [--json]";
    let mut args = args.into_iter();
    let mut layer = None;
    let mut root = None;
    while let Some(arg) = args.next() {
        if arg == "--repo" && root.is_none() {
            root = Some(PathBuf::from(
                args.next()
                    .ok_or_else(|| Error::InvalidInput(usage.into()))?,
            ));
        } else if layer.is_none() && !arg.to_string_lossy().starts_with('-') {
            let id = arg
                .to_str()
                .ok_or_else(|| Error::InvalidInput("Layer IDs must be UTF-8".into()))?;
            layer = Some(LayerId::parse(id)?);
        } else {
            return Err(Error::InvalidInput(usage.into()).into());
        }
    }
    let layer = layer.ok_or_else(|| Error::InvalidInput(usage.into()))?;
    let root = match root {
        Some(root) => root,
        None => std::env::current_dir().map_err(Error::from)?,
    };
    let mut repo = Repository::open(&root)?;
    let attached = repo.attachment()?.ok_or_else(|| {
        Error::InvalidInput("this repository is not attached to a node; attach first".into())
    })?;
    let link = Link::new(parse_target(&format!(
        "{}/{}",
        attached.node, attached.repository
    ))?);
    let refresh = stack_remote::refresh(&mut Session(&link), &mut repo, &layer)
        .map_err(|e| CliError::Remote(remote(&link, e)))?;
    let (status, seq, node_head) = match &refresh {
        Refresh::Current { seq, head } => ("current", *seq, *head),
        Refresh::Imported { seq, head, .. } => ("imported", *seq, *head),
    };
    let head = match repo.layer_state(&layer)? {
        LayerState::Active { head } => head,
        _ => return Err(Error::InvalidInput("the Layer is no longer active".into()).into()),
    };
    let workspace = match repo
        .workspace_root(&layer)?
        .try_exists()
        .map_err(Error::from)?
    {
        true => Some(repo.refresh_workspace(&layer)?),
        false => None,
    };
    let mut text = match &refresh {
        Refresh::Current { .. } => format!(
            "Layer {} already holds the node's head (seq {seq})",
            layer.as_str()
        ),
        Refresh::Imported { .. } => format!(
            "Layer {} moved to the node's head {node_head} (seq {seq}) as revision {}",
            layer.as_str(),
            head.revision()
        ),
    };
    if let Some(report) = &workspace {
        text.push_str(&format!(
            "\nWorkspace: {} paths applied, {} local edits kept",
            report.applied, report.kept
        ));
    }
    let value = json!({
        "ok": true,
        "command": "layer refresh",
        "layer": layer,
        "status": status,
        "node": {"seq": seq, "head": node_head},
        "head": head,
        "workspace": workspace,
    });
    Ok((value, text, 0))
}

/// `javelin layer take ID [--operation OP] [--repo DIR]`: moves the Layer's writer lease on
/// the attached node to this device, then brings the Layer there: a Layer this repository
/// lacks is created over the node's base, its head moves to the node's (as `layer refresh`
/// does), and its clone-backed directory is opened, or refreshed when it exists. Only the
/// Layer's owner can take the lease. The take is recorded under OP; without one, a fresh ID
/// is used, which is safe because taking a lease this device holds changes nothing.
pub fn layer_take(args: Vec<OsString>) -> std::result::Result<Output, CliError> {
    let usage = "expected: javelin layer take ID [--operation OP] [--repo DIR] [--json]";
    let mut args = args.into_iter();
    let (mut layer, mut operation, mut root) = (None, None, None);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| Error::InvalidInput(usage.into()));
        if arg == "--repo" && root.is_none() {
            root = Some(PathBuf::from(value()?));
        } else if arg == "--operation" && operation.is_none() {
            let value = value()?;
            let id = value
                .to_str()
                .ok_or_else(|| Error::InvalidInput("operation IDs must be UTF-8".into()))?;
            operation = Some(OperationId::parse(id)?);
        } else if layer.is_none() && !arg.to_string_lossy().starts_with('-') {
            let id = arg
                .to_str()
                .ok_or_else(|| Error::InvalidInput("Layer IDs must be UTF-8".into()))?;
            layer = Some(LayerId::parse(id)?);
        } else {
            return Err(Error::InvalidInput(usage.into()).into());
        }
    }
    let layer = layer.ok_or_else(|| Error::InvalidInput(usage.into()))?;
    let operation = match operation {
        Some(operation) => operation,
        None => fresh_operation("take", &layer)?,
    };
    let root = match root {
        Some(root) => root,
        None => std::env::current_dir().map_err(Error::from)?,
    };
    let mut repo = Repository::open(&root)?;
    let attached = repo.attachment()?.ok_or_else(|| {
        Error::InvalidInput("this repository is not attached to a node; attach first".into())
    })?;
    let link = Link::writing(parse_target(&format!(
        "{}/{}",
        attached.node, attached.repository
    ))?);
    let read = |e| CliError::Remote(remote(&link, e));
    let attached = recover_attachment(&link, &mut repo, attached)?;
    // Everything that can refuse the Layer here runs before the lease moves: the base World
    // is found by identity (fetched when it is newer than this repository's), and a local
    // Layer is refreshed, which refuses one holding captures the node lacks.
    let base = match repo.layer_state(&layer) {
        Ok(_) => None,
        Err(Error::InvalidInput(_)) => {
            let node = sender::layer_head(&mut Session(&link), &layer).map_err(read)?;
            Some(local_world(&link, &mut repo, &attached, node.base)?)
        }
        Err(error) => return Err(error.into()),
    };
    let mut refresh = match base {
        None => Some(stack_remote::refresh(&mut Session(&link), &mut repo, &layer).map_err(read)?),
        Some(_) => None,
    };
    let (seq, node_head) = match call(
        &link,
        &Message::LayerTake {
            operation: operation.clone(),
            layer: layer.clone(),
        },
    )
    .map_err(CliError::Remote)?
    {
        Message::LayerOutcome(LayerOutcome::Accepted { seq, checkpoint }) => (seq, checkpoint),
        Message::LayerOutcome(LayerOutcome::Refused { code }) => {
            return Err(CliError::Remote(RemoteError::new(
                &code,
                format!("the node refused to move Layer {}'s lease", layer.as_str()),
            )));
        }
        _ => return Err(CliError::Remote(unexpected())),
    };
    let created = base.is_some();
    if let Some(base) = base {
        let base = repo.layer_base(base)?;
        repo.create_layer(&layer, &base)?;
    }
    // The node's head may have moved between the refresh and the take.
    let current = |refresh: &Refresh| match refresh {
        Refresh::Current { head, .. } | Refresh::Imported { head, .. } => *head == node_head,
    };
    if !refresh.as_ref().is_some_and(current) {
        refresh =
            Some(stack_remote::refresh(&mut Session(&link), &mut repo, &layer).map_err(read)?);
    }
    let refresh = refresh.expect("refreshed above");
    // A replayed take reports what it recorded; the lease may have moved on since.
    let lease = sender::layer_head(&mut Session(&link), &layer)
        .map_err(read)?
        .lease;
    let head = match repo.layer_state(&layer)? {
        LayerState::Active { head } => head,
        _ => return Err(Error::InvalidInput("the Layer is no longer active".into()).into()),
    };
    let exists = repo
        .workspace_root(&layer)?
        .try_exists()
        .map_err(Error::from)?;
    let (workspace, refreshed) = if exists {
        (None, Some(repo.refresh_workspace(&layer)?))
    } else {
        (Some(repo.open_workspace(&layer)?), None)
    };
    let root = repo.workspace_root(&layer)?;
    let holds = lease == LeaseHolder::Caller;
    let mut text = format!(
        "Take {} is recorded: Layer {}'s writer lease moved to this device at the node's head \
         {node_head} (seq {seq})",
        operation.as_str(),
        layer.as_str()
    );
    if holds {
        text.push_str("\nThis device holds the lease.");
    } else {
        text.push_str(&format!(
            "\nAnother device holds the lease now: this device's writes to Layer {0} are \
             layer_stale. Run `javelin layer take {0}` to move it here again.",
            layer.as_str()
        ));
    }
    if let Some(report) = &refreshed {
        text.push_str(&format!(
            "\nWorkspace {}: {} paths applied, {} local edits kept",
            root.display(),
            report.applied,
            report.kept
        ));
    } else {
        text.push_str(&format!("\nWorkspace: {}", root.display()));
    }
    let value = json!({
        "ok": true,
        "command": "layer take",
        "layer": layer,
        "operation": operation,
        "node": {"seq": seq, "head": node_head},
        "holds_lease": holds,
        "lease": match lease {
            LeaseHolder::Caller => "this_device",
            LeaseHolder::Other => "another_device",
            LeaseHolder::Free => "free",
        },
        "created": created,
        "status": match refresh {
            Refresh::Current { .. } => "current",
            Refresh::Imported { .. } => "imported",
        },
        "head": head,
        "root": root,
        "workspace": workspace,
        "refresh": refreshed,
    });
    Ok((value, text, 0))
}

/// Finishes a sync that stopped after the local World advanced and before `attach.json`
/// named the new head. The local head counts only when a sync made it and the node holds
/// that remote version with the same chain ID and tree; then it is recorded and returned.
/// Any other head is left alone, and the attachment returned unchanged.
fn recover_attachment(
    link: &RefCell<Link>,
    repo: &mut Repository,
    attached: Attachment,
) -> std::result::Result<Attachment, CliError> {
    if repo.follows_attachment(&attached)? {
        return Ok(attached);
    }
    let Some(head) = repo.interrupted_sync()? else {
        return Ok(attached);
    };
    if head.number <= attached.head.number {
        return Ok(attached);
    }
    let resolved = call(
        link,
        &Message::Resolve {
            version: Some(VersionNumber::new(head.number)?),
        },
    )
    .map_err(CliError::Remote)?;
    match resolved {
        Message::Resolved { id, tree, .. } if id == head.id && tree == head.tree => {
            let repaired = Attachment { head, ..attached };
            repo.write_attachment(&repaired)?;
            Ok(repaired)
        }
        _ => Ok(attached),
    }
}

/// Exits at the named point when `JAVELIN_FAILPOINT` names it, for crash tests. Debug
/// builds only, like the library's failpoints.
#[cfg(debug_assertions)]
fn failpoint(name: &str) {
    if std::env::var_os("JAVELIN_FAILPOINT").is_some_and(|set| set == name) {
        std::process::exit(86);
    }
}

#[cfg(not(debug_assertions))]
fn failpoint(_: &str) {}

/// Refusal for following the node when the local World has versions the node lacks.
pub const LOCAL_WORLD_DIVERGED: &str = "local_world_diverged";

/// The local World holding remote World `base`'s tree. Remote and local numbers differ, so
/// the World is matched by its tree. One this repository lacks is fetched as attach does,
/// verified from the attached head, and the local World advances to it; a remote World
/// older than the attached one cannot be fetched that way and is refused. The node is the
/// World's authority, so the advance never overwrites local divergence: a local head that
/// is not the attachment's own World, such as a local Publish, is refused with
/// `local_world_diverged` before anything is fetched.
fn local_world(
    link: &RefCell<Link>,
    repo: &mut Repository,
    attached: &Attachment,
    base: VersionNumber,
) -> std::result::Result<VersionNumber, CliError> {
    let tree = match call(
        link,
        &Message::Resolve {
            version: Some(base),
        },
    )
    .map_err(CliError::Remote)?
    {
        Message::Resolved { number, tree, .. } if number == base => tree,
        _ => return Err(CliError::Remote(unexpected())),
    };
    if let Some(local) = repo.history()?.into_iter().rev().find(|v| v.tree == tree) {
        return Ok(local.number);
    }
    if base.get() < attached.head.number {
        return Err(Error::InvalidInput(format!(
            "the Layer's base, remote World {base}, is older than the attached World {} \
             and this repository does not hold it",
            attached.head.number
        ))
        .into());
    }
    if !repo.follows_attachment(attached)? {
        let local = repo.head()?;
        return Err(CliError::Remote(RemoteError::new(
            LOCAL_WORLD_DIVERGED,
            format!(
                "this repository's World {} (id {}, tree {}) is not the attached remote World {} \
                 (id {}, tree {}): it has local versions the node lacks, and following the node \
                 to remote World {base} would overwrite them. Nothing was changed and the lease \
                 did not move. Take the Layer from a fresh `javelin attach` directory; this \
                 repository keeps its local Worlds.",
                local.number,
                local.id,
                local.tree,
                attached.head.number,
                attached.head.id,
                attached.head.tree
            ),
        )));
    }
    let anchor = Anchor {
        number: VersionNumber::new(attached.head.number)?,
        id: attached.head.id,
        tree: attached.head.tree,
    };
    let trees = MemoryStore::default();
    let fetched = {
        let mut blobs = RepoBlobs(repo);
        fetch(
            Down(link),
            Up(link),
            &trees,
            &mut blobs,
            Some(&anchor),
            Some(base),
            None,
        )
        .map_err(|e| CliError::Remote(remote(link, e)))?
    };
    repo.import_tree(&trees, fetched.tree)?;
    let local = repo.head()?;
    let target = RemoteHead {
        number: fetched.number.get(),
        id: fetched.id,
        tree: fetched.tree,
    };
    let operation = Repository::sync_operation(&target)?;
    let version = match repo.advance_to_tree(&operation, local.number, fetched.tree)? {
        AdvanceOutcome::Accepted { version } => version,
        AdvanceOutcome::Stale { .. } => {
            return Err(Error::InvalidInput("the local World moved; retry".into()).into());
        }
    };
    // A stop here leaves a sync World that `attach.json` does not name yet;
    // `recover_attachment` finishes it on the next take.
    failpoint("take_after_sync_advance");
    repo.write_attachment(&Attachment {
        head: target,
        ..attached.clone()
    })?;
    Ok(version.number)
}

/// An operation ID no earlier command used, for a step that is safe to repeat under a new one.
fn fresh_operation(step: &str, layer: &LayerId) -> Result<OperationId> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let seed = format!(
        "javelin-{step}\0{}\0{nanos}\0{}",
        layer.as_str(),
        std::process::id()
    );
    OperationId::parse(&format!("{step}-{}", ObjectId::for_blob(seed.as_bytes())))
}

/// Delivers outbox records to a node, one request each, with the repository's credential.
/// A write never goes out without a token: no sign-in stalls the record with `unauthorized`,
/// and an expired token that cannot be renewed with `control_plane_unavailable`. A node that
/// refuses the token gets one fresh token and the same request again; the operation ID makes
/// the repeat safe. A node `Error` reply stalls the record with the node's code.
pub struct NodeTransport {
    target: Target,
    credential: Credential,
}

impl NodeTransport {
    /// A transport to `repository` on the node at `https://HOST[:PORT]` (plain http only for localhost).
    pub fn new(node: &str, repository: &str) -> Result<Self> {
        Ok(Self {
            target: parse_target(&format!("{node}/{repository}"))?,
            credential: Credential::for_repository(repository),
        })
    }

    fn send(&self, payload: &[u8], token: Option<&str>) -> std::result::Result<Vec<u8>, Fault> {
        post(&self.target, payload, token, false).map_err(|error| {
            // Every transport failure leaves the record undecided, so it stays queued.
            Fault::NodeUnavailable(if error.code == code::NODE_UNAVAILABLE {
                error.detail
            } else {
                format!("{}: {}", error.code, error.detail)
            })
        })
    }
}

impl Transport for NodeTransport {
    fn deliver(&mut self, entry: &Entry) -> std::result::Result<Vec<u8>, Fault> {
        let now = now();
        // A write is never sent anonymously: the node would refuse it after reading it.
        let token = self.credential.bearer(now)?.ok_or_else(|| {
            Fault::Unauthorized("not signed in; run `javelin login`, or set JAVELIN_TOKEN".into())
        })?;
        let mut reply = self.send(&entry.payload, Some(&token))?;
        if refused_caller(&reply).is_some() {
            let token = self.credential.renew(now)?;
            reply = self.send(&entry.payload, Some(&token))?;
            if let Some(detail) = refused_caller(&reply) {
                return Err(Fault::Unauthorized(format!(
                    "the node refused a fresh token: {detail}"
                )));
            }
        }
        recorded(reply)
    }
}

/// Only an outcome the node recorded acknowledges a record. An `Error` reply, such as a
/// storage failure or `busy`, recorded nothing, so the record stays queued under its
/// operation ID; so does a reply that does not decode.
fn recorded(reply: Vec<u8>) -> std::result::Result<Vec<u8>, Fault> {
    match decode(&reply) {
        Ok(Message::Error { code, detail }) => Err(Fault::Refused { code, detail }),
        Ok(_) => Ok(reply),
        Err(error) => Err(Fault::NodeUnavailable(format!(
            "the node's reply does not decode: {}",
            error.detail
        ))),
    }
}

/// `javelin outbox replay --remote NAME [--repo DIR]`: delivers the queued records for the
/// attached repository NAME in journal order, until one cannot be delivered. That record and
/// every later one stay queued; a later replay sends them again under the same operation IDs.
pub fn outbox_replay(args: Vec<OsString>) -> std::result::Result<Output, CliError> {
    let usage = "expected: javelin outbox replay --remote NAME [--repo DIR] [--json]";
    let mut args = args.into_iter();
    let (mut name, mut root) = (None, None);
    while let Some(arg) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| Error::InvalidInput(usage.into()))?;
        match arg.to_str() {
            Some("--remote") if name.is_none() => {
                name = Some(
                    value
                        .into_string()
                        .map_err(|_| Error::InvalidInput("NAME must be UTF-8".into()))?,
                );
            }
            Some("--repo") if root.is_none() => root = Some(PathBuf::from(value)),
            _ => return Err(Error::InvalidInput(usage.into()).into()),
        }
    }
    let name = name.ok_or_else(|| Error::InvalidInput(usage.into()))?;
    let root = match root {
        Some(root) => root,
        None => std::env::current_dir().map_err(Error::from)?,
    };
    let repo = Repository::open(&root)?;
    let attached = repo.attachment()?.ok_or_else(|| {
        Error::InvalidInput("this repository is not attached to a node; attach first".into())
    })?;
    if attached.repository != name {
        return Err(Error::InvalidInput(format!(
            "this repository is attached to {}, not {name}",
            attached.repository
        ))
        .into());
    }
    let mut outbox = Outbox::open(&root, &name)?;
    let mut transport = NodeTransport::new(&attached.node, &name)?;
    let replay = outbox.replay(&mut transport)?;
    let delivered: Vec<_> = replay
        .delivered
        .iter()
        .map(|delivered| {
            let entry = outbox.entry(delivered.seq);
            json!({
                "seq": delivered.seq,
                "kind": entry.map(|entry| entry.kind.as_str()),
                "operation": entry.map(|entry| entry.operation.as_str()),
                "outcome": summary(&delivered.outcome),
            })
        })
        .collect();
    let pending = outbox.pending().count();
    // Only this replay's stale answers: earlier ones were reported when they came back, and
    // `outbox status` lists them all.
    let stale: Vec<Value> = stale_appends(&outbox, &repo)
        .into_iter()
        .filter(|record| {
            replay
                .delivered
                .iter()
                .any(|delivered| record["seq"] == delivered.seq)
        })
        .collect();
    let mut text = format!(
        "Delivered {} records to {} at {}; {pending} pending.",
        delivered.len(),
        name,
        attached.node
    );
    let (stalled, code, exit) = match &replay.stalled {
        Some(stall) => {
            text.push_str(&format!(
                "\nRecord {} waits: {}: {}",
                stall.seq,
                stall.fault.code(),
                stall.fault.detail()
            ));
            (
                json!({"seq": stall.seq, "code": stall.fault.code(), "detail": stall.fault.detail()}),
                json!(stall.fault.code()),
                1,
            )
        }
        None if !stale.is_empty() => (Value::Null, json!(code::LAYER_STALE), 1),
        None => (Value::Null, Value::Null, 0),
    };
    for line in stale_lines(&stale) {
        text.push('\n');
        text.push_str(&line);
    }
    let value = json!({
        "ok": replay.stalled.is_none() && stale.is_empty(),
        "command": "outbox replay",
        "remote": name,
        "node": attached.node,
        "delivered": delivered,
        "stalled": stalled,
        "stale": stale,
        "code": code,
        "pending": pending,
    });
    Ok((value, text, exit))
}

/// The appends in `outbox` the node answered as stale: another device holds the Layer's
/// writer lease, or the Layer moved on. They stay in the journal with that outcome. Each
/// names the node's head, and the local Layer head that holds the checkpoints the node
/// lacks, which `javelin layer clone NEW --checkpoint HEAD` turns into a new Layer.
pub(crate) fn stale_appends(outbox: &Outbox, repo: &Repository) -> Vec<Value> {
    outbox
        .entries()
        .filter(|entry| entry.kind == javelin::outbox::Kind::Append)
        .filter_map(|entry| {
            let Ok(Message::LayerOutcome(LayerOutcome::Stale { head })) =
                decode(entry.outcome.as_deref()?)
            else {
                return None;
            };
            let local = LayerId::parse(&entry.scope).ok().and_then(|layer| {
                match repo.layer_state(&layer) {
                    Ok(LayerState::Active { head }) => Some(head.checkpoint()),
                    _ => None,
                }
            });
            Some(json!({
                "seq": entry.seq,
                "layer": entry.scope,
                "operation": entry.operation.as_str(),
                "code": code::LAYER_STALE,
                "node_head": head,
                "local_head": local,
            }))
        })
        .collect()
}

/// Text lines for `stale_appends`.
pub(crate) fn stale_lines(stale: &[Value]) -> Vec<String> {
    stale
        .iter()
        .map(|record| {
            let recover = match record["local_head"].as_str() {
                Some(head) => {
                    format!("; keep its work with `javelin layer clone NEW --checkpoint {head}`")
                }
                None => String::new(),
            };
            format!(
                "Record {} for Layer {} is layer_stale: the node holds {} and another device \
                 may hold the writer lease{recover}",
                record["seq"],
                record["layer"].as_str().unwrap_or_default(),
                record["node_head"].as_str().unwrap_or_default(),
            )
        })
        .collect()
}

/// A short view of a node's reply, for output.
fn summary(outcome: &[u8]) -> Value {
    match decode(outcome) {
        Ok(Message::LayerOutcome(LayerOutcome::Stale { head })) => {
            json!({"stale": head})
        }
        Ok(Message::PublishOutcome(PublishOutcome::Published { number, .. })) => {
            json!({"published": number})
        }
        Ok(Message::Error { code, detail }) => json!({"error": code, "detail": detail}),
        Ok(other) => {
            let text = format!("{other:?}");
            json!(text.chars().take(200).collect::<String>())
        }
        Err(error) => json!({"undecodable": error.detail}),
    }
}

/// One write to `repository` on the node at `https://HOST[:PORT]` (plain http only for
/// localhost), with the repository's credential, and its reply. Without a token nothing is
/// sent and the error is `unauthorized`. A node `Error` reply becomes the error.
pub fn call_node(
    node: &str,
    repository: &str,
    message: &Message,
) -> std::result::Result<Message, RemoteError> {
    let target = parse_target(&format!("{}/{repository}", node.trim_end_matches('/')))
        .map_err(|error| RemoteError::new(error.code(), error.to_string()))?;
    call(&Link::writing(target), message)
}

#[cfg(test)]
mod tests {
    use javelin::{FileEntry, Manifest, OperationId, RepoPath, Repository, VersionNumber};

    use super::{remove_staging, rename_new};

    #[test]
    fn remove_staging_removes_a_store_with_a_sealed_base() {
        let dir = tempfile::TempDir::new().unwrap();
        let staging = dir.path().join("staging");
        std::fs::create_dir(&staging).unwrap();
        let mut repo = Repository::init(&staging).unwrap();
        let content = repo.put_blob(b"shared").unwrap();
        let path = RepoPath::parse("a.txt").unwrap();
        let tree = repo
            .put_snapshot(
                &Manifest::new([(
                    path,
                    FileEntry::Regular {
                        content,
                        executable: false,
                    },
                )])
                .unwrap(),
            )
            .unwrap();
        let operation = OperationId::parse("seed-1").unwrap();
        repo.advance_to_tree(&operation, VersionNumber::new(0).unwrap(), tree)
            .unwrap();
        repo.warm_base().unwrap();
        drop(repo);
        assert!(staging.join(".javelin/bases/rolling").is_dir());

        // Without opening the sealed root first, removal fails with permission denied.
        remove_staging(&staging).unwrap();
        assert!(!staging.exists());
    }

    #[test]
    fn rename_new_never_replaces_an_existing_directory() {
        let dir = tempfile::TempDir::new().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        std::fs::create_dir(&from).unwrap();
        std::fs::write(from.join("marker"), b"new").unwrap();
        std::fs::create_dir(&to).unwrap();

        let error = rename_new(&from, &to).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(from.join("marker").exists());
        assert!(to.is_dir());

        std::fs::remove_dir(&to).unwrap();
        rename_new(&from, &to).unwrap();
        assert_eq!(std::fs::read(to.join("marker")).unwrap(), b"new");
    }
}
