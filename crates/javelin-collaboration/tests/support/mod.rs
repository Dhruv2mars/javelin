#![allow(dead_code)]

use std::collections::BTreeMap;

use javelin::{
    FileEntry, LayerId, LayerState, LayerUpdate, Manifest, RepoPath, Repository, SparseChanges,
};
use javelin_collaboration::{
    Action, AppliedPermissions, Command, CommentBody, OperationId, PrincipalId, RepositoryId,
    RequestId, Review, RevisionRef, Role, VersionNumber, Visibility,
};

/// A real native repository with World 1 and Layers that produce proposals.
pub struct Fixture {
    _dir: tempfile::TempDir,
    pub repo: Repository,
    next: u32,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut repo = Repository::init(dir.path()).unwrap();
        let blob = repo.put_blob(b"fn main() {}\n").unwrap();
        let manifest = Manifest::new([(
            RepoPath::parse("src/main.rs").unwrap(),
            FileEntry::Regular {
                content: blob,
                executable: false,
            },
        )])
        .unwrap();
        let id = repo.put_manifest(&manifest).unwrap();
        repo.compare_and_advance(&op("world-1"), VersionNumber::new(0).unwrap(), id)
            .unwrap();
        // World 2 has the same tree as World 1, so equal edits differ only by base.
        repo.compare_and_advance(&op("world-2"), VersionNumber::new(1).unwrap(), id)
            .unwrap();
        Self {
            _dir: dir,
            repo,
            next: 0,
        }
    }

    /// Captures `content` in a fresh Layer over `base` and returns its exact
    /// native proposal reference, optionally with a supplied context blob.
    pub fn proposal(&mut self, base: u64, content: &str, context: Option<&[u8]>) -> RevisionRef {
        self.next += 1;
        let base = self
            .repo
            .layer_base(VersionNumber::new(base).unwrap())
            .unwrap();
        let layer = LayerId::parse(&format!("layer-{}", self.next)).unwrap();
        let head = self.repo.create_layer(&layer, &base).unwrap();
        let blob = self.repo.put_blob(content.as_bytes()).unwrap();
        let changes = SparseChanges::new([(
            RepoPath::parse("src/lib.rs").unwrap(),
            Some(FileEntry::Regular {
                content: blob,
                executable: false,
            }),
        )])
        .unwrap();
        let head = match self
            .repo
            .capture_layer(&op(&format!("capture-{}", self.next)), &head, &changes)
            .unwrap()
        {
            LayerUpdate::Applied {
                state: LayerState::Active { head },
            } => head,
            other => panic!("capture failed: {other:?}"),
        };
        let checkpoint = self.repo.read_checkpoint(head.checkpoint()).unwrap();
        // The tree root is what the checkpoint composes to; storing it is idempotent.
        let manifest = self.repo.checkpoint_manifest(checkpoint.id()).unwrap();
        let tree = self.repo.put_snapshot(&manifest).unwrap();
        let context = context.map(|bytes| self.repo.put_blob(bytes).unwrap());
        RevisionRef::new(
            checkpoint.base().version(),
            checkpoint.id().object(),
            tree,
            context,
        )
    }
}

pub fn op(value: &str) -> OperationId {
    OperationId::parse(value).unwrap()
}

pub fn who(value: &str) -> PrincipalId {
    PrincipalId::parse(value).unwrap()
}

pub fn repo_id() -> RepositoryId {
    RepositoryId::parse("repo-1").unwrap()
}

pub fn review() -> Review {
    Review::new(repo_id(), RequestId::parse("pr-1").unwrap())
}

pub fn perms(
    epoch: u64,
    visibility: Visibility,
    roles: &[(&str, Role)],
    required: u32,
) -> AppliedPermissions {
    AppliedPermissions {
        repository: repo_id(),
        epoch,
        visibility,
        roles: roles
            .iter()
            .map(|(p, r)| (who(p), *r))
            .collect::<BTreeMap<_, _>>(),
        required_approvals: required,
    }
}

/// Private repository: alice authors, rita reviews, mona maintains.
pub fn team(epoch: u64) -> AppliedPermissions {
    perms(
        epoch,
        Visibility::Private,
        &[
            ("alice", Role::Read),
            ("rita", Role::Review),
            ("mona", Role::Maintain),
        ],
        1,
    )
}

pub fn cmd(operation: &str, actor: &str, action: Action) -> Command {
    Command {
        operation: op(operation),
        actor: who(actor),
        action,
    }
}

pub fn open(revision: RevisionRef) -> Action {
    Action::Open { revision }
}

pub fn revise(expected: u32, revision: RevisionRef) -> Action {
    Action::Revise { expected, revision }
}

pub fn approve(index: u32, revision: RevisionRef) -> Action {
    Action::Approve { index, revision }
}

pub fn publish(index: u32, revision: RevisionRef) -> Action {
    Action::Publish { index, revision }
}

/// Comment on revision `index`.
pub fn comment(index: u32, body: &str) -> Action {
    Action::Comment {
        index: Some(index),
        body: CommentBody::parse(body).unwrap(),
    }
}

/// Comment on the request itself.
pub fn request_comment(body: &str) -> Action {
    Action::Comment {
        index: None,
        body: CommentBody::parse(body).unwrap(),
    }
}
