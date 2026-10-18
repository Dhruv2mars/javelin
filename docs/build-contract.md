# Javelin build contract

This is the accepted product behavior for the GA build. Implementation choices may
change when evidence supports a simpler or more reliable solution. A change to
product behavior must be reconciled with this contract through the hub.

## Product boundaries

Javelin replaces Git and GitHub for agent-scale work. Javelin CLI is an
online-first client: it attaches to a hosted repository, captures changes in local
Layers, uploads blobs and checkpoints as they are captured, and materializes
Layers. Javelin hosting composes, publishes, checks conflicts and computes derived
data. Rule: assume online for speed, never for correctness. Layer work, capture
and checkpoints continue during an outage, and queued uploads and Publishes replay
idempotently afterwards. `git3-direction.md` records this decision. On 2026-10-18
it superseded the earlier boundary "Javelin CLI is a native local VCS".

Agents, IDEs, orchestration, model selection, and task assignment belong to other
tools. Javelin accepts traces and lifecycle events supplied by those tools.

Runtime, storage, tests, and user workflows never invoke or interpret Git.
Git is permitted for developing Javelin itself. Git import, export, compatibility,
and migration bridges are not requirements for this build.

Earlier implementations and binaries are not authoritative. The current scaffold
is the starting point. Reuse earlier work only after verifying it against this contract.

## Version control

- A World is the repository. Each accepted World version is immutable.
- Accepted versions form one ordered sequence with one publication authority.
- All human and agent edits happen in isolated Layers over a specific version.
- Layers share immutable content and record changes as changesets. Where the
  filesystem supports copy-on-write clones, a Layer never duplicates unchanged
  file data. Elsewhere the copy fallback is reported, never silent.
- A Layer exposes ordinary filesystem paths usable by external tools.
- One parent agent owns its Layer, and its subagents can share it. The parent chooses
  when a child needs isolation.
  Human-only work also happens in a Layer. Human edits in an existing Layer belong
  to that Layer's work.
- A Layer clone starts a separate isolated Layer from an immutable snapshot.
- Stable Layer changes are checkpointed automatically and remain recoverable.
  Checkpoints upload as they are captured; there is no push.
- Stack combines changes into a target Layer. Mechanically compatible edits can
  combine within one file. Structural conflicts remain explicit for its owner to resolve.
- Javelin reports World advances and conflicts. Layer owners control refresh,
  conflict resolution, and acceptance. Javelin does not invent semantic resolutions.
- Nonintersecting World advances do not force a Layer refresh. Intersecting changes
  require rechecking before acceptance.
- Publish accepts the whole Layer as the next World version. Partial acceptance
  requires preparing another Layer.
- A published Layer becomes read-only. Further work starts by creating or cloning
  another Layer from the current World. Javelin retains the Layer identity, final
  checkpoint reference, supplied agent context, Stack ancestry, and publication
  relationship. The workspace, overlay, and derived caches may be discarded when
  convenient. A published Layer remains inspectable through its immutable records.
- Publish validates VCS correctness and permissions. Tests, builds, reviews, and
  security checks are optional repository policy, not mandatory default Publish behavior.
- Final World advancement is ordered, atomic, durable, and idempotent. Preparation
  and independent work remain concurrent.
- Discard does not change accepted history. Restore preserves earlier immutable
  history rather than silently rewriting it.
- Code and supplied raw agent traces are immutable, content-addressed data with
  small references. Human-only work requires no agent trace. Missing agent traces
  produce a warning and permit an explicit Publish override.
- Changesets are the source of truth. Directory trees, history, blame, diffs and
  search indexes are derived data, computed on the server. Asynchronous indexing
  does not block Publish.
- Ignore rules affect untracked discovery. They never delete tracked content.
- Optional prepared environments can share immutable dependencies and give each
  Layer its own writable changes. They do not turn Javelin into an orchestrator.

A Layer is a plain directory cloned from the repository's materialized base with
the kernel's copy-on-write primitive, and changes are captured without a daemon at Javelin
commands and supplied agent lifecycle events. No mount, file provider, extension,
root helper or background process is required. `workspace-approach.md` records the
decision, measurements and limits; it superseded the provisional FSKit, OverlayFS
and ProjFS choices on 2026-10-17. Ordinary filesystem behavior, crash recovery, and
concurrency must be verified on each platform.

Storage is split by primitive (`git3-direction.md`, 2026-10-18). Blobs are
content-addressed objects: plain files locally, pending the recorded storage
benchmark, and object storage on the server. The World is an append-only
changeset log with one compare-and-swap head. SQLite holds Layers, publications
and indexes as a rebuildable derived index. It is not the storage core. A daemon,
a worker API or a new framework is not a settled requirement. Choose the smallest
design that meets the contract.

