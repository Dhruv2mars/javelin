# Git 3.0 direction

Status: accepted by the user on 2026-10-18. This amends `build-contract.md`
(product boundaries, hosting ownership, contributions) and `workspace-approach.md`
(always-online stance, VFS wording, immutable per-version bases). Where those
documents conflict with this one, this one wins.

## Vision

Javelin replaces Git and GitHub.

- **Git is the wrong primitive for agents.** It was designed for offline humans
  who each hold the full history. Agents work online, in parallel, through a
  harness, and merge at a rate Git's model and GitHub's infrastructure were not
  built for.
- **GitHub cannot carry agent-scale throughput**, and its outages stop work.
- **Javelin is built from scratch.** It has no dependency on Git or on a
  general-purpose database at its core, so the core can be rethought.
- **North star: extremely fast and scalable from fundamentals.** Costs scale with
  the size of a change, not the size of the repository or its history.
- **No complexity for its own sake.** Every mechanism below exists because a
  measured cost or a correctness rule needs it.

## Decisions

### 1. Online-first, outage-tolerant

Everyone working with Javelin, human or agent, is online, because they work
through an agent harness. The rule is: **assume online for speed, never for
correctness.**

- **Attach, not clone.** There is no clone and no full local history. The local
  store is a bounded content-addressed cache of what this machine has used.
- **The server composes.** Compose, Publish, conflict checks and derived data
  (history, blame, diff, search) run on the server. The client captures changes,
  uploads blobs and materializes Layers.
- **Nothing to push.** Checkpoints upload as they are captured, so work is
  visible and durable as soon as it exists.
- **Outages degrade, never block local work.** Layers, capture and checkpoints
  keep working locally. Checkpoint uploads and Publish requests queue and replay
  on reconnect. Publish operation IDs are already idempotent, so a replay gives
  exactly one outcome. A replayed Publish can still report a conflict; its owner
  resolves it as usual.
- **No single point of failure in hosting.** The World authority is a
  quorum-replicated log.

This supersedes "Javelin CLI is a native local VCS" in `build-contract.md` and
"We do not take Scion's … always-online client. Javelin stays local-first" in
`workspace-approach.md`.

### 2. Data model: Bonsai, fully

- **Changesets are the source of truth.** A World version is the set of paths it
  changed relative to its parent, with content-addressed blob pointers. The
  format-5 change makes this true for World storage.
- **Merkle directory trees are derived data.** Each version's tree is computed
  from the parent tree plus the changeset: only the directories on the path of a
  changed entry are rewritten, so the cost is O(change × depth). Unchanged
  subtrees are shared by hash.
- **Large directories are prolly trees.** A directory's sorted entries are split
  into chunks at content-defined boundaries (a hash of the entry falls below a
  target, as in Dolt), and chunk references are chunked the same way up to one
  root. A small directory is a single chunk, so one structure covers every
  directory. Changing one entry in a 50k-entry directory rewrites a few KB, not
  the whole listing.
