//! Real caller: two Layers over one World merge into a stored manifest, or report conflicts.
//! Usage: stack_preview <empty-existing-directory>

use std::{env, process::ExitCode};

use javelin::{
    FileEntry, LayerId, LayerState, LayerUpdate, Manifest, OperationId, RepoPath, Repository,
    SparseChanges, VersionNumber,
};
use javelin_merge::{Merged, merge};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    let Some(root) = env::args().nth(1) else {
        eprintln!("usage: stack_preview <empty-existing-directory>");
        return ExitCode::from(2);
    };
    match run(&root) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(root: &str) -> Result<()> {
    let mut repo = Repository::init(root)?;
    let base = file(
        &mut repo,
        b"fn main() {\n    one();\n    two();\n    three();\n}\n",
    )?;
    let world = Manifest::new([(RepoPath::parse("src/main.rs")?, base)])?;
    let world = repo.put_manifest(&world)?;
    repo.compare_and_advance(
        &OperationId::parse("world-1")?,
        VersionNumber::new(0)?,
        world,
    )?;
    let base = repo.read_manifest(world)?;

    let ours = edit(
        &mut repo,
        "ours",
        b"fn main() {\n    ONE();\n    two();\n    three();\n}\n",
    )?;
    let theirs = edit(
        &mut repo,
        "theirs",
        b"fn main() {\n    one();\n    two();\n    THREE();\n}\n",
    )?;
    let clash = edit(
        &mut repo,
        "clash",
        b"fn main() {\n    uno();\n    two();\n    three();\n}\n",
    )?;

    // Clean: store new bytes and the merged manifest through the public repository API.
    let outcome = merge(&base, &ours, &theirs, |id| repo.read_blob(id))?;
    if !outcome.is_clean() {
        return Err(format!("expected a clean merge: {:?}", outcome.conflicts).into());
    }
    let mut entries = Vec::new();
    for (path, merged) in outcome.merged {
        let entry = match merged {
            Merged::Retained(entry) => entry,
            Merged::Content { bytes, executable } => FileEntry::Regular {
                content: repo.put_blob(&bytes)?,
                executable,
            },
        };
        entries.push((path, entry));
    }
    let id = repo.put_manifest(&Manifest::new(entries)?)?;
    let merged = repo.read_manifest(id)?;
    let content = merged.entries()[&RepoPath::parse("src/main.rs")?].content();
    let text = String::from_utf8(repo.read_blob(content)?)?;
    if text != "fn main() {\n    ONE();\n    two();\n    THREE();\n}\n" {
        return Err(format!("unexpected merged text {text:?}").into());
    }
    println!("clean merged_manifest={id}");

    // Conflict: nothing is stored; the owner receives exact entries to choose between.
    let outcome = merge(&base, &ours, &clash, |id| repo.read_blob(id))?;
    for conflict in &outcome.conflicts {
        println!(
            "conflict path={} kind={:?}",
            conflict.path.as_str(),
            conflict.kind
        );
    }
    if outcome.conflicts.len() != 1 || !outcome.merged.is_empty() {
        return Err("expected exactly one unresolved path".into());
    }
    Ok(())
}

fn file(repo: &mut Repository, bytes: &[u8]) -> Result<FileEntry> {
    Ok(FileEntry::Regular {
        content: repo.put_blob(bytes)?,
        executable: false,
    })
}

/// Captures one edit in a new Layer over World 1 and returns its checkpoint manifest.
fn edit(repo: &mut Repository, name: &str, bytes: &[u8]) -> Result<Manifest> {
    let base = repo.layer_base(VersionNumber::new(1)?)?;
    let head = repo.create_layer(&LayerId::parse(name)?, &base)?;
    let changes =
        SparseChanges::new([(RepoPath::parse("src/main.rs")?, Some(file(repo, bytes)?))])?;
    let update = repo.capture_layer(&OperationId::parse(name)?, &head, &changes)?;
    let LayerUpdate::Applied {
        state: LayerState::Active { head },
    } = update
    else {
        return Err(format!("capture not applied: {update:?}").into());
    };
    Ok(repo.read_checkpoint(head.checkpoint())?.manifest().clone())
}
