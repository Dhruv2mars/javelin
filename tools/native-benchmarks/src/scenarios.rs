//! Single-process scenarios over the public Repository and Layer APIs.
use std::collections::BTreeMap;

use javelin::{
    AdvanceOutcome, FileEntry, LayerId, LayerState, LayerUpdate, Manifest, ObjectId, OperationId,
    RepoPath, Repository, SparseChanges, VersionNumber,
};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    ctx::{Ctx, R},
    fixture,
    host::hex,
    record::{Sample, timed},
};

/// Layer change payloads are truncated so cumulative captures stay small.
const LAYER_BLOB_MAX: usize = 16 * 1024;

fn regular(content: ObjectId) -> FileEntry {
    FileEntry::Regular {
        content,
        executable: false,
    }
}

fn advance_sample(
    ctx: &Ctx,
    op: &str,
    index: u64,
    ns: u64,
    operation: &str,
    expected: u64,
    result: &javelin::Result<AdvanceOutcome>,
) -> Sample {
    let mut s = ctx.sample(op, index, ns, result);
    s.operation = Some(operation.into());
    s.expected = Some(expected);
    match result {
        Ok(AdvanceOutcome::Accepted { version }) => {
            s.outcome = "accepted".into();
            s.result = Some(version.number.get());
            s.manifest = Some(version.manifest.to_string());
        }
        Ok(AdvanceOutcome::Stale { actual }) => {
            s.outcome = "stale".into();
            s.result = Some(actual.get());
        }
        Err(_) => {}
    }
    s
}

pub fn ingest(ctx: &mut Ctx) -> R<()> {
    let fx = ctx.fixture();
    let dir = ctx.repo_dir()?;
    let mut repo = Repository::init(&dir)?;
    ctx.disk("empty_repository", Some(0), None);
    let mut unique: BTreeMap<String, u64> = BTreeMap::new();
    let mut classes: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    let mut content_digest = Sha256::new();
    let mut api_digest = Sha256::new();
    let mut sizes = Vec::new();
    let mut done = 0;
    for i in 0..ctx.params.files {
        if ctx.expired() {
            ctx.note("budget exhausted during ingest put_blob; run is truncated");
            break;
        }
        let content = fx.content(i, 0);
        let (ns, result) = timed(|| repo.put_blob(&content));
        let mut s = ctx.sample("put_blob", i as u64, ns, &result);
        s.bytes = Some(content.len() as u64);
        s.content = Some(format!("file:{i}"));
        s.object = result.as_ref().ok().map(|id| id.to_string());
        ctx.push(s);
        let id = result?;
        content_digest.update(fx.path(i).as_bytes());
        content_digest.update((content.len() as u64).to_le_bytes());
        content_digest.update(&content);
        api_digest.update(format!("{}\t{id}\n", fx.path(i)));
        unique.entry(id.to_string()).or_insert(content.len() as u64);
        let class = classes.entry(fx.class(i).name()).or_default();
        class.0 += 1;
        class.1 += content.len() as u64;
        sizes.push(content.len() as u64);
        done = i + 1;
    }
    let payload: u64 = unique.values().sum();
    let (ns, check) = timed(|| repo.check());
    let objects = check.as_ref().ok().map(|c| c.objects as u64);
    ctx.push(ctx.sample("check", 0, ns, &check));
    ctx.disk("after_put_blob", Some(payload), objects);
    // Identical re-put: shared objects must not add payload; cost is measured, not assumed.
    for i in 0..done {
        if ctx.expired() {
            ctx.note("budget exhausted during duplicate put_blob; run is truncated");
            break;
        }
        let content = fx.content(i, 0);
        let (ns, result) = timed(|| repo.put_blob(&content));
        let mut s = ctx.sample("put_blob_duplicate", i as u64, ns, &result);
        s.bytes = Some(content.len() as u64);
        s.content = Some(format!("file:{i}"));
        s.object = result.as_ref().ok().map(|id| id.to_string());
        ctx.push(s);
        result?;
    }
    ctx.disk("after_duplicate_put_blob", Some(payload), objects);
    sizes.sort_unstable();
    let inventory = json!({
        "seed": ctx.params.seed,
        "files": done,
        "class_counts_and_bytes": classes.iter().map(|(k, v)| (k.to_string(), json!({"files": v.0, "bytes": v.1}))).collect::<BTreeMap<_, _>>(),
        "total_content_bytes": sizes.iter().sum::<u64>(),
        "unique_objects": unique.len(),
        "unique_payload_bytes": payload,
        "size_min": sizes.first(), "size_median": sizes.get(sizes.len() / 2), "size_max": sizes.last(),
        "content_digest_sha256": hex(&content_digest.finalize()),
        "api_object_digest_sha256": hex(&api_digest.finalize()),
        "digest_notes": "content digest hashes generator output; api digest hashes path and the ObjectId returned by put_blob",
    });
    std::fs::write(
        ctx.out.join("inventory-ingest.json"),
        serde_json::to_vec_pretty(&inventory)?,
    )?;
    Ok(())
}

