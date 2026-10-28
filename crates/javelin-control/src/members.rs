//! Membership (`docs/online-design.md` section 4.7): who holds which role in a repository, and
//! the invites that bring a new person in. The control plane keeps each repository's members
//! and their epoch. Every change bumps the epoch, then pushes the whole applied permissions to
//! the repository's node, which applies them from its next request. A Maintain holder, or the
//! admin token, makes changes; any member may list them.
//!
//! A member is named by an account the control plane already has: the admin, a passkey's
//! principal, or an account an invite made. For anyone else, `invite` returns a one-time code
//! for an account name that does not exist yet. Redeeming it makes that account, signs the
//! redeeming device in as it, and grants the invited role.
//!
//! The push runs under the control plane's lock and always sends the current state, so pushes
//! reach the node in epoch order. A change whose push fails stays saved; repeating the command
//! changes nothing more and pushes again.

use std::collections::BTreeMap;

use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use serde_json::{Value, json};

use crate::{
    ADMIN_PRINCIPAL, ControlError, ControlPlane, NodeTarget, Session,
    control::{Caller, authenticate},
    device::open_session,
    secret,
    state::{AccountRecord, InviteRecord, Phase, RepositoryRecord, State},
};

/// How long an invite code can wait to be redeemed: seven days.
const INVITE_TTL: u64 = 7 * 24 * 3600;
/// Prefix that marks an invite code, so it is never parsed as a JWS.
const INVITE_PREFIX: &str = "jav_invite_";

/// One member and their role.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Member {
    pub principal: String,
    pub role: Role,
}

/// An invite that has not been redeemed. Its code is never shown again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingInvite {
    pub principal: String,
    pub role: Role,
    pub expires_at: u64,
}

/// A repository's members as the control plane holds them, at `epoch`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Members {
    pub repository: String,
    pub owner: String,
    pub name: String,
    pub epoch: u64,
    pub required_approvals: u32,
    /// Sorted by principal.
    pub members: Vec<Member>,
    /// Sorted by principal.
    pub invites: Vec<PendingInvite>,
    /// False when the call changed nothing, as a retry does.
    pub changed: bool,
}

impl Members {
    /// The members as the API answers them, and as `javelin member --json` prints them.
    pub fn to_json(&self) -> Value {
        json!({
            "repository": self.repository,
            "owner": self.owner,
            "name": self.name,
            "epoch": self.epoch,
            "required_approvals": self.required_approvals,
            "members": self.members.iter().map(|member| json!({
                "principal": member.principal,
                "role": member.role.name(),
            })).collect::<Vec<_>>(),
            "invites": self.invites.iter().map(|invite| json!({
                "principal": invite.principal,
                "role": invite.role.name(),
                "expires_at": invite.expires_at,
            })).collect::<Vec<_>>(),
            "changed": self.changed,
        })
    }
}

/// A change to one member.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemberChange {
    /// Grants a role to an account that holds none. Repeating it changes nothing.
    Add(Role),
    /// Changes a member's role.
    Role(Role),
    /// Removes a member. Removing someone who is not a member changes nothing.
    Remove,
}

/// A new invite. `code` is shown once; the control plane keeps only its hash.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Invite {
    pub code: String,
    pub repository: String,
    pub principal: String,
    pub role: Role,
    pub expires_at: u64,
}

/// A redeemed invite: the new account's session and the membership it holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Redeemed {
    pub session: Session,
    pub repository: String,
    pub role: Role,
    /// Why the node did not take the new permissions yet, if it did not. The membership is
    /// saved, and the next member change pushes it again.
    pub node_error: Option<String>,
}

impl ControlPlane {
    /// The members of `repository`, by ID or `OWNER/NAME`. The caller must be a member, or the
    /// admin.
    pub fn members(
        &self,
        bearer: &str,
        repository: &str,
        now: u64,
    ) -> Result<Members, ControlError> {
        self.store.read(|state| {
            let caller = authenticate(state, bearer, now)?;
            let (id, record) = find(state, repository)?;
            if !caller.may_act_on(&id) {
                return Err(ControlError::Forbidden("token is for another repository"));
            }
            if !caller.admin && role_of(record, &caller.principal).is_none() {
                return Err(ControlError::Forbidden(
                    "caller has no role in this repository",
                ));
            }
            Ok(view(state, &id, record, false))
        })
    }

