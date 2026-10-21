//! The control plane: bootstrap, caller authentication, and token issue. The device-code
//! and repository operations live in their own modules and extend [`ControlPlane`].

use std::path::Path;

use ed25519_dalek::SigningKey;
use javelin_collaboration::AppliedPermissions;
use javelin_collaboration::RepositoryId;
use javelin_identity::{Claims, Issuer, KeySet, MAX_LIFETIME, Scope, verify};

use crate::{
    ControlError, LocalNode, secret,
    state::{State, Store},
};

/// The principal the bootstrap admin token acts as.
pub const ADMIN_PRINCIPAL: &str = "admin";
/// The device the admin token acts from.
const ADMIN_DEVICE: &str = "dev_admin";
/// Key ID of the signing key. Rotation adds a new ID and retires this one.
pub(crate) const KID: &str = "control-1";
/// Prefix that marks a bootstrap admin token, so it is never parsed as a JWS.
const ADMIN_PREFIX: &str = "jav_admin_";
/// Scopes a signed-in principal holds. Nodes intersect them with the principal's role.
pub(crate) const SESSION_SCOPES: [Scope; 7] = [
    Scope::Read,
    Scope::LayerWrite,
    Scope::Publish,
    Scope::ReviewWrite,
    Scope::ReviewApprove,
    Scope::RepoCreate,
    Scope::Import,
];

/// The node side of the control plane's pushes. Both methods are idempotent, so a retry
/// after a lost reply repeats them safely.
pub trait NodeLink {
    /// Creates the repository's node, empty World included. Repeating it changes nothing.
    fn provision(&self, repository: &RepositoryId) -> Result<(), ControlError>;
    /// Pushes applied permissions. The node accepts the applied epoch again only if it is identical.
    fn apply(&self, permissions: &AppliedPermissions) -> Result<(), ControlError>;
}

pub struct ControlPlane {
    pub(crate) store: Store,
    pub(crate) node: Box<dyn NodeLink>,
}

impl ControlPlane {
    /// Opens the control plane in `dir`, with self-hosted nodes under `dir/nodes`.
    pub fn open(dir: &Path) -> Result<Self, ControlError> {
        Self::with_node(dir, Box::new(LocalNode::new(dir.join("nodes"))))
    }

    /// Opens the control plane in `dir` with a caller-supplied node.
    pub fn with_node(dir: &Path, node: Box<dyn NodeLink>) -> Result<Self, ControlError> {
        Ok(Self {
            store: Store::open(dir)?,
            node,
        })
    }

    /// The keys a node trusts for this control plane's access tokens. Nodes install these;
    /// they never receive the signing seed.
    pub fn node_keys(&self) -> Result<KeySet, ControlError> {
        self.store.read(|state| {
            let mut keys = KeySet::new();
            keys.trust(KID, signing_key(state)?.verifying_key())
                .map_err(|error| ControlError::Storage(error.to_string()))?;
            Ok(keys)
        })
    }

    /// Creates the signing key and the admin token, once. Only the token's hash is kept, so
    /// the returned token is the only copy.
    pub fn bootstrap(&self) -> Result<String, ControlError> {
        let token = secret::token(ADMIN_PREFIX);
        let seed = secret::encode(&secret::bytes::<32>());
        self.store.update(|state| {
            if state.initialized() {
                return Err(ControlError::AlreadyInitialized);
            }
            state.signing_seed = seed;
            state.admin_token_hash = secret::hash(&token);
            Ok(())
        })?;
        Ok(token)
    }
}

/// Who a bearer token speaks for, and what it may do.
pub(crate) struct Caller {
    pub principal: String,
    pub device: String,
    pub scopes: Vec<Scope>,
    pub admin: bool,
}

impl Caller {
    pub fn require(&self, scope: Scope, detail: &'static str) -> Result<(), ControlError> {
        if self.scopes.contains(&scope) {
            Ok(())
        } else {
            Err(ControlError::Forbidden(detail))
        }
    }
}

pub(crate) fn require_initialized(state: &State) -> Result<(), ControlError> {
    if state.initialized() {
        Ok(())
    } else {
        Err(ControlError::NotInitialized)
    }
}

pub(crate) fn signing_key(state: &State) -> Result<SigningKey, ControlError> {
    require_initialized(state)?;
    let seed = secret::decode::<32>(&state.signing_seed)
        .ok_or_else(|| ControlError::Storage("signing key is missing or corrupt".into()))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Resolves a bearer token. The admin token is checked by hash; anything else must be a
/// signed access token that this control plane issued and that has not expired.
pub(crate) fn authenticate(state: &State, bearer: &str, now: u64) -> Result<Caller, ControlError> {
    require_initialized(state)?;
    if bearer.starts_with(ADMIN_PREFIX) {
        if secret::hash(bearer) != state.admin_token_hash {
            return Err(ControlError::Unauthorized("admin token is not valid"));
        }
        return Ok(Caller {
            principal: ADMIN_PRINCIPAL.into(),
            device: ADMIN_DEVICE.into(),
            scopes: SESSION_SCOPES.to_vec(),
            admin: true,
        });
    }
    let mut keys = KeySet::new();
    keys.trust(KID, signing_key(state)?.verifying_key())
        .map_err(|error| ControlError::Storage(error.to_string()))?;
    let claims =
        verify(&keys, bearer, now).map_err(|error| ControlError::Unauthorized(error.reason()))?;
    Ok(Caller {
        principal: claims.sub,
        device: claims.dev,
        scopes: claims.scopes,
        admin: false,
    })
}

/// Signs a one-hour access token for a session on `device`, for `repo` (`*` for discovery).
pub(crate) fn issue(
    state: &State,
    principal: &str,
    device: &str,
    repo: &str,
    now: u64,
) -> Result<String, ControlError> {
    let issuer = Issuer::new(KID, signing_key(state)?)
        .map_err(|error| ControlError::Storage(error.to_string()))?;
    let claims = Claims {
        sub: principal.into(),
        dev: device.into(),
        repo: repo.into(),
        scopes: SESSION_SCOPES.to_vec(),
        layer: None,
        iat: now,
        exp: now + MAX_LIFETIME,
        jti: secret::id("jti_"),
    };
    issuer
        .issue(&claims)
        .map_err(|error| ControlError::Storage(error.to_string()))
}
