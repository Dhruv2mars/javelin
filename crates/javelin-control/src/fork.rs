//! Fork: a new repository that inherits another's World log up to version N (`docs/online-design.md`
//! section 4.9, U11). Anyone who can read the source may fork it (D13). The fork takes the node's
//! copy, then the same provisioning steps as create, so a retry resumes where it stopped.

use javelin::ObjectId;
use javelin_collaboration::{RepositoryId, Role};
use javelin_identity::Scope;
use javelin_node::fork::Origin;

use crate::{
    ControlError, ControlPlane,
    control::{ForkPlan, authenticate},
    repo::{CreateRequest, Created, created, initial_permissions, reserve, validate_request},
    state::{ForkOf, Phase},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForkRequest {
    /// The repository to fork, by ID.
    pub source: String,
    /// The World version to fork at. Its ID and tree carry over unchanged.
    pub at: u64,
    /// The new repository. Its operation ID is the fork's operation.
    pub target: CreateRequest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForkedRepository {
    pub repository: Created,
    /// `fork_of`: the source, the version forked at, and that version's ID.
    pub origin: Origin,
    /// Traces the caller could not read, left out of the fork and marked there.
    pub omitted: Vec<ObjectId>,
}

impl ControlPlane {
    /// Forks `request.source` at `request.at` into a new repository. Repeating the same request
    /// with the same operation ID finishes an interrupted fork and returns the same repository.
    pub fn fork_repository(
        &self,
        bearer: &str,
        request: &ForkRequest,
        now: u64,
    ) -> Result<ForkedRepository, ControlError> {
        validate_request(&request.target)?;
        let (repository, record, inserted, reader) = self.store.update(|state| {
            let caller = authenticate(state, bearer, now)?;
            if caller.repo != "*" {
                return Err(ControlError::Forbidden(
                    "fork needs a sign-in token, not a repository token",
                ));
            }
            caller.require(Scope::RepoCreate, "token does not carry repo:create")?;
            let source = state
                .repositories
                .get(&request.source)
                .ok_or(ControlError::NotFound("no such repository"))?;
            if source.phase != Phase::Ready {
                return Err(ControlError::InvalidInput(
                    "the source repository is still provisioning".into(),
                ));
            }
            let reader = if source.creator == caller.principal {
                Role::Maintain
            } else if source.public {
                Role::Read
            } else {
                return Err(ControlError::Forbidden(
                    "caller has no role in this repository",
                ));
            };
            let source_id = RepositoryId::parse(&request.source)
                .map_err(|error| ControlError::Storage(error.to_string()))?;
            let head = self.node.head(&source_id)?;
            if request.at > head.get() {
                return Err(ControlError::InvalidInput(format!(
                    "fork point {} is past the source's head {head}",
                    request.at
                )));
            }
            let fork = ForkOf {
                source: request.source.clone(),
                number: request.at,
            };
            let (id, record, inserted) =
                reserve(state, &caller.principal, &request.target, Some(fork))?;
            Ok((id, record, inserted, reader))
        })?;

        let repository_id = RepositoryId::parse(&repository)
            .map_err(|error| ControlError::Storage(error.to_string()))?;
        let source_id = RepositoryId::parse(&request.source)
            .map_err(|error| ControlError::Storage(error.to_string()))?;
        self.node.provision(&repository_id)?;
        let forked = self.node.fork(&ForkPlan {
            source: source_id,
            target: repository_id.clone(),
            at: request.at,
            reader,
        })?;
        if record.phase == Phase::Ready {
            // A retry after a lost reply: the permissions and the Ready mark are already done,
            // and marking again would reset the epoch.
            return Ok(ForkedRepository {
                repository: created(&repository, &record, inserted),
                origin: forked.origin,
                omitted: forked.omitted,
            });
        }
        self.node
            .apply(&initial_permissions(&repository_id, &record)?)?;
        let ready = self.mark_ready(&repository)?;
        Ok(ForkedRepository {
            repository: created(&repository, &ready, inserted),
            origin: forked.origin,
            omitted: forked.omitted,
        })
    }
}
