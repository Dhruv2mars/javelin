//! Pure deterministic Publish Request review and permission state.
//!
//! No storage, network, identity provider, credential verification, or World
//! advancement. [`check_publishable`] evaluates the supplied current state only.
//! It is not an authorization token. Publication must recheck the exact revision,
//! permissions, and policy at its own correctness boundary.

use std::{collections::BTreeMap, fmt};

pub use javelin::{ObjectId, OperationId, VersionNumber};

/// Exact proposal: the native base version, the checkpoint that carries the
/// changeset, the tree root that checkpoint composes to, and an optional opaque
/// context object. The identity of a revision is its tree root (`G§2`). The
/// context is an unvalidated reference; no trace kind, storage, or
/// confidentiality is implied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RevisionRef {
    base: VersionNumber,
    checkpoint: ObjectId,
    tree: ObjectId,
    context: Option<ObjectId>,
}

impl RevisionRef {
    pub fn new(
        base: VersionNumber,
        checkpoint: ObjectId,
        tree: ObjectId,
        context: Option<ObjectId>,
    ) -> Self {
        Self {
            base,
            checkpoint,
            tree,
            context,
        }
    }
    pub fn base(&self) -> VersionNumber {
        self.base
    }
    pub fn checkpoint(&self) -> ObjectId {
        self.checkpoint
    }
    pub fn tree(&self) -> ObjectId {
        self.tree
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
/// `index` on `Approve` and `Publish` pins the revision the actor saw, so a stale
/// view is refused rather than applied to a newer revision.
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
    /// `None` comments on the request; `Some(n)` comments on revision `n`.
    Comment {
        index: Option<u32>,
        body: CommentBody,
    },
    Publish {
        index: u32,
        revision: RevisionRef,
    },
    /// Only the author or a Maintain role holder may close an open request. `index` pins
    /// the revision being closed, so a close prepared for one revision cannot close a later one.
    Close {
        index: u32,
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
    /// `None` for a request-level comment.
    pub index: Option<u32>,
    pub body: CommentBody,
}

/// Lifecycle of one Publish Request. A request is `Open` from its `Open`
/// command; `Published` and `Closed` are terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestState {
    Open,
    Published,
    Closed,
}

/// State of one Publish Request. Every transition returns a new value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Review {
    repository: RepositoryId,
    request: RequestId,
    author: Option<PrincipalId>,
    state: RequestState,
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
            state: RequestState::Open,
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
    pub fn state(&self) -> RequestState {
        self.state
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
        // Replays still require permissions for this repository.
        if permissions.repository != self.repository {
            return Err(Refusal::RepositoryMismatch);
        }
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
        self.check(command, permissions)?;
        Ok(Applied {
            review: self.transition(command, permissions.epoch)?,
            replayed: false,
        })
    }

    /// Re-applies a command that was already accepted, without re-checking roles.
    /// Storage uses this to rebuild state. Current grants must not be applied to
    /// past decisions, so the caller passes the epoch recorded with the command.
    pub fn replay(&self, command: &Command, epoch: u64) -> Result<Review, Refusal> {
        self.transition(command, epoch)
    }

    /// Permission, role, and policy checks that depend on current grants.
    fn check(&self, command: &Command, permissions: &AppliedPermissions) -> Result<(), Refusal> {
        self.check_permissions(permissions)?;
        let actor = &command.actor;
        if permissions.role(actor).is_none() {
            return Err(Refusal::NoAccess);
        }
        match &command.action {
            Action::Approve { .. } if !permissions.has(actor, Role::Review) => {
                Err(Refusal::InsufficientRole {
                    required: Role::Review,
                })
            }
            Action::Close { .. }
                if self.author.as_ref() != Some(actor)
                    && !permissions.has(actor, Role::Maintain) =>
            {
                Err(Refusal::InsufficientRole {
                    required: Role::Maintain,
                })
            }
            Action::Publish { index, revision } => {
                check_publishable(self, *index, revision, permissions, actor).map(|_| ())
            }
            _ => Ok(()),
        }
    }

    /// State transitions. Only facts recorded in the state are consulted, so
    /// this is deterministic for a given command and epoch.
    fn transition(&self, command: &Command, epoch: u64) -> Result<Review, Refusal> {
        let actor = &command.actor;
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
                let current = self.open_revision()?;
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
                let current = self.open_revision()?;
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
                self.open_revision()?;
                if let Some(index) = index
                    && (*index == 0 || *index as usize > self.revisions.len())
                {
                    return Err(Refusal::UnknownRevision);
                }
                next.comments.push(Comment {
                    author: actor.clone(),
                    index: *index,
                    body: body.clone(),
                });
            }
            Action::Publish { .. } => {
                // Policy was checked in `check`; the transition only records it.
                self.open_revision()?;
                next.state = RequestState::Published;
            }
            Action::Close { index } => {
                let current = self.open_revision()?;
                if *index != current.index {
                    return Err(Refusal::StaleRevision {
                        current: current.index,
                    });
                }
                next.state = RequestState::Closed;
            }
        }
        next.epoch = epoch;
        next.history.push(command.clone());
        Ok(next)
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

    /// The current revision, if the request is Open and has been opened.
    fn open_revision(&self) -> Result<&Revision, Refusal> {
        match (self.state, self.revisions.last()) {
            (RequestState::Open, Some(current)) => Ok(current),
            _ => Err(Refusal::NotOpen),
        }
    }

    /// The same request, Published. Storage uses this when the World already holds the
    /// current revision's publication, whose record was lost before it was written.
    pub fn published_by_world(&self) -> Self {
        let mut next = self.clone();
        next.state = RequestState::Published;
        next
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

/// Evaluates whether `publisher` may Publish revision `index`, which must be
/// `exact`, under `permissions` now. Counts approvals of that revision by
/// principals who currently hold `Review` or `Maintain`, excluding the author.
/// Does not compare `exact.base()` with the current World; publication handles
/// that at its correctness boundary.
pub fn check_publishable(
    review: &Review,
    index: u32,
    exact: &RevisionRef,
    permissions: &AppliedPermissions,
    publisher: &PrincipalId,
) -> Result<Publishable, Refusal> {
    review.check_permissions(permissions)?;
    // Refuse unauthorized publishers before revealing request or revision state.
    if permissions.role(publisher).is_none() {
        return Err(Refusal::NoAccess);
    }
    if !permissions.has(publisher, Role::Maintain) {
        return Err(Refusal::InsufficientRole {
            required: Role::Maintain,
        });
    }
    let current = review.open_revision()?;
    if index != current.index {
        return Err(Refusal::StaleRevision {
            current: current.index,
        });
    }
    if current.revision != *exact {
        return Err(Refusal::RevisionMismatch);
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
