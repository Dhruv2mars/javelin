//! Foreground content sampling. Only frozen, revalidated observations may capture.
use super::*;
use std::time::{Duration, Instant};

const SAMPLE: Duration = Duration::from_millis(250);

#[derive(Default)]
struct Quiet {
    observed: Option<(Snapshot, Instant)>,
}

impl Quiet {
    fn ready(&mut self, snapshot: Snapshot, now: Instant, quiet: Duration) -> bool {
        match &self.observed {
            Some((previous, since)) if previous == &snapshot => now.duration_since(*since) >= quiet,
            _ => {
                self.observed = Some((snapshot, now));
                false
            }
        }
    }
}

impl LinuxLayerRuntime {
    /// Opt-in foreground watch, sampling exact file bytes every 250ms after each
    /// scan. The quiet window must be at least 250ms. Returns the number of
    /// completed captures on duration expiry; terminal or stale bindings return
    /// errors. No locks are held between samples. Duration bounds scheduling,
    /// not the execution time of an individual scan or capture.
    pub fn watch(
        &mut self,
        repo: &mut Repository,
        layer: &LayerId,
        quiet: Duration,
        duration: Option<Duration>,
    ) -> Result<u64> {
        ensure_linux()?;
        if quiet < SAMPLE || duration.is_some_and(|d| d.is_zero()) {
            return Err(Error::InvalidInput(
                "quiet window must be >=250ms and duration must be positive".into(),
            ));
        }
        let start = Instant::now();
        let mut stable = Quiet::default();
        let mut captures = 0;
        loop {
            if duration.is_some_and(|d| start.elapsed() >= d) {
                return Ok(captures);
            }
            match self.watch_sample(repo, layer, &mut stable, quiet) {
                Ok(captured) => captures += u64::from(captured),
                Err(error @ Error::Busy(_)) => {
                    // A terminal view whose freeze is blocked must fail visibly,
                    // not turn duration expiry into a misleading successful watch.
                    if !matches!(repo.layer_state(layer)?, LayerState::Active { .. }) {
                        return Err(error);
                    }
                    stable = Quiet::default();
                }
                Err(error) => return Err(error),
            }
            #[cfg(test)]
            crate::repository::test_boundary("watch_sleep");
            let sleep = duration.map_or(SAMPLE, |d| SAMPLE.min(d.saturating_sub(start.elapsed())));
            std::thread::sleep(sleep);
        }
    }