## Hosting ownership

Each hosted repository has one logical node on one user-chosen infrastructure
provider. A logical node may share physical infrastructure with other repositories.

The repository node owns immutable objects, World history and publication authority,
Publish Requests, revisions, reviews, comments, approvals, checks, policy, applied
permissions, operation IDs, and repository activity.

The central control plane owns identity, organizations, memberships, repository
discovery and addressing, provisioning, billing connections, and derived caches.
The central control plane is not self-hosted. Repository creation presents name,
visibility, Publish setting, provider, and create.

After authentication, CLI and browser clients communicate directly with the node.
Routine repository operations do not require a live control-plane lookup.
Already-authorized clients can continue supported actions during a control-plane
outage within credential validity and applied permissions. A provider outage pauses
that repository's remote operations while local Layer work, capture and
checkpoints continue; checkpoint uploads and Publishes queue and replay
idempotently when the node returns.

One repository initially uses one provider. Its World authority is a
quorum-replicated log, so no single node failure stops Publish. Mirroring across
providers and cross-provider failover remain outside the current design. On
2026-10-18 `git3-direction.md` superseded the earlier exclusion of quorum
replication.

## Contributions and reviews

- Authorized members can Publish directly when permitted or open a Publish Request.
- Outside contributors attach to a public repository, create a Layer, and submit
  a Publish Request directly upstream. A hosted fork is optional. This replaced
  "clone locally" on 2026-10-18; attach keeps a bounded cache, not full history.
- Each Publish Request has one author. Only that author submits new revisions.
- Each submitted revision is immutable. Later local checkpoints do not mutate it.
- Approval applies to the exact code and context revision. A changed revision
  invalidates approvals. Comments and revision history persist.
- Approval and Publish are separate actions. Publication must check the exact
  revision and current permission and policy state at its correctness boundary.
- Maintainers review and Publish when authorized. They prepare separate work
  rather than silently rewriting another author's proposal.

An attached client retains the same hosted repository and World authority. A fork
creates a new hosted identity and independent future publications. It inherits
history through the selected World version, including permitted code and context,
and retains that data independently of the source. Credentials, memberships, and
policies do not transfer. Restricted traces cannot gain broader access.

## Protocol and first provider

The direction is versioned HTTPS with HTTP/2 support, compact binary messages where
useful, scoped direct object transfers, optional WebSocket notifications, and operation
IDs for retries. CLI device credentials and short-lived browser credentials are the
authentication direction. Transfers follow derived directory trees from the root:
trees first, then the blobs a client wants, with no packfiles (`git3-direction.md`).
Schemas and algorithms remain implementation work.

Cloudflare is the provisional first provider: Worker endpoint, R2 immutable objects,
a small World publication coordinator, independently partitionable review state,
and optional notifications. This does not prescribe one Durable Object per request
or per Publish Request. Only final World advancement needs repository-wide total
ordering. Revisions, approvals, and permission changes need their own correct ordering.

Objects must be readable and verified before committing references. Separate object
and metadata stores do not imply one shared transaction. Successful acknowledged
Publishes must survive restart and retry.

CI workflow syntax compatibility is accepted. CI execution remains to be designed
and verified. The broad forge wishlist is later scope, not a reason to delay proving
the local and hosted development workflows.

## Verification gates

1. Prove native immutable objects, initialization, World ordering, restart behavior,
   and rejection of invalid or missing objects with a runnable verification suite.
2. Prove the Layer backend on APFS, btrfs or XFS, and ReFS, with an unprivileged
   user and no running Javelin process between commands. Read, write, atomic-save
   rename, delete, traversal, symlinks, file watching, a compiler or test run,
   cross-Layer isolation, automatic capture of every edit, and interrupted-operation
   recovery must pass. Unchanged file data must stay shared with the base, verified
   by disk-usage measurement. The copy fallback must pass the same behavior checks
   on ext4 and NTFS and report itself as a copy.
3. Complete a real local project with externally managed parallel agents. Exercise
   checkpoints, Layer clone, Stack, conflicts, Publish, discard, history, restore,
   ignore discovery and supplied traces. Pass through a rerunnable acceptance
   script with explicit expected outcomes from this contract, including unchanged
   source snapshots after clone, compatible advances without forced refresh,
   explicit intersection conflicts, and the missing-agent-trace warning and override.
