//! The credential this client sends to a node, as `Authorization: Bearer` (D6,
//! `docs/online-design.md` section 4.7). `JAVELIN_TOKEN`, when set, is sent as it is and never
//! refreshed, which suits a CI reporter. Otherwise the device's sign-in is used: the access
//! token for each repository is cached until shortly before it expires, and a fresh one comes
//! from the control plane with the refresh token in `$JAVELIN_HOME/credentials`. The refresh
//! token itself is never sent to a node.
//!
//! The cache is `$JAVELIN_HOME/tokens/<principal>+<device>/repo-<repository>.json`: one
//! directory per sign-in, one file per repository, each replaced by rename. A token from
//! another sign-in is therefore never read, processes refreshing different repositories
//! never overwrite each other, and `javelin login` only switches directories. It removes
//! another sign-in's directory only once nothing in it has changed for `STALE_AFTER`, so it
//! never deletes a file a running command is writing. A token that cannot be cached is still
//! used for the command that fetched it.
//!
//! While a cached token is valid, node work needs no control plane. Once it has expired, a
//! control plane that cannot be reached is `control_plane_unavailable`, and a sign-in the
//! control plane refuses is `unauthorized`.

use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, ErrorKind, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use javelin::outbox::Fault;
use javelin_control::{ControlError, Credentials};
use serde_json::{Value, json};

use crate::cli_control;

/// A cached token is renewed this many seconds before it expires, so it does not lapse in
/// flight or on a node whose clock runs ahead.
const MARGIN: u64 = 60;
const CACHE_DIR: &str = "tokens";
/// Another sign-in's cache directory is removed once nothing in it changed for this long:
/// twice the one-hour access-token life (D6), so every token in it has expired.
const STALE_AFTER: Duration = Duration::from_secs(2 * 3600);

pub struct Credential {
    repository: String,
    source: Source,
    /// A renewal through the control plane gives up at this instant.
    deadline: Option<std::time::Instant>,
}

enum Source {
    /// `JAVELIN_TOKEN`: sent unchanged, never refreshed.
    Static(String),
    /// The device's sign-in, with access tokens cached per repository.
    Device,
}

impl Credential {
    /// The credential for `repository`'s node.
    pub fn for_repository(repository: &str) -> Self {
        Self::until(repository, None)
    }

    /// As [`Credential::for_repository`], renewing only until `deadline`, when there is one.
    pub fn until(repository: &str, deadline: Option<std::time::Instant>) -> Self {
        let source = match env::var("JAVELIN_TOKEN") {
            Ok(token) if !token.is_empty() => Source::Static(token),
            _ => Source::Device,
        };
        Self {
            repository: repository.into(),
            source,
            deadline,
        }
    }

    /// The token to send now, or `None` when this device is not signed in. An expired cached token is renewed through the control plane first.
    pub fn bearer(&mut self, now: u64) -> Result<Option<String>, Fault> {
        match &self.source {
            Source::Static(token) => Ok(Some(token.clone())),
            Source::Device => {
                let Some(credentials) = signed_in()? else {
                    return Ok(None);
                };
                match cached(&self.repository, &credentials, now)? {
                    Some(token) => Ok(Some(token)),
                    None => self.renew(now).map(Some),
                }
            }
        }
    }

    /// A fresh token, after the node refused the one sent: the device's refresh token buys a
    /// sign-in token, which buys an access token for this repository.
    pub fn renew(&mut self, now: u64) -> Result<String, Fault> {
        match &self.source {
            Source::Static(_) => Err(Fault::Unauthorized(
                "the node refused JAVELIN_TOKEN; set a current token".into(),
            )),
            Source::Device => {
                let credentials = Credentials::load(&credentials_path()?).map_err(fault)?;
                let control = cli_control::control_until(self.deadline).map_err(fault)?;
                let signin = control
                    .refresh(&credentials.refresh_token, now)
                    .map_err(fault)?;
                let token = control
                    .repository_token(&signin.access_token, &self.repository, now)
                    .map_err(fault)?;
                // The token is good whether or not it can be cached: a failed write costs only
                // a refresh on the next command.
                let _ = store(
                    &self.repository,
                    &credentials,
                    &token.access_token,
                    now + token.expires_in,
                );
                Ok(token.access_token)
            }
        }
    }
}

