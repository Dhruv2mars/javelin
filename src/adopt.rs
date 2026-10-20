//! One-command adoption of an existing project directory that is not a Javelin repository.
//!
//! Adoption creates the repository and publishes the directory's files as the first World
//! after World 0, which stays empty as `init` defines it. The files enter through the
//! reserved Layer [`ADOPTION_LAYER`]: capture reads the directory once, honoring root
//! `.javelinignore`, and Publish accepts that checkpoint whole. The directory itself is
//! never written. Later edits reach a World only through a Layer, never by re-reading the
//! directory. Git metadata (`.git`) is never read or imported.
//!
//! Retries: each step replays or resumes under the caller's operation ID. A crash before
//! Publish leaves the reserved Layer Active, and the same operation completes it. A completed
//! adoption replays its receipt. Any other existing repository is refused without change.

use std::{fs, path::Path};

use crate::{
    Error, LayerId, LayerState, OperationId, PublicationReceipt, PublishAttribution,
    PublishOutcome, Repository, Result,
};

/// The reserved Layer that carries an adopted project into its World.
pub const ADOPTION_LAYER: &str = "adoption";

/// The result of [`Repository::adopt`].
#[derive(Debug, serde::Serialize)]
pub struct Adoption {
    pub receipt: PublicationReceipt,
    /// Files in the adopted World. `None` when Publish did not accept the adoption.
    pub files: Option<usize>,
}

impl Repository {
    /// Creates a repository for the project directory `root` and publishes its files, or
    /// completes an interrupted adoption of it under the same `operation`.
    ///
    /// Refuses any other existing repository, including one made by `init`, with
    /// `invalid_input` and no change. Files that keep changing during capture return
    /// `busy`, because the adopted World must match the directory at one moment.
    pub fn adopt(root: impl AsRef<Path>, operation: &OperationId) -> Result<Adoption> {
        let root = fs::canonicalize(root)?;
        if !root.is_dir() {
            return Err(Error::InvalidInput(
                "project root must be a directory".into(),
            ));
        }
        let layer = LayerId::parse(ADOPTION_LAYER)?;
        let existing = || {
            Error::InvalidInput(
                "repository already exists; adopt needs a project directory without one".into(),
            )
        };
        // A store that opens is either this adoption (layer present) or a repository to refuse.
        // A fresh store gets the adoption Layer in the same transaction as World 0.
        let mut repo = match Self::open(&root) {
            Ok(repo) => repo,
            Err(Error::NotInitialized(_)) => Self::init_adopting(&root, &layer)?,
            Err(error) => return Err(error),
        };
        if repo.layer_outside(&layer)? || !repo.layer_exists(&layer)? {
            return Err(existing());
        }
        match repo.layer_state(&layer)? {
            LayerState::Active { .. } => {}
            LayerState::Published {
                operation: done, ..
            } if &done == operation => return repo.adopted(operation),
            LayerState::Published {
                operation: done, ..
            } => {
                return Err(Error::InvalidInput(format!(
                    "repository was already adopted by operation {done}"
                )));
            }
            LayerState::Discarded { .. } => return Err(existing()),
        }
        let capture = repo.capture_project(&layer, &root)?;
        if !capture.uncertain.is_empty() {
            let paths: Vec<_> = capture.uncertain.iter().map(|p| p.as_str()).collect();
            return Err(Error::Busy(format!(
                "project files kept changing during adoption: {}",
                paths.join(", ")
            )));
        }
        #[cfg(test)]
        crate::repository::test_boundary("adopt_after_capture");
        let expected = match repo.layer_state(&layer)? {
            LayerState::Active { head } => head,
            _ => return Err(existing()),
        };
        let receipt =
            repo.publish_layer(operation, &expected, &PublishAttribution::Human, |_, _| {
                Ok(())
            })?;
        repo.adoption_result(receipt)
    }

    fn adopted(&self, operation: &OperationId) -> Result<Adoption> {
        let receipt = self.publication(operation)?.ok_or_else(|| {
            Error::CorruptRepository("adopted Layer has no publication receipt".into())
        })?;
        self.adoption_result(receipt)
    }

    fn adoption_result(&self, receipt: PublicationReceipt) -> Result<Adoption> {
        let files = match &receipt.outcome {
            PublishOutcome::Published { version } => {
                Some(self.manifest_at(version.number)?.entries().len())
            }
            PublishOutcome::Conflict { .. } | PublishOutcome::LayerStale { .. } => None,
        };
        Ok(Adoption { receipt, files })
    }

    fn layer_exists(&self, layer: &LayerId) -> Result<bool> {
        Ok(self
            .conn
            .prepare("SELECT 1 FROM layer_heads WHERE layer=?1")?
            .exists([layer.as_str()])?)
    }

    /// Any Layer other than the reserved adoption Layer makes the repository non-fresh.
    fn layer_outside(&self, layer: &LayerId) -> Result<bool> {
        Ok(self
            .conn
            .prepare("SELECT 1 FROM layer_heads WHERE layer<>?1")?
            .exists([layer.as_str()])?)
    }
}