4. Complete a hosted public and private repository workflow through CLI and browser.
   Exercise direct Publish, exact review approvals, revisions, upstream contributions,
   forks, permissions, retries, and outages. A rerunnable acceptance script must
   assert exact revision authorization, immutable prior revisions, one durable
   outcome after a lost reply, inherited fork data without inherited credentials,
   no access to restricted traces, and the distinct control-plane/provider outage
   outcomes in this contract.
5. Initial GA requires macOS, Linux, and Windows support. Verify installation,
   upgrade, recovery, and complete workflows on each. Record supported architectures
   in the release manifest. A platform that has not passed remains a GA blocker.
   The user explicitly confirmed all three for initial GA in this hub on
   2026-10-14. This supersedes the earlier Q61 macOS-first release direction.
6. Measure hot-repository and many-repository workloads. Record fixtures, machines,
   backend, limits, errors, and cost assumptions. Baselines inform engineering;
   they do not pass this gate. Before a release acceptance run, commit its workload
   shape, repository/file/history sizes, concurrent Layer/client counts, duration,
   numeric latency/throughput/resource/error limits and cost budget. A missing
   threshold or exceeded limit fails acceptance. Changes to criteria require a
   recorded rationale and a new run; do not relax them retroactively to pass the
   same result. Numerical criteria remain open engineering work. Measure Layer
   filesystem paths separately from API throughput.
7. Verify public release artifacts and documentation as a new developer. Publish
   reproducible evidence, known limits, and supported versions before declaring GA.

The complete local and hosted acceptance scripts must run with a failing `git`
stand-in on PATH whose positive control proves it detects invocation. Check the
resulting marker is absent during product workflows and inspect runtime/storage
dependencies for Git invocation or interpretation. Developer checkout and build
metadata collection remain outside those product executions.

## Work ownership

This hub owns `TASKS.md`, this contract, and the integration sequence. Every separate
implementation thread owns one PR in its own worktree. Work branches may start from
verified unmerged dependencies so progress does not require premature merging.
Dependent PRs must name their base and cannot claim independent acceptance.

Workers report evidence and a VERIFIED, NOT VERIFIED, or INCONCLUSIVE result.
The hub inspects the actual artifact before checking an item. No PR is merged
without explicit user instruction.

The user's 2026-10-16 workflow update supersedes earlier draft-first and repeated
review requirements. Each owner implements its task, runs relevant checks, opens
and links one regular PR when ready, reports the result briefly and finishes.
Do not create planning drafts or routine separate architect, audit, acceptance or
handoff rounds. Keep the solution and coordination simple. Existing verification
records are historical evidence; they do not prescribe that process for new work.
Maintain T3 thread ownership and settle completed threads. Product correctness and
release verification still require actual working behavior.

## Decision sources

- Git 3.0 direction, accepted by the user on 2026-10-18 in the research thread:
  `git3-direction.md`. It records the online-first model, the Bonsai data model,
  the storage split, the rolling base and the VFS stance.

- Final design and consistency review: `codex://threads/01a12eae-a899-7ad3-9462-5f95a64e76cc`.
- Earlier product and hosting grilling: `codex://threads/01a084f7-7b5f-79d1-b381-d686c430a7f9`.
- Explicit fork-history answer, "Inherit history through the selected version":
  `t3-thread://v1/6bbb5b4b-899f-4bc7-b999-a78669bcc177` and
  `codex://threads/01a12ebd-0de7-7113-b9ba-b99a0771ce30`.
- Read-only published Layers and retained provenance: earlier grilling Q82 and Q121,
  each answered "agreed" by the user. Local transcript
  `~/.codex/sessions/2026/09/09/rollout-2026-09-09T12-30-06-01a084f7-7b5f-79d1-b381-d686c430a7f9.jsonl`,
  verified by the hub on 2026-10-14.
- Marathon authorization, replacement of earlier code, and one-PR-per-thread rule:
  `t3-thread://v1/e659350e-23d8-4473-98e8-d3609b784340`, user messages on 2026-10-13.
  Local backing transcript: `codex://threads/01a135d7-d344-72c2-b0c3-331850d6b2ec`.
- Build direction confirmation of record: the user's opening messages in that
  hub direct the implementation to follow the final design and authorize the OSS
  GA marathon. They do not alter the prior thread's formal confirmation metadata.
- Initial GA platform scope: in this hub on 2026-10-14 the user answered
  "All three platforms for initial GA" to the release-scope question. The earlier
  Q61 answer was "first os is macos only" with portable architecture; presenting
  all three as an already accepted gate before this answer was a hub attribution
  error. The new explicit answer settles that scope.