pub fn history(ctx: &mut Ctx) -> R<()> {
    let fx = ctx.fixture();
    let (files, versions, k) = (
        ctx.params.files,
        ctx.params.history,
        ctx.params.changed_per_version,
    );
    let dir = ctx.repo_dir()?;
    let mut repo = Repository::init(&dir)?;
    let mut ids = Vec::with_capacity(files);
    for i in 0..files {
        let content = fx.content(i, 0);
        let (ns, result) = timed(|| repo.put_blob(&content));
        let mut s = ctx.sample("put_blob_base", i as u64, ns, &result);
        s.bytes = Some(content.len() as u64);
        ctx.push(s);
        ids.push(result?);
    }
    let paths: Vec<RepoPath> = (0..files)
        .map(|i| RepoPath::parse(&fx.path(i)))
        .collect::<Result<_, _>>()?;
    let mut marks = vec![1, versions / 4, versions / 2, versions];
    marks.retain(|m| *m >= 1);
    marks.sort_unstable();
    marks.dedup();
    for h in 1..=versions {
        if ctx.expired() {
            ctx.note("budget exhausted during history build; run is truncated");
            break;
        }
        let changed: Vec<usize> = if h == 1 {
            vec![]
        } else {
            (0..k).map(|j| ((h - 1) * k + j) % files).collect()
        };
        for &i in &changed {
            let content = fx.content(i, h as u64);
            let (ns, result) = timed(|| repo.put_blob(&content));
            let mut s = ctx.sample("put_blob_changed", i as u64, ns, &result);
            s.bytes = Some(content.len() as u64);
            s.param = Some(h as u64);
            ctx.push(s);
            ids[i] = result?;
        }
        let (ns, manifest) =
            timed(|| Manifest::new(paths.iter().cloned().zip(ids.iter().map(|id| regular(*id)))));
        let mut s = ctx.sample("manifest_build", h as u64, ns, &manifest);
        s.param = Some(h as u64);
        ctx.push(s);
        let manifest = manifest?;
        let (ns, stored) = timed(|| repo.put_manifest(&manifest));
        let mut s = ctx.sample("put_manifest", h as u64, ns, &stored);
        s.param = Some(h as u64);
        s.object = stored.as_ref().ok().map(|id| id.to_string());
        ctx.push(s);
        let manifest_id = stored?;
        let operation = format!("history-{h}");
        let expected = VersionNumber::new(h as u64 - 1)?;
        let (ns, outcome) = timed(|| {
            repo.compare_and_advance(&OperationId::parse(&operation)?, expected, manifest_id)
        });
        let mut s = advance_sample(
            ctx,
            "compare_and_advance",
            h as u64,
            ns,
            &operation,
            h as u64 - 1,
            &outcome,
        );
        s.param = Some(h as u64);
        ctx.push(s);
        match outcome? {
            AdvanceOutcome::Accepted { version } if version.number.get() == h as u64 => {}
            other => {
                return Err(format!("history advance {h} not accepted at {h}: {other:?}").into());
            }
        }
        if marks.contains(&h) {
            for rep in 0..3 {
                let (ns, opened) = timed(|| Repository::open(&dir));
                let mut s = ctx.sample("open", rep, ns, &opened);
                s.param = Some(h as u64);
                ctx.push(s);
                let reader = opened?;
                let (ns, hist) = timed(|| reader.history());
                let mut s = ctx.sample("history", rep, ns, &hist);
                s.param = Some(h as u64);
                s.result = hist.as_ref().ok().map(|v| v.len() as u64);
                ctx.push(s);
                let (ns, version) = timed(|| reader.version(VersionNumber::new(h as u64)?));
                let mut s = ctx.sample("version", rep, ns, &version);
                s.param = Some(h as u64);
                ctx.push(s);
                let (ns, read) = timed(|| reader.read_manifest(manifest_id));
                let mut s = ctx.sample("read_manifest", rep, ns, &read);
                s.param = Some(h as u64);
                s.bytes = read.as_ref().ok().map(|m| m.canonical_bytes().len() as u64);
                ctx.push(s);
                let (ns, check) = timed(|| reader.check());
                let mut s = ctx.sample("check", rep, ns, &check);
                s.param = Some(h as u64);
                s.result = check.as_ref().ok().map(|c| c.objects as u64);
                ctx.push(s);
                hist?;
                check?;
            }
            ctx.disk(&format!("history_{h}"), None, None);
        }
    }
    Ok(())
}

