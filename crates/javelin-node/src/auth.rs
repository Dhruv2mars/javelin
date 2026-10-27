//! The node's authorization gate. It checks a bearer token against the repository's
//! applied permissions and revocation set, and reports each refusal as a protocol error:
//! `unauthorized` for who the caller is, `forbidden` for what it may do, and
//! `stale_permissions` for an out-of-date push. The HTTP server passes the token from each
//! request's `Authorization: Bearer` header, or `None` for an anonymous caller.
//!
//! A self-hosted node keeps what the gate starts from in its data directory: the applied
//! permissions (`permissions.json`) and the public keys it trusts (`keys.json`). The control
//! plane writes both; the node never holds a signing key.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, ErrorKind, Write},
    path::Path,
};

use javelin::Reader;
use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_identity::{Access, Grant, KeySet, Need, Refusal, Revocation};
use javelin_protocol::ProtocolError;
use serde::{Deserialize, Serialize};

/// The applied permissions in a node's data directory.
pub const PERMISSIONS_FILE: &str = "permissions.json";
/// The public keys a node trusts, in its data directory.
pub const KEYS_FILE: &str = "keys.json";

pub struct Gate {
    access: Access,
}

impl Gate {
    pub fn new(repository: RepositoryId, keys: KeySet) -> Self {
        Self {
            access: Access::new(repository, keys),
        }
    }

    /// The gate a node serving `dir` starts with: its stored keys and permissions. The
    /// repository is the one the permissions name, or `name` before the first push, when
    /// every request is refused.
    pub fn load(dir: &Path, name: &str) -> io::Result<Self> {
        let permissions = read_permissions(dir)?;
        let repository = match &permissions {
            Some(permissions) => permissions.repository.clone(),
            None => RepositoryId::parse(name)
                .map_err(|error| io::Error::new(ErrorKind::InvalidInput, error.to_string()))?,
        };
        let mut gate = Self::new(repository, read_keys(dir)?);
        if let Some(permissions) = permissions {
            gate.apply(permissions)
                .map_err(|error| io::Error::new(ErrorKind::InvalidData, error.detail))?;
        }
        Ok(gate)
    }

    /// The permissions currently applied, if any.
    pub fn permissions(&self) -> Option<&AppliedPermissions> {
        self.access.permissions()
    }

    /// The trusted keys, for rotation.
    pub fn keys_mut(&mut self) -> &mut KeySet {
        self.access.keys_mut()
    }

    /// Applies a control-plane permissions push. The caller must authenticate the push.
    pub fn apply(&mut self, permissions: AppliedPermissions) -> Result<(), ProtocolError> {
        self.access.apply(permissions).map_err(refused)
    }

    /// Applies a control-plane revocation. The caller must authenticate the push.
    pub fn revoke(&mut self, target: Revocation) {
        self.access.revoke(target);
    }

    /// Decides one request at Unix time `now`. `token` is `None` for an anonymous caller.
    pub fn check(
        &self,
        token: Option<&str>,
        now: u64,
        need: Need<'_>,
    ) -> Result<Grant, ProtocolError> {
        self.access.authorize(token, now, need).map_err(refused)
    }
}

/// The audiences a caller reads, from its role in the repository: no role is an outsider,
/// Read and Review are members, Maintain is a maintainer. Named groups are not applied yet.
pub fn reader_for(role: Option<Role>) -> Reader {
    match role {
        None => Reader::outsider(),
        Some(Role::Maintain) => Reader::maintainer(),
        Some(Role::Read | Role::Review) => Reader::member([]),
    }
}

impl Gate {
    /// The audiences of an authorized request, from the role the applied permissions grant
    /// the caller by name. An anonymous caller, and an authenticated one that a public
    /// repository only lets read, hold no granted role: both are outsiders.
    pub fn reader(&self, grant: &Grant) -> Reader {
        let role = grant
            .principal
            .as_deref()
            .and_then(|principal| PrincipalId::parse(principal).ok())
            .and_then(|principal| self.permissions()?.roles.get(&principal).copied());
        let reader = reader_for(role);
        match &grant.principal {
            Some(principal) => reader.with_principal(principal.clone()),
            None => reader,
        }
    }
}

/// Refuses a write by `writer` whose `changes`, over World `base`, touch a path outside the
/// caller's audiences (`Repository::check_writer`). The refusal is `forbidden` and names no
/// path, so it does not reveal which hidden path matched.
pub fn check_write(
    repo: &javelin::Repository,
    writer: &Reader,
    changes: &javelin::SparseChanges,
    base: javelin::VersionNumber,
) -> Result<(), ProtocolError> {
    match repo.check_writer(changes, base, writer) {
        Err(javelin::Error::HiddenPath(_)) => Err(ProtocolError::new(
            "forbidden",
            "the change touches a path outside the caller's audiences",
        )),
        other => Ok(other?),
    }
}