/// An unreachable control plane keeps its code. Any other refusal means the device cannot get
/// a credential until it signs in again.
fn fault(error: ControlError) -> Fault {
    if error.code() == javelin_protocol::codec::code::CONTROL_PLANE_UNAVAILABLE {
        Fault::ControlPlaneUnavailable(error.to_string())
    } else {
        Fault::Unauthorized(format!("{}: {error}", error.code()))
    }
}

fn local(error: impl std::fmt::Display) -> Fault {
    Fault::Unauthorized(format!("the local credentials cannot be used: {error}"))
}

fn credentials_path() -> Result<PathBuf, Fault> {
    cli_control::credentials_path().map_err(local)
}

/// This device's sign-in, or `None` when it is signed out.
fn signed_in() -> Result<Option<Credentials>, Fault> {
    let path = credentials_path()?;
    if !path.exists() {
        return Ok(None);
    }
    Credentials::load(&path).map(Some).map_err(fault)
}

fn cache_root() -> io::Result<PathBuf> {
    cli_control::javelin_home()
        .map(|home| home.join(CACHE_DIR))
        .map_err(|error| io::Error::other(error.to_string()))
}

/// `value` as a file name: bytes outside `[A-Za-z0-9_.-]` are escaped as `%XX`, so an ID with
/// `:` is still valid on Windows.
fn escape(value: &str) -> String {
    let mut name = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"_.-".contains(&byte) {
            name.push(byte as char);
        } else {
            name.push_str(&format!("%{byte:02X}"));
        }
    }
    name
}

/// The cache directory of one sign-in. `+` never survives `escape`, so it separates the two
/// IDs unambiguously, and neither half can be `.` or `..` on its own.
fn sign_in_dir(credentials: &Credentials) -> String {
    format!(
        "{}+{}",
        escape(&credentials.principal),
        escape(&credentials.device)
    )
}

/// The cache file of `repository` for this sign-in.
fn cache_path(repository: &str, credentials: &Credentials) -> Result<PathBuf, Fault> {
    Ok(cache_root()
        .map_err(local)?
        .join(sign_in_dir(credentials))
        .join(format!("repo-{}.json", escape(repository))))
}

/// The cached token for `repository`, if this sign-in has one still good for `MARGIN` seconds.
fn cached(repository: &str, credentials: &Credentials, now: u64) -> Result<Option<String>, Fault> {
    // The cache is only an optimization: a missing, unreadable or damaged entry costs a refresh.
    let entry: Value = match fs::read(cache_path(repository, credentials)?) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => return Ok(None),
    };
    let fresh = entry["expires_at"]
        .as_u64()
        .is_some_and(|expires_at| expires_at > now.saturating_add(MARGIN));
    Ok(fresh
        .then(|| entry["access_token"].as_str().map(str::to_owned))
        .flatten())
}

fn store(
    repository: &str,
    credentials: &Credentials,
    token: &str,
    expires_at: u64,
) -> Result<(), Fault> {
    let entry = json!({"access_token": token, "expires_at": expires_at});
    let bytes = serde_json::to_vec_pretty(&entry).map_err(local)?;
    write_private(&cache_path(repository, credentials)?, &bytes).map_err(local)
}

/// Removes other sign-ins' cache directories in which nothing changed for `STALE_AFTER`.
/// `javelin login` calls this after saving `current`. The current sign-in's directory, and any
/// directory a command may still be writing to, is left alone.
pub fn remove_stale_sign_ins(current: &Credentials) -> io::Result<()> {
    let root = cache_root()?;
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let keep = sign_in_dir(current);
    let now = SystemTime::now();
    for entry in entries {
        let entry = entry?;
        if entry.file_name().to_string_lossy() == keep || !entry.file_type()?.is_dir() {
            continue;
        }
        if newest_change(&entry.path())?
            .is_none_or(|newest| now.duration_since(newest).unwrap_or_default() >= STALE_AFTER)
        {
            match fs::remove_dir_all(entry.path()) {
                Err(error) if error.kind() != ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
    }
    Ok(())
}

/// The latest modification time of a file in `dir`, or `None` when it holds none.
fn newest_change(dir: &Path) -> io::Result<Option<SystemTime>> {
    let mut newest = None;
    for entry in fs::read_dir(dir)? {
        let modified = entry?.metadata()?.modified()?;
        newest = newest.max(Some(modified));
    }
    Ok(newest)
}

/// Writes through a temporary file, private to the owner on Unix.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Unique per process and call, so concurrent writers never share a temporary file.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let tmp = path.with_extension(format!("tmp-{}-{nanos}", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(tmp, path)
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