    /// Adds, changes or removes `principal`'s role in `repository`. The caller must hold
    /// Maintain, or be the admin. A repository always keeps one Maintain holder.
    pub fn change_member(
        &self,
        bearer: &str,
        repository: &str,
        principal: &str,
        change: MemberChange,
        now: u64,
    ) -> Result<Members, ControlError> {
        let principal = account_name(principal)?;
        let (id, changed) = self.store.update(|state| {
            let (id, _) = self.maintained(state, bearer, repository, now)?;
            let known = known_account(state, &principal);
            let record = record_mut(state, &id)?;
            let mut roles = roles(record);
            let current = roles.get(&principal).copied();
            match (change, current) {
                (MemberChange::Add(role), None) => {
                    if !known {
                        return Err(ControlError::NotFound(
                            "no account has this name; invite them with `javelin member invite`",
                        ));
                    }
                    roles.insert(principal.clone(), role);
                }
                (MemberChange::Add(role), Some(held)) if held != role => {
                    return Err(ControlError::Conflict(format!(
                        "{principal} already holds {}; change it with `javelin member role`",
                        held.name()
                    )));
                }
                (MemberChange::Role(_), None) => {
                    return Err(ControlError::NotFound(
                        "not a member of this repository; add them with `javelin member add`",
                    ));
                }
                (MemberChange::Role(role), Some(_)) => {
                    roles.insert(principal.clone(), role);
                }
                (MemberChange::Remove, _) => {
                    roles.remove(&principal);
                }
                (MemberChange::Add(_), Some(_)) => {}
            }
            if !roles.values().any(|role| *role == Role::Maintain) {
                return Err(ControlError::InvalidInput(
                    "a repository keeps at least one maintain member; give the role to someone else first".into(),
                ));
            }
            let changed = set_roles(record, &roles);
            Ok((id, changed))
        })?;
        self.push(&id, changed)
    }

    /// Sets the approvals a Publish Request needs in `repository`. The caller must hold
    /// Maintain, or be the admin.
    pub fn set_required_approvals(
        &self,
        bearer: &str,
        repository: &str,
        required: u32,
        now: u64,
    ) -> Result<Members, ControlError> {
        let (id, changed) = self.store.update(|state| {
            let (id, _) = self.maintained(state, bearer, repository, now)?;
            let record = record_mut(state, &id)?;
            let changed = record.required_approvals != required;
            if changed {
                record.required_approvals = required;
                record.epoch += 1;
            }
            Ok((id, changed))
        })?;
        self.push(&id, changed)
    }

    /// Invites a new account named `principal` into `repository` as `role`. The caller must
    /// hold Maintain, or be the admin. An account that already exists is added with
    /// [`ControlPlane::change_member`] instead. A new invite replaces a pending one for the same
    /// name and repository.
    pub fn invite(
        &self,
        bearer: &str,
        repository: &str,
        principal: &str,
        role: Role,
        now: u64,
    ) -> Result<Invite, ControlError> {
        let principal = account_name(principal)?;
        let code = secret::token(INVITE_PREFIX);
        self.store.update(|state| {
            let (id, caller) = self.maintained(state, bearer, repository, now)?;
            if known_account(state, &principal) {
                return Err(ControlError::Conflict(format!(
                    "{principal} already has an account; add them with `javelin member add`"
                )));
            }
            state.invites.retain(|_, invite| {
                invite.expires_at > now
                    && !(invite.repository == id && invite.principal == principal)
            });
            let expires_at = now + INVITE_TTL;
            state.invites.insert(
                secret::hash(&code),
                InviteRecord {
                    repository: id.clone(),
                    principal: principal.clone(),
                    role: role.name().into(),
                    invited_by: caller.principal,
                    expires_at,
                },
            );
            Ok(Invite {
                code: code.clone(),
                repository: id,
                principal: principal.clone(),
                role,
                expires_at,
            })
        })
    }

