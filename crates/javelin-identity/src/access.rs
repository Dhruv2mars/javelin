//! Applied permissions, the revocation set, and the decision for one request. Effective
//! rights are the token's scopes intersected with what the caller's role allows at the
//! applied epoch. Tokens carry no role, so a role change applies as soon as the node
//! applies the newer epoch, whatever the token still says.

use std::{cmp::Ordering, collections::BTreeSet, error::Error, fmt};

use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use serde::{Deserialize, Serialize};

use crate::{KeySet, TokenError, token::verify};

/// A capability a token can carry. The serialized names are the design's scope names.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum Scope {
    #[serde(rename = "read")]
    Read,
    #[serde(rename = "layer:write")]
    LayerWrite,
    #[serde(rename = "publish")]
    Publish,
    #[serde(rename = "review:write")]
    ReviewWrite,
    #[serde(rename = "review:approve")]
    ReviewApprove,
    #[serde(rename = "repo:create")]
    RepoCreate,
    #[serde(rename = "import")]
    Import,
}

/// What one request needs: a scope, and the Layer it acts on, if it acts on one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Need<'a> {
    pub scope: Scope,
    pub layer: Option<&'a str>,
}

/// Who a request runs as. `principal` is `None` for an anonymous read of a public
/// repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Grant {
    pub principal: Option<String>,
    pub role: Role,
}

/// A refusal, with its protocol code. `Unauthorized` means the caller is not
/// authenticated; `Forbidden` means it is, but may not do this.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    Unauthorized(&'static str),
    Forbidden(&'static str),
    StalePermissions { seen: u64 },
}

impl Refusal {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized(_) => "unauthorized",
            Self::Forbidden(_) => "forbidden",
            Self::StalePermissions { .. } => "stale_permissions",
        }
    }
}

impl Refusal {
    /// Why the request was refused, without the code.
    pub fn detail(&self) -> String {
        match self {
            Self::Unauthorized(reason) | Self::Forbidden(reason) => (*reason).to_string(),
            Self::StalePermissions { seen } => format!("epoch {seen} was already applied"),
        }
    }
}

impl From<TokenError> for Refusal {
    fn from(error: TokenError) -> Self {
        Self::Unauthorized(error.reason())
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.detail())
    }
}

impl Error for Refusal {}

/// A control-plane revocation. Tokens are refused by `jti`, or every token from a device.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Revocation {
    Token(String),
    Device(String),
}

/// Scopes each repository role may exercise. `repo:create` is a discovery scope that no
/// repository role grants, so a node never honors it.
fn role_allows(role: Role, scope: Scope) -> bool {
    match scope {
        Scope::Read | Scope::LayerWrite | Scope::ReviewWrite => true,
        Scope::ReviewApprove => role >= Role::Review,
        Scope::Publish | Scope::Import => role >= Role::Maintain,
        Scope::RepoCreate => false,
    }
}

/// The node's view of one repository: the keys it trusts, the applied permissions, and the
/// revocation set. Everything here is held in memory and comes from the control plane, so
/// a restart forgets revocations until they are pushed again.
#[derive(Debug)]
pub struct Access {
    repository: RepositoryId,
    keys: KeySet,
    permissions: Option<AppliedPermissions>,
    revoked_tokens: BTreeSet<String>,
    revoked_devices: BTreeSet<String>,
}

impl Access {
    pub fn new(repository: RepositoryId, keys: KeySet) -> Self {
        Self {
            repository,
            keys,
            permissions: None,
            revoked_tokens: BTreeSet::new(),
            revoked_devices: BTreeSet::new(),
        }
    }

    pub fn keys_mut(&mut self) -> &mut KeySet {
        &mut self.keys
    }

    /// The applied permissions, or `None` before the first permissions push.
    pub fn permissions(&self) -> Option<&AppliedPermissions> {
        self.permissions.as_ref()
    }

    /// The applied epoch, or `None` before the first permissions push.
    pub fn epoch(&self) -> Option<u64> {
        self.permissions.as_ref().map(|p| p.epoch)
    }

    /// Applies a permissions push. A newer epoch replaces the state. Re-sending the applied
    /// epoch is accepted only when it is identical. Any other push is refused and changes
    /// nothing.
    pub fn apply(&mut self, next: AppliedPermissions) -> Result<(), Refusal> {
        if next.repository != self.repository {
            return Err(Refusal::Forbidden("permissions are for another repository"));
        }
        if let Some(current) = &self.permissions {
            match next.epoch.cmp(&current.epoch) {
                Ordering::Less => {
                    return Err(Refusal::StalePermissions {
                        seen: current.epoch,
                    });
                }
                Ordering::Equal if *current == next => return Ok(()),
                Ordering::Equal => {
                    return Err(Refusal::StalePermissions {
                        seen: current.epoch,
                    });
                }
                Ordering::Greater => {}
            }
        }
        self.permissions = Some(next);
        Ok(())
    }

    pub fn revoke(&mut self, target: Revocation) {
        match target {
            Revocation::Token(jti) => self.revoked_tokens.insert(jti),
            Revocation::Device(device) => self.revoked_devices.insert(device),
        };
    }

    /// Decides one request. `token` is the bearer token, or `None` for an anonymous caller.
    /// An anonymous caller may only read a public repository.
    pub fn authorize(
        &self,
        token: Option<&str>,
        now: u64,
        need: Need<'_>,
    ) -> Result<Grant, Refusal> {
        let Some(token) = token else {
            return self.anonymous(need);
        };
        let claims = verify(&self.keys, token, now)?;
        if self.revoked_tokens.contains(&claims.jti) || self.revoked_devices.contains(&claims.dev) {
            return Err(Refusal::Unauthorized("token is revoked"));
        }
        if claims.repo != self.repository.as_str() {
            return Err(Refusal::Forbidden("token is for another repository"));
        }
        let permissions = self
            .permissions
            .as_ref()
            .ok_or(Refusal::Forbidden("no permissions are applied yet"))?;
        let principal = PrincipalId::parse(&claims.sub)
            .map_err(|_| Refusal::Unauthorized("token subject is not an opaque ID"))?;
        let role = permissions
            .role(&principal)
            .ok_or(Refusal::Forbidden("caller has no role in this repository"))?;
        if !claims.scopes.contains(&need.scope) {
            return Err(Refusal::Forbidden("token does not carry this scope"));
        }
        if !role_allows(role, need.scope) {
            return Err(Refusal::Forbidden(
                "caller's role does not allow this scope",
            ));
        }
        if let Some(layer) = &claims.layer
            && need.layer != Some(layer.as_str())
        {
            return Err(Refusal::Forbidden("token is limited to another Layer"));
        }
        Ok(Grant {
            principal: Some(claims.sub),
            role,
        })
    }

    fn anonymous(&self, need: Need<'_>) -> Result<Grant, Refusal> {
        match &self.permissions {
            Some(permissions)
                if permissions.visibility == Visibility::Public && need.scope == Scope::Read =>
            {
                Ok(Grant {
                    principal: None,
                    role: Role::Read,
                })
            }
            _ => Err(Refusal::Unauthorized("a token is required")),
        }
    }
}
