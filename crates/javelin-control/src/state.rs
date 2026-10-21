//! The control plane's durable state: one JSON document, rewritten whole under an exclusive
//! lock on its directory. Secrets appear only as hashes, except the signing seed, which
//! stays in the directory's private file.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::ErrorKind,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    ControlError,
    passkey::{BrowserRecord, CeremonyRecord, PasskeyRecord},
    secret,
};

const STATE_FILE: &str = "control.json";
const LOCK_FILE: &str = "control.lock";

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct State {
    /// Ed25519 seed that signs access tokens, base64url. Empty until bootstrap.
    pub signing_seed: String,
    /// Hash of the bootstrap admin token. Empty until bootstrap.
    pub admin_token_hash: String,
    /// Keyed by the hash of the device code.
    pub device_codes: BTreeMap<String, DeviceRecord>,
    /// Keyed by the hash of the refresh token.
    pub sessions: BTreeMap<String, SessionRecord>,
    /// Keyed by repository ID.
    pub repositories: BTreeMap<String, RepositoryRecord>,
    /// Operation ID to the repository it created.
    pub operations: BTreeMap<String, String>,
    /// Enrolled passkeys, keyed by credential ID.
    pub passkeys: BTreeMap<String, PasskeyRecord>,
    /// Passkey ceremonies in flight, keyed by ceremony ID.
    pub ceremonies: BTreeMap<String, CeremonyRecord>,
    /// Browser sign-ins, keyed by the hash of their token.
    pub browser_sessions: BTreeMap<String, BrowserRecord>,
}

impl State {
    pub fn initialized(&self) -> bool {
        !self.admin_token_hash.is_empty()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct DeviceRecord {
    /// Hash of the canonical user code, which is what the approver types.
    pub user_hash: String,
    pub expires_at: u64,
    pub approved_by: Option<String>,
    pub device: Option<String>,
    pub redeemed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SessionRecord {
    pub principal: String,
    pub device: String,
    pub expires_at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Phase {
    Provisioning,
    Ready,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RepositoryRecord {
    pub owner: String,
    pub name: String,
    pub public: bool,
    pub publish: String,
    pub creator: String,
    pub operation: String,
    pub phase: Phase,
    pub epoch: u64,
    /// The node provider the repository was reserved for, as `NodeTarget::provider` names it.
    /// Empty for a record written before this was stored.
    #[serde(default)]
    pub provider: String,
    /// The node URL the repository was reserved for. Only a Provisioning record's node matters
    /// on retry; a Ready record returns unchanged.
    #[serde(default)]
    pub node_url: Option<String>,
    /// Set for a fork: its source repository and the version it was forked at.
    #[serde(default)]
    pub fork: Option<ForkOf>,
}

/// Where a fork came from. Its version ID comes from the node, which holds the copied log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ForkOf {
    pub source: String,
    pub number: u64,
}

/// The state directory: the document plus the lock that serializes every call.
pub(crate) struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn open(dir: &Path) -> Result<Self, ControlError> {
        fs::create_dir_all(dir).map_err(storage)?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    /// Runs `read` on the current state. Nothing is written.
    pub fn read<T>(
        &self,
        read: impl FnOnce(&State) -> Result<T, ControlError>,
    ) -> Result<T, ControlError> {
        let _lock = self.lock()?;
        read(&self.load()?)
    }

    /// Runs `change` on the current state, then saves it. If `change` fails, nothing is saved.
    pub fn update<T>(
        &self,
        change: impl FnOnce(&mut State) -> Result<T, ControlError>,
    ) -> Result<T, ControlError> {
        let _lock = self.lock()?;
        let mut state = self.load()?;
        let value = change(&mut state)?;
        let bytes = serde_json::to_vec_pretty(&state)
            .map_err(|error| ControlError::Storage(error.to_string()))?;
        secret::write_private(&self.dir.join(STATE_FILE), &bytes).map_err(storage)?;
        Ok(value)
    }

    fn lock(&self) -> Result<File, ControlError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(LOCK_FILE))
            .map_err(storage)?;
        file.lock().map_err(storage)?;
        Ok(file)
    }

    fn load(&self) -> Result<State, ControlError> {
        match fs::read(self.dir.join(STATE_FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| ControlError::Storage(format!("state is corrupt: {error}"))),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(State::default()),
            Err(error) => Err(storage(error)),
        }
    }
}

fn storage(error: std::io::Error) -> ControlError {
    ControlError::Storage(error.to_string())
}