    /// Redeems an invite code once: makes its account, grants its role, and signs a new device
    /// in as that account. An account that came to exist since the invite was made is refused,
    /// so a code never signs anyone in as an existing person.
    pub fn redeem_invite(&self, code: &str, now: u64) -> Result<Redeemed, ControlError> {
        let (session, id, role) =
            self.store.update(|state| {
                let invite = state.invites.get(&secret::hash(code)).cloned().ok_or(
                    ControlError::NotFound("this invite code is not known or was already used"),
                )?;
                if now >= invite.expires_at {
                    return Err(ControlError::Expired(
                        "this invite has expired; ask for a new one",
                    ));
                }
                let record = state
                    .repositories
                    .get(&invite.repository)
                    .ok_or(ControlError::NotFound("the invited repository is gone"))?;
                self.check_node(record)?;
                if known_account(state, &invite.principal) {
                    return Err(ControlError::Conflict(format!(
                        "{} already has an account; ask to be added with `javelin member add`",
                        invite.principal
                    )));
                }
                let role = Role::parse(&invite.role).ok_or_else(|| {
                    ControlError::Storage("an invite names an unknown role".into())
                })?;
                state.invites.remove(&secret::hash(code));
                state.accounts.insert(
                    invite.principal.clone(),
                    AccountRecord {
                        created_at: now,
                        invited_to: invite.repository.clone(),
                    },
                );
                let record = record_mut(state, &invite.repository)?;
                let mut roles = roles(record);
                roles.insert(invite.principal.clone(), role);
                set_roles(record, &roles);
                let session = open_session(state, invite.principal, secret::id("dev_"), now)?;
                Ok((session, invite.repository, role))
            })?;
        let node_error = self.push(&id, true).err().map(|error| error.to_string());
        Ok(Redeemed {
            session,
            repository: id,
            role,
            node_error,
        })
    }

    /// The node `repository` (by ID or `OWNER/NAME`) lives on, so a client can reach its node
    /// to push to it.
    pub fn node_target(&self, repository: &str) -> Result<NodeTarget, ControlError> {
        self.store
            .read(|state| find(state, repository).map(|(_, record)| target_of(record)))
    }

    /// Authenticates a caller who may change `repository`'s members: a Maintain holder, or
    /// the admin. The repository must be ready, on the node this control plane pushes to.
    fn maintained(
        &self,
        state: &State,
        bearer: &str,
        repository: &str,
        now: u64,
    ) -> Result<(String, Caller), ControlError> {
        let caller = authenticate(state, bearer, now)?;
        let (id, record) = find(state, repository)?;
        if !caller.may_act_on(&id) {
            return Err(ControlError::Forbidden("token is for another repository"));
        }
        if !caller.admin && role_of(record, &caller.principal) != Some(Role::Maintain) {
            return Err(ControlError::Forbidden(
                "changing members needs the maintain role",
            ));
        }
        if record.phase != Phase::Ready {
            return Err(ControlError::InvalidInput(
                "the repository is still provisioning".into(),
            ));
        }
        self.check_node(record)?;
        Ok((id, caller))
    }

    /// Refuses a repository whose node is not the one this control plane pushes to, so a push
    /// never lands on another node.
    fn check_node(&self, record: &RepositoryRecord) -> Result<(), ControlError> {
        let target = target_of(record);
        if target == self.target {
            return Ok(());
        }
        let url = target
            .url
            .as_deref()
            .unwrap_or("this control plane's directory");
        Err(ControlError::InvalidInput(format!(
            "{}/{}'s node is {} at {url}; change its members from a control plane that reaches \
             that node (for cloudflare, without JAVELIN_CONTROL_URL and with \
             JAVELIN_NODE_ADMIN_TOKEN set)",
            record.owner, record.name, target.provider
        )))
    }

    /// Pushes `repository`'s current permissions to its node, under the lock, so the node
    /// receives epochs in order. Returns the members as pushed.
    fn push(&self, repository: &str, changed: bool) -> Result<Members, ControlError> {
        self.store.read(|state| {
            let record = state
                .repositories
                .get(repository)
                .ok_or(ControlError::NotFound("no such repository"))?;
            let id = RepositoryId::parse(repository)
                .map_err(|error| ControlError::Storage(error.to_string()))?;
            let permissions = permissions(&id, record, record.epoch)?;
            self.node.apply(&permissions).map_err(|error| {
                ControlError::Node(format!(
                    "epoch {} is saved, but the node did not take it: {error}; run the command \
                     again to push it",
                    record.epoch
                ))
            })?;
            Ok(view(state, repository, record, changed))
        })
    }
}

/// `principal`'s role in `record`, if any.
pub(crate) fn role_of(record: &RepositoryRecord, principal: &str) -> Option<Role> {
    roles(record).get(principal).copied()
}

/// The roles `record` grants. A record from before membership grants its creator Maintain.
pub(crate) fn roles(record: &RepositoryRecord) -> BTreeMap<String, Role> {
    if record.members.is_empty() {
        return BTreeMap::from([(record.creator.clone(), Role::Maintain)]);
    }
    record
        .members
        .iter()
        .filter_map(|(principal, role)| Some((principal.clone(), Role::parse(role)?)))
        .collect()
}

