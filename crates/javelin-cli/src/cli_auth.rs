//! The credential this client sends to a node, as `Authorization: Bearer` (D6,
//! `docs/online-design.md` section 4.7). `JAVELIN_TOKEN`, when set, is sent as it is and never
//! refreshed, which suits a CI reporter. Otherwise the device's sign-in is used: the access
//! token for the repository is cached in `$JAVELIN_HOME/tokens.json` until shortly before it
//! expires, and a fresh one comes from the control plane with the refresh token in
//! `$JAVELIN_HOME/credentials`. The refresh token itself is never sent to a node.
//!
//! While a cached token is valid, node work needs no control plane. Once it has expired, a
//! control plane that cannot be reached is `control_plane_unavailable`, and a sign-in the
//! control plane refuses is `unauthorized`.

use std::{
    collections::BTreeMap,
    env,
    fs::{self, OpenOptions},
    io::{self, ErrorKind, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use javelin::outbox::Fault;
use javelin_control::{ControlError, Credentials};
use serde_json::{Value, json};

use crate::cli_control;

/// A cached token is renewed this many seconds before it expires, so it does not lapse in
/// flight or on a node whose clock runs ahead.
const MARGIN: u64 = 60;
const CACHE_FILE: &str = "tokens.json";

pub struct Credential {
    repository: String,
    source: Source,
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
        let source = match env::var("JAVELIN_TOKEN") {
            Ok(token) if !token.is_empty() => Source::Static(token),
            _ => Source::Device,
        };
        Self {
            repository: repository.into(),
            source,
        }
    }

    /// The token to send now, or `None` for an anonymous request when this device is not
    /// signed in. An expired cached token is renewed through the control plane first.
    pub fn bearer(&mut self, now: u64) -> Result<Option<String>, Fault> {
        match &self.source {
            Source::Static(token) => Ok(Some(token.clone())),
            Source::Device => {
                if let Some(token) = cached(&self.repository, now)? {
                    return Ok(Some(token));
                }
                if !credentials_path()?.exists() {
                    return Ok(None);
                }
                self.renew(now).map(Some)
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
                let control = cli_control::control().map_err(fault)?;
                let signin = control
                    .refresh(&credentials.refresh_token, now)
                    .map_err(fault)?;
                let token = control
                    .repository_token(&signin.access_token, &self.repository, now)
                    .map_err(fault)?;
                store(
                    &self.repository,
                    &token.access_token,
                    now + token.expires_in,
                )?;
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

fn cache_path() -> Result<PathBuf, Fault> {
    Ok(cli_control::javelin_home().map_err(local)?.join(CACHE_FILE))
}

fn read_cache(path: &Path) -> Result<BTreeMap<String, Value>, Fault> {
    match fs::read(path) {
        // A damaged cache only costs a refresh.
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(local(error)),
    }
}

/// The cached token for `repository`, if it is still good for `MARGIN` seconds.
fn cached(repository: &str, now: u64) -> Result<Option<String>, Fault> {
    let cache = read_cache(&cache_path()?)?;
    let Some(entry) = cache.get(repository) else {
        return Ok(None);
    };
    let fresh = entry["expires_at"]
        .as_u64()
        .is_some_and(|expires_at| expires_at > now.saturating_add(MARGIN));
    Ok(fresh
        .then(|| entry["access_token"].as_str().map(str::to_owned))
        .flatten())
}

fn store(repository: &str, token: &str, expires_at: u64) -> Result<(), Fault> {
    let path = cache_path()?;
    let mut cache = read_cache(&path)?;
    cache.insert(
        repository.into(),
        json!({"access_token": token, "expires_at": expires_at}),
    );
    let bytes = serde_json::to_vec_pretty(&cache).map_err(local)?;
    write_private(&path, &bytes).map_err(local)
}

/// Writes through a temporary file, private to the owner on Unix.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
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
