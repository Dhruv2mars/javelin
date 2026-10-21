//! Repository create. It is idempotent per operation ID and provisions the node in steps.
//! A crash leaves the repository `Provisioning`; a retry with the same operation ID resumes
//! it, and every step it repeats is idempotent. Forks reuse these steps (see `fork.rs`).

use std::collections::BTreeMap;

use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_identity::Scope;

use crate::{
    ControlError, ControlPlane,
    control::{authenticate, issue},
    device::{ACCESS_TTL, AccessToken},
    secret,
    state::{ForkOf, Phase, RepositoryRecord, State},
};

/// How a repository accepts changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Publish {
    /// Maintainers Publish directly.
    Direct,
    /// Changes go through Publish Requests.
    Requests,
}

impl Publish {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "direct" => Some(Self::Direct),
            "requests" => Some(Self::Requests),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Requests => "requests",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateRequest {
    /// Lowercase slug: letters, digits and dashes, starting with a letter or digit.
    pub owner: String,
    pub name: String,
    pub public: bool,
    pub publish: Publish,
    /// Reuse this on retry. It names one request for good.
    pub operation: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Created {
    pub repository: String,
    pub owner: String,
    pub name: String,
    pub public: bool,
    pub publish: Publish,
    pub epoch: u64,
    /// False when the operation had already created this repository.
    pub created: bool,
}

impl ControlPlane {
    /// Issues an access token for `repository`, which a node accepts. Only the creator and the
    /// admin may ask for one until membership exists; the node still applies the role.
    pub fn repository_token(
        &self,
        bearer: &str,
        repository: &str,
        now: u64,
    ) -> Result<AccessToken, ControlError> {
        self.store.read(|state| {
            let caller = authenticate(state, bearer, now)?;
            if !caller.may_act_on(repository) {
                return Err(ControlError::Forbidden("token is for another repository"));
            }
            let record = state
                .repositories
                .get(repository)
                .ok_or(ControlError::NotFound("no such repository"))?;
            if !caller.admin && caller.principal != record.creator {
                return Err(ControlError::Forbidden(
                    "caller has no role in this repository",
                ));
            }
            Ok(AccessToken {
                access_token: issue(state, &caller.principal, &caller.device, repository, now)?,
                expires_in: ACCESS_TTL,
            })
        })
    }

    /// Creates `owner/name`. The caller needs the `repo:create` scope. Repeating the same
    /// operation ID with the same request returns the same repository.
    pub fn create_repository(
        &self,
        bearer: &str,
        request: &CreateRequest,
        now: u64,
    ) -> Result<Created, ControlError> {
        validate_request(request)?;
        let (repository, record, inserted) = self.store.update(|state| {
            let caller = authenticate(state, bearer, now)?;
            if caller.repo != "*" {
                return Err(ControlError::Forbidden(
                    "create needs a sign-in token, not a repository token",
                ));
            }
            caller.require(Scope::RepoCreate, "token does not carry repo:create")?;
            reserve(state, &caller.principal, request, None)
        })?;

        let repository_id = RepositoryId::parse(&repository)
            .map_err(|error| ControlError::Storage(error.to_string()))?;
        if record.phase == Phase::Ready {
            return Ok(created(&repository, &record, inserted));
        }
        self.node.provision(&repository_id)?;
        self.node
            .apply(&initial_permissions(&repository_id, &record)?)?;
        let ready = self.mark_ready(&repository)?;
        Ok(created(&repository, &ready, inserted))
    }

    /// Marks a repository Ready once its node holds the initial permissions.
    pub(crate) fn mark_ready(&self, repository: &str) -> Result<RepositoryRecord, ControlError> {
        self.store.update(|state| {
            let record = state
                .repositories
                .get_mut(repository)
                .ok_or_else(|| ControlError::Storage("repository vanished during create".into()))?;
            record.phase = Phase::Ready;
            record.epoch = 1;
            Ok(record.clone())
        })
    }
}

/// Reserves `request`'s name and operation as a new Provisioning record created by `creator`.
/// A retry with the same operation and request returns the record reserved first. A fork
/// passes its origin, so a create and a fork never share an operation.
pub(crate) fn reserve(
    state: &mut State,
    creator: &str,
    request: &CreateRequest,
    fork: Option<ForkOf>,
) -> Result<(String, RepositoryRecord, bool), ControlError> {
    let publish = request.publish.as_str();
    if let Some(id) = state.operations.get(&request.operation) {
        let record = state
            .repositories
            .get(id)
            .ok_or_else(|| ControlError::Storage("operation names a missing repository".into()))?;
        let same = record.owner == request.owner
            && record.name == request.name
            && record.public == request.public
            && record.publish == publish
            && record.fork == fork
            && record.creator == creator;
        if !same {
            return Err(ControlError::OperationReused);
        }
        return Ok((id.clone(), record.clone(), false));
    }
    if state
        .repositories
        .values()
        .any(|record| record.owner == request.owner && record.name == request.name)
    {
        return Err(ControlError::NameTaken);
    }
    let id = secret::id("repo_");
    let record = RepositoryRecord {
        owner: request.owner.clone(),
        name: request.name.clone(),
        public: request.public,
        publish: publish.into(),
        creator: creator.into(),
        operation: request.operation.clone(),
        phase: Phase::Provisioning,
        epoch: 0,
        fork,
    };
    state.repositories.insert(id.clone(), record.clone());
    state
        .operations
        .insert(request.operation.clone(), id.clone());
    Ok((id, record, true))
}

/// The permissions a new repository starts with: its creator as Maintain, at epoch 1.
pub(crate) fn initial_permissions(
    repository: &RepositoryId,
    record: &RepositoryRecord,
) -> Result<AppliedPermissions, ControlError> {
    Ok(AppliedPermissions {
        repository: repository.clone(),
        epoch: 1,
        visibility: if record.public {
            Visibility::Public
        } else {
            Visibility::Private
        },
        roles: creator_roles(&record.creator)?,
        required_approvals: 0,
    })
}

fn creator_roles(creator: &str) -> Result<BTreeMap<PrincipalId, Role>, ControlError> {
    let principal =
        PrincipalId::parse(creator).map_err(|error| ControlError::Storage(error.to_string()))?;
    Ok(BTreeMap::from([(principal, Role::Maintain)]))
}

pub(crate) fn created(repository: &str, record: &RepositoryRecord, inserted: bool) -> Created {
    Created {
        repository: repository.into(),
        owner: record.owner.clone(),
        name: record.name.clone(),
        public: record.public,
        publish: if record.publish == "requests" {
            Publish::Requests
        } else {
            Publish::Direct
        },
        epoch: record.epoch,
        created: inserted,
    }
}

pub(crate) fn validate_request(request: &CreateRequest) -> Result<(), ControlError> {
    validate_slug("owner", &request.owner)?;
    validate_slug("repository name", &request.name)?;
    validate_operation(&request.operation)
}

fn validate_slug(what: &str, value: &str) -> Result<(), ControlError> {
    let valid = !value.is_empty()
        && value.len() <= 39
        && value.as_bytes()[0] != b'-'
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if valid {
        Ok(())
    } else {
        Err(ControlError::InvalidInput(format!(
            "{what} must be 1 to 39 lowercase letters, digits or dashes, not starting with a dash"
        )))
    }
}

fn validate_operation(value: &str) -> Result<(), ControlError> {
    PrincipalId::parse(value)
        .map(|_| ())
        .or_else(|_| RepositoryId::parse(value).map(|_| ()))
        .map_err(|_| {
            ControlError::InvalidInput(
                "operation ID must be 1 to 128 ASCII letters, digits, '-', '_', '.' or ':'".into(),
            )
        })
}
