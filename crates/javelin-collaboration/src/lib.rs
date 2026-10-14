//! Pure deterministic Publish Request review and permission state.
//!
//! No storage, network, identity provider, credential verification, or World
//! advancement. [`check_publishable`] evaluates the supplied current state only.
//! It is not an authorization token. Publication must recheck the exact revision,
//! permissions, and policy at its own correctness boundary.

use std::{collections::BTreeMap, fmt};

pub use javelin::{ObjectId, OperationId, VersionNumber};

/// Exact proposal: native base version, native manifest, and an optional opaque
/// context object. The context is an unvalidated reference; no trace kind,
/// storage, or confidentiality is implied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RevisionRef {
    base: VersionNumber,
    manifest: ObjectId,
    context: Option<ObjectId>,
}

impl RevisionRef {
    pub fn new(base: VersionNumber, manifest: ObjectId, context: Option<ObjectId>) -> Self {
        Self {
            base,
            manifest,
            context,
        }
    }
    pub fn base(&self) -> VersionNumber {
        self.base
    }
    pub fn manifest(&self) -> ObjectId {
        self.manifest
    }
    pub fn context(&self) -> Option<ObjectId> {
        self.context
    }
}

macro_rules! opaque_id {
    ($name:ident, $what:literal) => {
        #[doc = concat!("Opaque ", $what, ": 1..128 ASCII letters, digits, '-', '_', '.', or ':'.")]
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: &str) -> Result<Self, InvalidId> {
                if value.is_empty()
                    || value.len() > 128
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
                {
                    return Err(InvalidId($what));
                }
                Ok(Self(value.into()))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

opaque_id!(PrincipalId, "principal ID");
opaque_id!(RepositoryId, "repository ID");
opaque_id!(RequestId, "Publish Request ID");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidId(&'static str);

impl fmt::Display for InvalidId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} must contain 1..128 ASCII letters, digits, '-', '_', '.' or ':'",
            self.0
        )
    }
}

impl std::error::Error for InvalidId {}

/// Comment text: 1..=65536 bytes. Content is not interpreted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommentBody(String);

impl CommentBody {
    pub const MAX_BYTES: usize = 65536;
    pub fn parse(value: &str) -> Result<Self, InvalidComment> {
        if value.is_empty() || value.len() > Self::MAX_BYTES {
            return Err(InvalidComment);
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidComment;

impl fmt::Display for InvalidComment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "comment must contain 1..=65536 bytes")
    }
}

impl std::error::Error for InvalidComment {}

/// Ordered so each role includes the abilities of the roles before it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Role {
    /// Read, comment, and author Publish Requests.
    Read,
    /// Also approve other authors' revisions.
    Review,
    /// Also Publish.
    Maintain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Visibility {
    /// Anyone may read, comment, and author Publish Requests.
    Public,
    Private,
}

/// Permissions and review policy currently applied to one repository.
/// The supplier increments `epoch` on every change; older epochs are refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppliedPermissions {
    pub repository: RepositoryId,
    pub epoch: u64,
    pub visibility: Visibility,
    pub roles: BTreeMap<PrincipalId, Role>,
    /// Distinct current reviewers, other than the author, who must approve the
    /// exact current revision before Publish. Zero means reviews are optional.
    pub required_approvals: u32,
}

impl AppliedPermissions {
    /// Effective role from current grants only. Public visibility gives `Read`.
    pub fn role(&self, principal: &PrincipalId) -> Option<Role> {
        let granted = self.roles.get(principal).copied();
        match self.visibility {
            Visibility::Public => granted.or(Some(Role::Read)),
            Visibility::Private => granted,
        }
    }

    fn has(&self, principal: &PrincipalId, role: Role) -> bool {
        self.role(principal).is_some_and(|r| r >= role)
    }
}

/// One retried unit of work. The same operation ID must carry identical input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Command {
    pub operation: OperationId,
    pub actor: PrincipalId,
    pub action: Action,
}

