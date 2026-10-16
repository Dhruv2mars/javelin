//! Real caller: a native repository proposal reviewed, revised, and checked.
//! Usage: review_flow <empty-directory>

use std::collections::BTreeMap;

use javelin::{
    FileEntry, LayerId, LayerState, LayerUpdate, Manifest, RepoPath, Repository, SparseChanges,
};
use javelin_collaboration::{
    Action, AppliedPermissions, Command, OperationId, PrincipalId, RepositoryId, RequestId, Review,
    RevisionRef, Role, VersionNumber, Visibility, check_publishable,
};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn proposal(repo: &mut Repository, n: u32, text: &str) -> Res<RevisionRef> {
    let base = repo.layer_base(VersionNumber::new(1)?)?;
    let head = repo.create_layer(&LayerId::parse(&format!("layer-{n}"))?, &base)?;
    let entry = FileEntry::Regular {
        content: repo.put_blob(text.as_bytes())?,
        executable: false,
    };
    let changes = SparseChanges::new([(RepoPath::parse("README")?, Some(entry))])?;
    let LayerUpdate::Applied {
        state: LayerState::Active { head },
    } = repo.capture_layer(
        &OperationId::parse(&format!("capture-{n}"))?,
        &head,
        &changes,
    )?
    else {
        return Err("capture was not applied".into());
    };
    let checkpoint = repo.read_checkpoint(head.checkpoint())?;
    let manifest = repo.put_manifest(checkpoint.manifest())?;
    let context = repo.put_blob(format!("supplied context {n}").as_bytes())?;
    Ok(RevisionRef::new(
        checkpoint.base().version(),
        manifest,
        Some(context),
    ))
}

fn main() -> Res<()> {
    let dir = std::env::args()
        .nth(1)
        .ok_or("usage: review_flow <empty-directory>")?;
    let mut repo = Repository::init(&dir)?;
    let empty = repo.put_manifest(&Manifest::new([])?)?;
    repo.compare_and_advance(
        &OperationId::parse("world-1")?,
        VersionNumber::new(0)?,
        empty,
    )?;
    let (v1, v2) = (
        proposal(&mut repo, 1, "first\n")?,
        proposal(&mut repo, 2, "second\n")?,
    );

    let p = |s: &str| PrincipalId::parse(s);
    let perms = AppliedPermissions {
        repository: RepositoryId::parse("example")?,
        epoch: 1,
        visibility: Visibility::Public,
        roles: BTreeMap::from([(p("rita")?, Role::Review), (p("mona")?, Role::Maintain)]),
        required_approvals: 1,
    };
    let command = |op: &str, actor: &str, action| -> Res<Command> {
        Ok(Command {
            operation: OperationId::parse(op)?,
            actor: p(actor)?,
            action,
        })
    };
    let steps = [
        command("open", "outsider", Action::Open { revision: v1 })?,
        command(
            "approve-1",
            "rita",
            Action::Approve {
                index: 1,
                revision: v1,
            },
        )?,
        command(
            "takeover",
            "mona",
            Action::Revise {
                expected: 1,
                revision: v2,
            },
        )?,
        command(
            "revise-2",
            "outsider",
            Action::Revise {
                expected: 1,
                revision: v2,
            },
        )?,
        command(
            "approve-2",
            "rita",
            Action::Approve {
                index: 2,
                revision: v2,
            },
        )?,
        command(
            "approve-2",
            "rita",
            Action::Approve {
                index: 2,
                revision: v2,
            },
        )?,
    ];
    let mut review = Review::new(perms.repository.clone(), RequestId::parse("pr-1")?);
    for step in &steps {
        match review.apply(step, &perms) {
            Ok(applied) => {
                println!("{} accepted replayed={}", step.operation, applied.replayed);
                review = applied.review;
            }
            Err(refusal) => println!("{} refused {}", step.operation, refusal.code()),
        }
    }
    for exact in [v1, v2] {
        let result = check_publishable(&review, &exact, &perms, &p("mona")?);
        println!(
            "publishable revision manifest={} -> {:?}",
            exact.manifest(),
            result.map(|ok| ok.index).map_err(|r| r.code())
        );
    }
    Ok(())
}