    fn watch_sample(
        &mut self,
        repo: &mut Repository,
        layer: &LayerId,
        stable: &mut Quiet,
        quiet: Duration,
    ) -> Result<bool> {
        let _lock = self.lock()?;
        let slot = slot(layer);
        match repo.layer_state(layer)? {
            LayerState::Published { operation, .. } => {
                self.freeze_view(&slot)?;
                return Err(Error::LayerPublished {
                    layer: layer.clone(),
                    operation,
                });
            }
            LayerState::Discarded { .. } => {
                return Err(Error::InvalidInput(
                    "watch stopped: Layer is discarded".into(),
                ));
            }
            LayerState::Active { .. } => {}
        }
        let mut record = self.read_record(&slot, layer)?.ok_or_else(|| {
            Error::InvalidInput("Layer has no view; mount explicitly before watch".into())
        })?;
        if record.rebinding() {
            return Err(Error::InvalidInput(
                "watch stopped: recover interrupted rebind explicitly".into(),
            ));
        }
        if let Some(pending) = &record.pending {
            if pending.action != Action::Capture {
                return Err(Error::InvalidInput(
                    "watch stopped: recover pending Publish/discard explicitly".into(),
                ));
            }
            let operation = OperationId::parse(&pending.operation)?;
            self.capture_frozen(repo, layer, &slot, &mut record, &operation)?;
            *stable = Quiet::default();
            return Ok(true);
        }
        let bound = self.head(repo, layer, &record)?;
        if repo.layer_state(layer)?
            != (LayerState::Active {
                head: bound.clone(),
            })
        {
            return Err(Error::InvalidInput(
                "watch stopped: native head advanced externally; explicitly rebind the workspace"
                    .into(),
            ));
        }
        let view = self.root.join("views").join(&slot);
        if mount_state(&view)?.is_none() {
            return Err(Error::InvalidInput(
                "watch stopped: Layer view is not mounted".into(),
            ));
        }
        // A crash before a pending write can leave a frozen view. Existing
        // resume behavior preserves terminal states and thaws this active view.
        self.resume_view(repo, layer, &slot, &record, false)?;
        let checkpoint = repo.read_checkpoint(bound.checkpoint())?;
        let observed = match read_snapshot(&view, None, checkpoint.manifest(), &mut |bytes| {
            Ok(ObjectId::hash(ObjectKind::Blob, bytes))
        }) {
            // Atomic saves/deletions during a live scan are expected races.
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::Busy("path disappeared during observation".into()));
            }
            result => result?,
        };
        if observed.entries == *checkpoint.manifest().entries() {
            *stable = Quiet::default();
            return Ok(false);
        }
        if !stable.ready(observed, Instant::now(), quiet) {
            return Ok(false);
        }
        #[cfg(test)]
        crate::repository::test_boundary("watch_quiet");
        self.helper("freeze", &["view", &slot])?;
        let validation = (|| {
            let (epoch, upper) = parse_upper(&self.helper("list", &["view", &slot])?)?;
            let opened = repo.read_checkpoint(CheckpointId::parse(&record.opened)?)?;
            let frozen = snapshot(&view, &upper, opened.manifest(), &mut |b| {
                Ok(ObjectId::hash(ObjectKind::Blob, b))
            })?;
            if self.epoch()? != epoch || stable.observed.as_ref().map(|(s, _)| s) != Some(&frozen) {
                return Err(Error::Busy(
                    "frozen candidate differs from quiet observation".into(),
                ));
            }
            Ok(())
        })();
        if let Err(error) = validation {
            self.resume_view(repo, layer, &slot, &record, false)?;
            return Err(error);
        }
        // Random operation identity is persisted by the existing pending record
        // before attempting native capture. Restart always resolves it first.
        let mut nonce = [0; 32];
        fs::File::open("/dev/urandom")?.read_exact(&mut nonce)?;
        let operation = OperationId::parse(&format!(
            "watch-{}",
            ObjectId::hash(ObjectKind::Blob, &nonce)
        ))?;
        let base = repo.read_manifest(checkpoint.base().manifest())?;
        let changes = complete_changes(
            &base,
            &stable
                .observed
                .as_ref()
                .expect("quiet observation")
                .0
                .entries,
        )?;
        record.pending = Some(Pending {
            operation: operation.as_str().into(),
            action: Action::Capture,
            candidate: Some(Candidate {
                expected: bound.revision(),
                changes: changes_digest(&changes),
            }),
            publication: None,
        });
        self.write_record(&slot, &record)?;
        #[cfg(test)]
        crate::repository::test_boundary("watch_pending");
        let outcome = self.capture_frozen(repo, layer, &slot, &mut record, &operation)?;
        *stable = Quiet::default();
        if !matches!(outcome.update, LayerUpdate::Applied { .. }) {
            return Err(Error::InvalidInput(
                "watch stopped: native binding changed; explicitly rebind".into(),
            ));
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_window_restarts_on_exact_content_or_mode_change() {
        let sample = |executable| Snapshot {
            entries: BTreeMap::from([(
                RepoPath::parse("a").unwrap(),
                FileEntry::Regular {
                    content: ObjectId::hash(ObjectKind::Blob, b"same"),
                    executable,
                },
            )]),
            unrepresented: vec![],
        };
        let now = Instant::now();
        let quiet = Duration::from_secs(1);
        let mut state = Quiet::default();
        assert!(!state.ready(sample(false), now, quiet));
        assert!(!state.ready(sample(false), now + quiet / 2, quiet));
        assert!(!state.ready(sample(true), now + quiet, quiet));
        assert!(!state.ready(sample(true), now + quiet + quiet / 2, quiet));
        assert!(state.ready(sample(true), now + quiet * 2, quiet));
    }
}