/// Revision indexes start at 1. `expected` is the index the author last observed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Open {
        revision: RevisionRef,
    },
    Revise {
        expected: u32,
        revision: RevisionRef,
    },
    Approve {
        index: u32,
        revision: RevisionRef,
    },
    Comment {
        index: u32,
        body: CommentBody,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    RepositoryMismatch,
    StalePermissions { seen: u64 },
    OperationIdReused(OperationId),
    NotOpen,
    AlreadyOpen,
    NoAccess,
    InsufficientRole { required: Role },
    NotAuthor,
    SelfApproval,
    StaleRevision { current: u32 },
    UnknownRevision,
    RevisionMismatch,
    UnchangedRevision,
    AlreadyApproved,
    NotEnoughApprovals { required: u32, current: u32 },
}

impl Refusal {
    pub fn code(&self) -> &'static str {
        match self {
            Self::RepositoryMismatch => "repository_mismatch",
            Self::StalePermissions { .. } => "stale_permissions",
            Self::OperationIdReused(_) => "operation_id_reused",
            Self::NotOpen => "not_open",
            Self::AlreadyOpen => "already_open",
            Self::NoAccess => "no_access",
            Self::InsufficientRole { .. } => "insufficient_role",
            Self::NotAuthor => "not_author",
            Self::SelfApproval => "self_approval",
            Self::StaleRevision { .. } => "stale_revision",
            Self::UnknownRevision => "unknown_revision",
            Self::RevisionMismatch => "revision_mismatch",
            Self::UnchangedRevision => "unchanged_revision",
            Self::AlreadyApproved => "already_approved",
            Self::NotEnoughApprovals { .. } => "not_enough_approvals",
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StalePermissions { seen } => {
                write!(f, "stale_permissions: epoch {seen} was already applied")
            }
            Self::OperationIdReused(id) => {
                write!(f, "operation_id_reused: {id} was used with different input")
            }
            Self::InsufficientRole { required } => {
                write!(f, "insufficient_role: requires {required:?}")
            }
            Self::StaleRevision { current } => {
                write!(f, "stale_revision: current revision is {current}")
            }
            Self::NotEnoughApprovals { required, current } => {
                write!(f, "not_enough_approvals: {current} of {required}")
            }
            other => f.write_str(other.code()),
        }
    }
}

impl std::error::Error for Refusal {}

/// Immutable submitted revision. Approvals list principals, never roles, so
/// they are evaluated against current permissions when counted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Revision {
    pub index: u32,
    pub revision: RevisionRef,
    pub approvals: Vec<PrincipalId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Comment {
    pub author: PrincipalId,
    pub index: u32,
    pub body: CommentBody,
}

/// State of one Publish Request. Every transition returns a new value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Review {
    repository: RepositoryId,
    request: RequestId,
    author: Option<PrincipalId>,
    epoch: u64,
    revisions: Vec<Revision>,
    comments: Vec<Comment>,
    history: Vec<Command>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Applied {
    pub review: Review,
    /// True when an identical accepted command was retried; state is unchanged.
    pub replayed: bool,
}

impl Review {
    pub fn new(repository: RepositoryId, request: RequestId) -> Self {
        Self {
            repository,
            request,
            author: None,
            epoch: 0,
            revisions: Vec::new(),
            comments: Vec::new(),
            history: Vec::new(),
        }
    }