pub fn layers(ctx: &mut Ctx) -> R<()> {
    let fx = ctx.fixture();
    let files = ctx.params.layer_files;
    let dir = ctx.repo_dir()?;
    let mut repo = Repository::init(&dir)?;
    let mut ids = Vec::with_capacity(files);
    for i in 0..files {
        let content = fx.content(i, 0);
        let (ns, result) = timed(|| repo.put_blob(&content));
        let mut s = ctx.sample("put_blob_base", i as u64, ns, &result);
        s.bytes = Some(content.len() as u64);
        ctx.push(s);
        ids.push(result?);
    }
    let paths: Vec<RepoPath> = (0..files)
        .map(|i| RepoPath::parse(&fx.path(i)))
        .collect::<Result<_, _>>()?;
    let manifest = Manifest::new(paths.iter().cloned().zip(ids.iter().map(|id| regular(*id))))?;
    let manifest_id = repo.put_manifest(&manifest)?;
    let (ns, base_world) = timed(|| {
        repo.compare_and_advance(
            &OperationId::parse("layers-base")?,
            VersionNumber::new(0)?,
            manifest_id,
        )
    });
    let s = advance_sample(
        ctx,
        "compare_and_advance",
        1,
        ns,
        "layers-base",
        0,
        &base_world,
    );
    ctx.push(s);
    base_world?;
    let (ns, base) = timed(|| repo.layer_base(VersionNumber::new(1)?));
    ctx.push(ctx.sample("layer_base", 0, ns, &base));
    let base = base?;
    let layer = LayerId::parse("work")?;
    let (ns, created) = timed(|| repo.create_layer(&layer, &base));
    ctx.push(ctx.sample("create_layer", 0, ns, &created));
    let mut head = created?;
    ctx.disk("layer_created", None, None);
    for round in 1..=ctx.params.layer_rounds {
        if ctx.expired() {
            ctx.note("budget exhausted during layer captures; run is truncated");
            break;
        }
        let n = round * ctx.params.layer_changed;
        let mut entries = Vec::new();
        for (j, path) in paths.iter().enumerate().take(n) {
            let path = path.clone();
            if j % 5 == 4 {
                entries.push((path, None));
            } else {
                let mut content = fx.content(j, 500 + round as u64);
                content.truncate(LAYER_BLOB_MAX);
                entries.push((path, Some(regular(repo.put_blob(&content)?))));
            }
        }
        for j in 0..(n / 5).max(1) {
            let target = repo.put_blob(format!("target-{round}-{j}").as_bytes())?;
            entries.push((
                RepoPath::parse(&format!("added/a{j}"))?,
                Some(FileEntry::Symlink { target }),
            ));
        }
        let changes = SparseChanges::new(entries)?;
        let entries_count = changes.entries().len() as u64;
        let operation = format!("layer-capture-{round}");
        let (ns, update) =
            timed(|| repo.capture_layer(&OperationId::parse(&operation)?, &head, &changes));
        let mut s = ctx.sample("capture_layer", round as u64, ns, &update);
        s.operation = Some(operation);
        s.param = Some(entries_count);
        if let Ok(LayerUpdate::Applied { .. }) = &update {
            s.outcome = "applied".into();
        } else if let Ok(LayerUpdate::Stale { .. }) = &update {
            s.outcome = "stale".into();
        }
        ctx.push(s);
        head = match update? {
            LayerUpdate::Applied {
                state: LayerState::Active { head },
            } => head,
            other => return Err(format!("capture {round} not applied: {other:?}").into()),
        };
        let (ns, diff) = timed(|| repo.layer_diff(&head));
        let mut s = ctx.sample("layer_diff", round as u64, ns, &diff);
        s.param = Some(entries_count);
        s.result = diff.as_ref().ok().map(|d| d.len() as u64);
        ctx.push(s);
        diff?;
        let (ns, state) = timed(|| repo.layer_state(&layer));
        ctx.push(ctx.sample("layer_state", round as u64, ns, &state));
        state?;
        let (ns, frozen) = timed(|| repo.read_checkpoint(head.checkpoint()));
        let mut s = ctx.sample("read_checkpoint", round as u64, ns, &frozen);
        s.param = Some(entries_count);
        ctx.push(s);
        frozen?;
        ctx.disk(&format!("after_capture_{round}"), None, None);
    }
    // One checkpoint holding a large blob, cloned N times; clones reference, not copy.
    let big = fixture::bytes(ctx.params.seed ^ 0xB16, ctx.params.big_blob_bytes);
    let big_id = repo.put_blob(&big)?;
    let changes = SparseChanges::new([(RepoPath::parse("big/blob.bin")?, Some(regular(big_id)))])?;
    let (ns, update) =
        timed(|| repo.capture_layer(&OperationId::parse("layer-capture-big")?, &head, &changes));
    let mut s = ctx.sample("capture_layer_big", 0, ns, &update);
    s.bytes = Some(big.len() as u64);
    ctx.push(s);
    head = match update? {
        LayerUpdate::Applied {
            state: LayerState::Active { head },
        } => head,
        other => return Err(format!("big capture not applied: {other:?}").into()),
    };
    let checkpoint = head.checkpoint();
    let (ns, check) = timed(|| repo.check());
    let objects = check.as_ref().ok().map(|c| c.objects as u64);
    ctx.push(ctx.sample("check", 0, ns, &check));
    ctx.disk("before_clones", Some(big.len() as u64), objects);
    let mut clone_heads = Vec::new();
    for n in 1..=ctx.params.clones {
        if ctx.expired() {
            ctx.note("budget exhausted during clones; run is truncated");
            break;
        }
        let id = LayerId::parse(&format!("clone-{n}"))?;
        let (ns, cloned) = timed(|| repo.clone_layer(&id, checkpoint));
        let mut s = ctx.sample("clone_layer", n as u64, ns, &cloned);
        s.param = Some(n as u64);
        ctx.push(s);
        let cloned = cloned?;
        let verified = cloned.revision() == 0
            && cloned.checkpoint() == checkpoint
            && repo.layer_state(&id)?
                == LayerState::Active {
                    head: cloned.clone(),
                };
        let mut check = ctx.sample("clone_state_check", n as u64, 0, &Ok(()));
        if !verified {
            check.outcome = "mismatch".into();
        }
        ctx.push(check);
        clone_heads.push(cloned);
        ctx.disk(&format!("after_clone_{n}"), Some(big.len() as u64), None);
    }
    for (n, clone) in [
        (1, clone_heads.first()),
        (clone_heads.len(), clone_heads.last()),
    ] {
        if let Some(clone) = clone {
            let (ns, diff) = timed(|| repo.layer_diff(clone));
            let mut s = ctx.sample("layer_diff_clone", n as u64, ns, &diff);
            s.result = diff.as_ref().ok().map(|d| d.len() as u64);
            ctx.push(s);
            diff?;
        }
    }
    for (n, clone) in clone_heads.iter().take(3).enumerate() {
        let operation = format!("discard-clone-{}", n + 1);
        let (ns, update) = timed(|| repo.discard_layer(&OperationId::parse(&operation)?, clone));
        let mut s = ctx.sample("discard_layer", n as u64 + 1, ns, &update);
        s.operation = Some(operation);
        ctx.push(s);
        update?;
    }
    let (ns, history) = timed(|| repo.layer_history(&layer));
    let mut s = ctx.sample("layer_history", 0, ns, &history);
    s.result = history.as_ref().ok().map(|h| h.len() as u64);
    ctx.push(s);
    history?;
    let (ns, check) = timed(|| repo.check());
    ctx.push(ctx.sample("check", 1, ns, &check));
    let check = check?;
    let mut count = ctx.sample("layer_count_check", 0, 0, &Ok(()));
    count.result = Some(check.layers as u64);
    if check.layers != clone_heads.len() + 1 {
        count.outcome = "mismatch".into();
    }
    ctx.push(count);
    Ok(())
}