/// Stores `roles` in `record`, bumping its epoch if they changed. Returns whether they did.
fn set_roles(record: &mut RepositoryRecord, roles: &BTreeMap<String, Role>) -> bool {
    let stored: BTreeMap<String, String> = roles
        .iter()
        .map(|(principal, role)| (principal.clone(), role.name().to_string()))
        .collect();
    if stored == record.members {
        return false;
    }
    let changed = roles != &self::roles(record);
    record.members = stored;
    if changed {
        record.epoch += 1;
    }
    changed
}

/// The permissions `record` applies at `epoch`.
pub(crate) fn permissions(
    repository: &RepositoryId,
    record: &RepositoryRecord,
    epoch: u64,
) -> Result<AppliedPermissions, ControlError> {
    let roles = roles(record)
        .into_iter()
        .map(|(principal, role)| {
            PrincipalId::parse(&principal)
                .map(|principal| (principal, role))
                .map_err(|error| ControlError::Storage(error.to_string()))
        })
        .collect::<Result<_, _>>()?;
    Ok(AppliedPermissions {
        repository: repository.clone(),
        epoch,
        visibility: if record.public {
            Visibility::Public
        } else {
            Visibility::Private
        },
        roles,
        required_approvals: record.required_approvals,
    })
}

/// The repository named by ID or `OWNER/NAME`.
fn find<'a>(
    state: &'a State,
    repository: &str,
) -> Result<(String, &'a RepositoryRecord), ControlError> {
    if let Some(record) = state.repositories.get(repository) {
        return Ok((repository.to_string(), record));
    }
    if let Some((owner, name)) = repository.split_once('/') {
        let found = state
            .repositories
            .iter()
            .find(|(_, record)| record.owner == owner && record.name == name);
        if let Some((id, record)) = found {
            return Ok((id.clone(), record));
        }
    }
    Err(ControlError::NotFound("no such repository"))
}

fn record_mut<'a>(
    state: &'a mut State,
    repository: &str,
) -> Result<&'a mut RepositoryRecord, ControlError> {
    state
        .repositories
        .get_mut(repository)
        .ok_or(ControlError::NotFound("no such repository"))
}

fn target_of(record: &RepositoryRecord) -> NodeTarget {
    match record.provider.as_str() {
        "" | "self" => NodeTarget::self_hosted(),
        provider => NodeTarget {
            provider: provider.into(),
            url: record.node_url.clone(),
        },
    }
}

/// Whether the control plane has an account named `principal`: the admin, a passkey's
/// principal, a signed-in principal, an account an invite made, or a repository's creator.
fn known_account(state: &State, principal: &str) -> bool {
    principal == ADMIN_PRINCIPAL
        || state.accounts.contains_key(principal)
        || state
            .passkeys
            .values()
            .any(|record| record.principal == principal)
        || state
            .sessions
            .values()
            .any(|session| session.principal == principal)
        || state
            .repositories
            .values()
            .any(|record| record.creator == principal)
}

/// An account name: a principal ID (`PrincipalId`), which tokens carry as their subject.
fn account_name(principal: &str) -> Result<String, ControlError> {
    PrincipalId::parse(principal)
        .map(|_| principal.to_string())
        .map_err(|_| {
            ControlError::InvalidInput(
                "an account name has 1 to 128 ASCII letters, digits, '-', '_', '.' or ':'".into(),
            )
        })
}

fn view(state: &State, repository: &str, record: &RepositoryRecord, changed: bool) -> Members {
    let members = roles(record)
        .into_iter()
        .map(|(principal, role)| Member { principal, role })
        .collect();
    let mut invites: Vec<PendingInvite> = state
        .invites
        .values()
        .filter(|invite| invite.repository == repository)
        .filter_map(|invite| {
            Some(PendingInvite {
                principal: invite.principal.clone(),
                role: Role::parse(&invite.role)?,
                expires_at: invite.expires_at,
            })
        })
        .collect();
    invites.sort_by(|a, b| a.principal.cmp(&b.principal));
    Members {
        repository: repository.to_string(),
        owner: record.owner.clone(),
        name: record.name.clone(),
        epoch: record.epoch,
        required_approvals: record.required_approvals,
        members,
        invites,
        changed,
    }
}