/// `check_write` for the changes of stored checkpoint `checkpoint` over its own base, or over
/// `base` when given. A checkpoint this store does not hold, or one the writer may not read,
/// is left to the handler, which refuses both exactly as absent: refusing an unreadable one
/// here would confirm that it exists.
pub fn check_checkpoint_write(
    repo: &javelin::Repository,
    writer: &Reader,
    checkpoint: javelin::ObjectId,
    base: Option<javelin::VersionNumber>,
) -> Result<(), ProtocolError> {
    let Ok(id) = javelin::CheckpointId::parse(&checkpoint.to_string()) else {
        return Ok(());
    };
    if !repo.checkpoint_visible(id, writer).unwrap_or(false) {
        return Ok(());
    }
    let Ok(checkpoint) = repo.read_checkpoint(id) else {
        return Ok(());
    };
    let base = base.unwrap_or(checkpoint.base().version());
    check_write(repo, writer, checkpoint.changes(), base)
}

fn refused(refusal: Refusal) -> ProtocolError {
    ProtocolError::new(refusal.code(), refusal.detail())
}

/// The permissions stored in `dir`, or `None` before the first push. A file that does not
/// parse is `InvalidData`.
pub fn read_permissions(dir: &Path) -> io::Result<Option<AppliedPermissions>> {
    let bytes = match fs::read(dir.join(PERMISSIONS_FILE)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let stored: Stored = serde_json::from_slice(&bytes).map_err(|error| corrupt(&error))?;
    stored.into_permissions().map(Some)
}

/// Stores `permissions` in `dir`, replacing what was there in one rename.
pub fn write_permissions(dir: &Path, permissions: &AppliedPermissions) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(&Stored::from(permissions)).map_err(io::Error::other)?;
    write_replacing(&dir.join(PERMISSIONS_FILE), &bytes)
}

/// The keys stored in `dir`. With none stored the node trusts no key, so only anonymous
/// reads of a public repository pass.
pub fn read_keys(dir: &Path) -> io::Result<KeySet> {
    match fs::read(dir.join(KEYS_FILE)) {
        Ok(bytes) => KeySet::from_json(&bytes).map_err(|error| corrupt(&error)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(KeySet::new()),
        Err(error) => Err(error),
    }
}

/// Stores the keys the node trusts in `dir`, replacing what was there in one rename.
pub fn write_keys(dir: &Path, keys: &KeySet) -> io::Result<()> {
    write_replacing(&dir.join(KEYS_FILE), &keys.to_json())
}

fn corrupt(error: &dyn std::fmt::Display) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, format!("{error}"))
}

/// Writes through a temporary file, so a crash leaves the old or the new content. The file
/// is private to its owner on Unix.
fn write_replacing(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
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

#[derive(Serialize, Deserialize)]
struct Stored {
    repository: String,
    epoch: u64,
    public: bool,
    roles: BTreeMap<String, String>,
    required_approvals: u32,
}

impl From<&AppliedPermissions> for Stored {
    fn from(permissions: &AppliedPermissions) -> Self {
        Self {
            repository: permissions.repository.as_str().into(),
            epoch: permissions.epoch,
            public: permissions.visibility == Visibility::Public,
            roles: permissions
                .roles
                .iter()
                .map(|(principal, role)| (principal.as_str().into(), role_name(*role).into()))
                .collect(),
            required_approvals: permissions.required_approvals,
        }
    }
}

impl Stored {
    fn into_permissions(self) -> io::Result<AppliedPermissions> {
        let corrupt = |what: &str| {
            io::Error::new(
                ErrorKind::InvalidData,
                format!("permissions are corrupt: {what}"),
            )
        };
        let repository =
            RepositoryId::parse(&self.repository).map_err(|_| corrupt("repository"))?;
        let roles = self
            .roles
            .iter()
            .map(|(principal, role)| {
                let principal = PrincipalId::parse(principal).map_err(|_| corrupt("principal"))?;
                let role = parse_role(role).ok_or_else(|| corrupt("role"))?;
                Ok((principal, role))
            })
            .collect::<io::Result<_>>()?;
        Ok(AppliedPermissions {
            repository,
            epoch: self.epoch,
            visibility: if self.public {
                Visibility::Public
            } else {
                Visibility::Private
            },
            roles,
            required_approvals: self.required_approvals,
        })
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Read => "read",
        Role::Review => "review",
        Role::Maintain => "maintain",
    }
}

fn parse_role(value: &str) -> Option<Role> {
    match value {
        "read" => Some(Role::Read),
        "review" => Some(Role::Review),
        "maintain" => Some(Role::Maintain),
        _ => None,
    }
}
