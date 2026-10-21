//! `javelin attach`: fetches a hosted repository's head into the local store over the
//! node's HTTP transport, one protocol frame per request, then records the attachment.
//! `javelin_protocol::fetch` runs unchanged on top of `Link`. See `docs/online-design.md`
//! section 4.1 (U4).

use std::{
    cell::RefCell,
    ffi::OsString,
    io::{self, Cursor, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    time::Duration,
};

use javelin::{
    Attachment, Error, ObjectId, RemoteHead, Repository, Result, VersionNumber, tree::MemoryStore,
};
use javelin_protocol::{
    Message, ProtocolError,
    codec::{VERSION, decode, encode, read_frame, write_frame},
    fetch::{Blobs, fetch},
};
use serde_json::json;

use crate::CliError;
use crate::cli::Output;

const PROVIDER: &str = "self-hosted";
const TIMEOUT: Duration = Duration::from_secs(30);
/// One framed message plus the HTTP head in front of it.
const MAX_REPLY: u64 = javelin_protocol::limits::MAX_FRAME as u64 + 16 * 1024;

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
    /// `http://HOST:PORT`, as stored in `attach.json`.
    node: String,
    /// `HOST:PORT`, for connecting.
    authority: String,
    repository: String,
}

fn parse_target(text: &str) -> Result<Target> {
    let usage = "attach takes an http:// node URL ending in /REPOSITORY";
    let rest = text
        .strip_prefix("http://")
        .ok_or_else(|| Error::InvalidInput(usage.into()))?;
    let (authority, repository) = rest
        .split_once('/')
        .ok_or_else(|| Error::InvalidInput(usage.into()))?;
    let valid_name = !repository.is_empty()
        && repository
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
    if authority.is_empty() || !valid_name {
        return Err(Error::InvalidInput(usage.into()));
    }
    Ok(Target {
        node: format!("http://{authority}"),
        authority: authority.into(),
        repository: repository.into(),
    })
}

/// The transport state shared by both halves of one protocol session.
struct Link {
    target: Target,
    /// The frame the protocol is writing. It is sent when its reply is first read.
    outbound: Vec<u8>,
    inbox: Cursor<Vec<u8>>,
    /// The transport failure behind a protocol error, if any.
    failure: Option<RemoteError>,
}

impl Link {
    fn new(target: Target) -> RefCell<Self> {
        RefCell::new(Self {
            target,
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

    fn round_trip(&self, body: &[u8]) -> std::result::Result<Vec<u8>, RemoteError> {
        let unavailable = |e: io::Error| RemoteError::new("node_unavailable", e.to_string());
        let address = self
            .target
            .authority
            .to_socket_addrs()
            .map_err(unavailable)?
            .next()
            .ok_or_else(|| RemoteError::new("node_unavailable", "no address for the node"))?;
        let mut stream = TcpStream::connect_timeout(&address, TIMEOUT).map_err(unavailable)?;
        stream
            .set_read_timeout(Some(TIMEOUT))
            .map_err(unavailable)?;
        stream
            .set_write_timeout(Some(TIMEOUT))
            .map_err(unavailable)?;
        let head = format!(
            "POST /v2/r/{} HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.target.repository,
            self.target.authority,
            body.len()
        );
        stream.write_all(head.as_bytes()).map_err(unavailable)?;
        stream.write_all(body).map_err(unavailable)?;
        // Bounded in total, so a node cannot make the client buffer without limit.
        let mut reply = Vec::new();
        stream
            .take(MAX_REPLY + 1)
            .read_to_end(&mut reply)
            .map_err(unavailable)?;
        if reply.len() as u64 > MAX_REPLY {
            return Err(RemoteError::new(
                "limit_exceeded",
                "the node's reply is larger than one frame",
            ));
        }
        let split = reply
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| RemoteError::new("node_unavailable", "malformed HTTP reply"))?;
        let status = std::str::from_utf8(&reply[..split])
            .ok()
            .and_then(|head| head.split(' ').nth(1)?.parse::<u16>().ok());
        match status {
            Some(200) => Ok(reply[split + 4..].to_vec()),
            Some(404) => Err(RemoteError::new(
                "unknown_repository",
                format!(
                    "{} has no repository {}",
                    self.target.node, self.target.repository
                ),
            )),
            other => Err(RemoteError::new(
                "node_error",
                format!("the node answered HTTP {other:?}"),
            )),
        }
    }
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

/// `javelin attach http://HOST:PORT/REPOSITORY [project-directory]`
pub fn attach(args: Vec<OsString>) -> std::result::Result<Output, CliError> {
    let usage = "expected: javelin attach http://HOST:PORT/REPOSITORY [project-directory] [--json]";
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
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let staging = root.join(format!(".javelin-attach-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&staging).map_err(Error::from)?;
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
    repo.check_attach(head)?;

    // Pinned to the number resolved above: World versions are immutable, so the fetch
    // reads exactly the head this attach reports.
    let trees = MemoryStore::default();
    let fetched = {
        let mut blobs = RepoBlobs(&mut repo);
        let version = Some(VersionNumber::new(head.number)?);
        fetch(Down(link), Up(link), &trees, &mut blobs, version, None)
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