- **One structure serves four jobs:**
  - materializing a base or a sparse Layer (walk only the wanted subtrees);
  - protocol fetch (root first, then trees, then blobs; no packfiles, so no
    repacking);
  - conflict checks (an unchanged subtree hash skips the whole subtree);
  - verification (one root hash identifies a version's full content).
- **Limits become per directory and per chunk.** The repository-wide
  `MAX_ENTRIES` cap of 65,536 in `crates/javelin-protocol` goes away once the
  protocol moves to trees.
- **Tree objects are immutable and content-addressed,** so every cache tier can
  keep them forever.
- **Derived data uses one recursive mechanism.** A derived value for version V is
  computed from its value at the parent and the changeset V. Trees, path history,
  blame, trace blame and search indexes are all instances of it.

### 3. Storage split by primitive

SQLite is not the core. Each primitive gets the simplest storage that fits it.
Javelin builds these primitives itself instead of adopting a general-purpose
database.

| Data | Local | Server |
| --- | --- | --- |
| Blobs | plain content-addressed files (a) | object storage |
| World | append-only changeset log, single CAS head, one file (b) | quorum-replicated log |
| Layers, publications, indexes | SQLite, as a rebuildable derived index (c) | same role |

- **(a) Blob files** let bases and Layers materialize by per-file clonefile or
  reflink from the blob store: zero byte copies, and about 1× disk for the store
  plus every base and Layer. Copy-fallback filesystems (ext4, NTFS) still copy.
- **(b) The World log** is append-only. Advancing it is one compare-and-swap on
  the head. That matches Scion's single-row ref update.
- **(c) SQLite stays** for Layers, publications and indexes for now. The target
  is that it holds only state rebuildable from the World log, the object store
  and the server's checkpoint records.

**(a) is gated by measurement.** A parallel thread benchmarks blob files against
SQLite. The criterion is fixed here, before the result:

> Go if, on the 100k-file `tools/layer-bench` fixture, on APFS and on one Linux
> reflink filesystem (btrfs or XFS), materializing a cold base from blob files
> (1) copies 0 file bytes, (2) is no slower than materializing from SQLite, and
> (3) uses at most 1.1× the tree's bytes for blob store plus base. Also,
> (4) a 10-edit checkpoint commit is at most 10% slower than with SQLite, and
> (5) a writer killed mid-write leaves no visible partial blob. Any miss is a
> no-go on that platform, and blobs stay in SQLite there.

### 4. Rolling base

- **One materialized base per repository per machine.** It is advanced in place,
  under a short lock, by applying changesets, so the cost is O(change).
- **Layer create is lock, clone, unlock.** A clone does not depend on its source
  afterwards, so advancing the base never affects an existing Layer.
- **Crash safety.** An advance records its target version before writing. A crash
  leaves that record set, and recovery drops the partial base; the next create
  rebuilds it cold. Reapplying over a partial base was rejected, because a half-written
  path cannot be told apart from a case-folding collision.
- **Replaces per-version bases** (`.javelin/bases/<N>`, two cached), whose disk use
  grew with the number of versions in use. Implemented in `src/workspace.rs`.
- Anything that reads a base after Layer creation must read the stored World
  version instead, because the base moves.

### 5. No kernel VFS on local machines

- Local Layers stay plain cloned directories (`workspace-approach.md`). No FUSE,
  FSKit, ProjFS or NFS loopback runs on developer machines.
- Prior art agrees. VFS for Git was superseded by Scalar and sparse-checkout for
  new deployments, largely because Apple deprecated the kernel features its macOS
  port needed. EdenFS on macOS runs over NFS loopback (NFSv3).
- **Later, an optional `javelin mount`.** A FUSE filesystem that lazily fetches
  from the tree and blob caches, for cloud agent runners and Linux CI. It needs
  hosting first and is not a GA item.

### 6. Verdicts on other ideas

Recorded so they are not reopened without new evidence.

| Idea | Verdict | Reason |
| --- | --- | --- |
| Stale-read detection | Dropped | Harness read sets are incomplete and the warnings are noisy. O(change) Publish and revert, plus tests, are enough: undo beats friction. |
| Group-commit Publish | Dropped | Publish already composes inside its exclusive transaction. The real cost was critical-section length, which format 5 fixes. Anthropic's peak is about 0.35 Publishes/s (30,000/day). |
| Collision radar | Keep | The server sees every live Layer's changed paths (checkpoints stream) and warns about overlaps early, before Publish. |
| Layer pool | Low priority | Pre-created Layers save create time. Revisit after the rolling base. |
| Trace blame | Keep | Per-line agent, session and prompt provenance from supplied traces, computed as derived data. |
| Per-tool-call rewind | Dropped | Checkpoints are commit-like. Finer rewind belongs to the harness. |

## Rejected

- Clones and full local history; `push`.
- Kernel VFS on local machines.
- Per-version materialized bases.
- A general-purpose database as the storage core.
- A client daemon. Capture stays daemonless (`workspace-approach.md`).
- Packfiles and repository maintenance.
- A global merge-queue lock. Publish is a compare-and-swap; concurrent compatible
  Publishes rebase, and intersecting ones conflict.
- Stale-read detection, group-commit Publish and per-tool-call rewind (above).

## Open questions

- **Version identity.** Should the version ID cover the root tree hash, so that
  one comparison verifies a materialized checkout? Recommended: yes, once trees
  exist.
- **Chunk target and per-directory limits.** Exact sizes come with the tree PR,
  backed by measurement.
- **Layers at older versions.** Done: a Layer at version V < base is a clone of the
  rolling base with the reverse changeset applied, at O(distance) cost. The distance
  is not measured at scale yet; see TASKS.md.
- **Local cache bound.** Pick the eviction policy. Blobs referenced by the base,
  live Layers or unuploaded checkpoints are never evicted.
- **Fully local repositories.** Before hosting exists, the local store is the
  authority. Decide whether a no-hosting mode is a product mode after GA or only
  a test configuration. Recommended: keep an in-process authority behind the same
  protocol, with no separate code path.
- **SQLite rebuildability.** Layer state is primary in SQLite today. Record which
  tables cannot yet be rebuilt.
- **Quorum on the first provider.** Can Cloudflare's coordinator storage serve as
  the quorum-replicated World log? UNCONFIRMED; not researched yet.
- **Cache tiers.** Scion runs three tiers that answer identical RPCs. A shared
  cache tier for hosting is an option to decide with measurements; it is not a
  client daemon.

## Roadmap

Dependency order. Each step is one PR or a short series.

1. **Format-5 changeset World** (in flight): O(change) Publish and shallow
   hot-path validation.
2. **This decision.**
3. **Storage benchmark, blob files vs SQLite** (in flight): apply the go/no-go
   criterion above.
4. **Tree objects as derived data:** Merkle trees with prolly chunking, then the
   protocol on trees. This drops the 65k entry cap.
5. **Rolling base.**
6. **Collision radar and trace blame.**
7. **Online pieces:** attach, streaming checkpoints, server-side compose, outage
   queue and replay, and the replicated World log.
8. **Later:** optional `javelin mount`.

## Sources

Checked on 2026-10-18.

- **Scion,** Anthropic Engineering,
  [Redesigning source control](https://www.anthropic.com/engineering/redesigning-source-control).
  Checked against the post's text. CONFIRMED:
  - A changeset is the files changed relative to the first parent, plus
    content-addressed blob pointers.
  - A ref update is a compare-and-swap on one Spanner row.
  - Each type of derived data is defined recursively from its parents' values.
  - scion-daemon, scion-cache and scion-server answer identical RPCs, with a
    cache hit rate over 99% at each tier.
  - There are no packfiles. Fetch starts at the root tree, then trees, then blobs.
  - Clients are always online. Checkouts can be sparse or virtualized.
  - No global merge-queue lock; Anthropic tests post-merge instead.
  - Infrastructure is 200× smaller in CPU and memory, and CI saw a 25% CPU
    capacity win.
  - "As of September, we merged over 30,000 per day."
  - A footnote credits Mononoke's Bonsai data model.
- **UNCONFIRMED:**
  - The single-day peak of 30,401 merges on September 9 came from the research
    thread. It does not appear in the post's HTML text (looked at the text and
    image alt attributes). It is probably in a chart image.
  - Per-command speedups read from a chart in
    [Theo's video](https://www.youtube.com/watch?v=hiI0Vk4NgeY) (pulls 25×,
    status 207×, …) are not in the post's text, which only says checkouts are
    "two orders of magnitude faster".
- **Mononoke and Bonsai:** known here only through Scion's footnote and a
  secondary critique page. The Mononoke source was not read, so details beyond
  "changesets plus recursive derived data" are UNCONFIRMED.
- **VFS for Git,** [README](https://github.com/microsoft/VFSForGit). CONFIRMED:
  "for new deployments, we strongly recommend you consider Scalar instead of VFS
  for Git". The README does not say "maintenance mode".
  [The Story of Scalar](https://github.blog/open-source/git/the-story-of-scalar/)
  (GitHub blog). CONFIRMED: "Apple deprecated the kernel features that provided
  the filesystem virtualization", so Office moved to sparse-checkout instead.
- **EdenFS,** [Overview](https://github.com/facebook/sapling/blob/main/eden/fs/docs/Overview.md).
  CONFIRMED: "On macOS, EdenFS uses NFSv3", and on Windows it uses ProjFS.
- **Dolt prolly trees,** [storage engine docs](https://www.dolthub.com/docs/architecture/storage-engine/prolly-tree).
  CONFIRMED: chunk boundaries form where a hash value falls below a target. An
  edit rewrites about 4 KB per tree level.
- **Not re-read for this decision:** CitC, jj and Pijul. They informed the
  research thread, but nothing here depends on them.