    pub fn repository(&self) -> &RepositoryId {
        &self.repository
    }
    pub fn request(&self) -> &RequestId {
        &self.request
    }
    pub fn author(&self) -> Option<&PrincipalId> {
        self.author.as_ref()
    }
    /// Highest permissions epoch applied so far.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn revisions(&self) -> &[Revision] {
        &self.revisions
    }
    pub fn current(&self) -> Option<&Revision> {
        self.revisions.last()
    }
    pub fn comments(&self) -> &[Comment] {
        &self.comments
    }
    /// Accepted commands in order. Refused commands are not recorded.
    pub fn history(&self) -> &[Command] {
        &self.history
    }

    /// Applies one command against the supplied current permissions.
    /// An identical retry of an accepted command returns the unchanged state.
    pub fn apply(
        &self,
        command: &Command,
        permissions: &AppliedPermissions,
    ) -> Result<Applied, Refusal> {
        if let Some(prior) = self
            .history
            .iter()
            .find(|c| c.operation == command.operation)
        {
            if prior == command {
                return Ok(Applied {
                    review: self.clone(),
                    replayed: true,
                });
            }
            return Err(Refusal::OperationIdReused(command.operation.clone()));
        }
        self.check_permissions(permissions)?;
        let actor = &command.actor;
        if permissions.role(actor).is_none() {
            return Err(Refusal::NoAccess);
        }
        let mut next = self.clone();
        match &command.action {
            Action::Open { revision } => {
                if self.author.is_some() {
                    return Err(Refusal::AlreadyOpen);
                }
                next.author = Some(actor.clone());
                next.push_revision(*revision);
            }
            Action::Revise { expected, revision } => {
                let current = self.current_open()?;
                if self.author.as_ref() != Some(actor) {
                    return Err(Refusal::NotAuthor);
                }
                if *expected != current.index {
                    return Err(Refusal::StaleRevision {
                        current: current.index,
                    });
                }
                if current.revision == *revision {
                    return Err(Refusal::UnchangedRevision);
                }
                next.push_revision(*revision);
            }
            Action::Approve { index, revision } => {
                let current = self.current_open()?;
                if !permissions.has(actor, Role::Review) {
                    return Err(Refusal::InsufficientRole {
                        required: Role::Review,
                    });
                }
                if self.author.as_ref() == Some(actor) {
                    return Err(Refusal::SelfApproval);
                }
                if *index != current.index {
                    return Err(Refusal::StaleRevision {
                        current: current.index,
                    });
                }
                if current.revision != *revision {
                    return Err(Refusal::RevisionMismatch);
                }
                if current.approvals.contains(actor) {
                    return Err(Refusal::AlreadyApproved);
                }
                let last = next
                    .revisions
                    .last_mut()
                    .expect("open request has a revision");
                last.approvals.push(actor.clone());
            }
            Action::Comment { index, body } => {
                self.current_open()?;
                if *index == 0 || *index as usize > self.revisions.len() {
                    return Err(Refusal::UnknownRevision);
                }
                next.comments.push(Comment {
                    author: actor.clone(),
                    index: *index,
                    body: body.clone(),
                });
            }
        }
        next.epoch = permissions.epoch;
        next.history.push(command.clone());
        Ok(Applied {
            review: next,
            replayed: false,
        })
    }

    fn check_permissions(&self, permissions: &AppliedPermissions) -> Result<(), Refusal> {
        if permissions.repository != self.repository {
            return Err(Refusal::RepositoryMismatch);
        }
        if permissions.epoch < self.epoch {
            return Err(Refusal::StalePermissions { seen: self.epoch });
        }
        Ok(())
    }

    fn current_open(&self) -> Result<&Revision, Refusal> {
        self.revisions.last().ok_or(Refusal::NotOpen)
    }

    fn push_revision(&mut self, revision: RevisionRef) {
        let index = u32::try_from(self.revisions.len() + 1).expect("revision count fits u32");
        self.revisions.push(Revision {
            index,
            revision,
            approvals: Vec::new(),
        });
    }
}

/// Result of evaluating the supplied current state. Not an authorization token:
/// it grants nothing and must not be stored or forwarded as proof.
#[derive(Debug, Eq, PartialEq)]
pub struct Publishable {
    pub index: u32,
    pub approvals: Vec<PrincipalId>,
}

/// Evaluates whether `publisher` may Publish `exact` under `permissions` now.
/// Counts approvals of the exact current revision by principals who currently
/// hold `Review`, excluding the author. Does not compare `exact.base()` with the
/// current World; publication handles that at its correctness boundary.
pub fn check_publishable(
    review: &Review,
    exact: &RevisionRef,
    permissions: &AppliedPermissions,
    publisher: &PrincipalId,
) -> Result<Publishable, Refusal> {
    review.check_permissions(permissions)?;
    let current = review.current_open()?;
    if current.revision != *exact {
        return Err(Refusal::RevisionMismatch);
    }
    if permissions.role(publisher).is_none() {
        return Err(Refusal::NoAccess);
    }
    if !permissions.has(publisher, Role::Maintain) {
        return Err(Refusal::InsufficientRole {
            required: Role::Maintain,
        });
    }
    let approvals: Vec<PrincipalId> = current
        .approvals
        .iter()
        .filter(|p| review.author.as_ref() != Some(*p) && permissions.has(p, Role::Review))
        .cloned()
        .collect();
    let count = u32::try_from(approvals.len()).unwrap_or(u32::MAX);
    if count < permissions.required_approvals {
        return Err(Refusal::NotEnoughApprovals {
            required: permissions.required_approvals,
            current: count,
        });
    }
    Ok(Publishable {
        index: current.index,
        approvals,
    })
}
