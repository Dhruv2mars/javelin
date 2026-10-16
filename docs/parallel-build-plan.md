# Parallel build plan

This records the earlier parallel launch plan. The current user pause and
working rules in `TASKS.md` take precedence. Each implementation thread owns one
regular PR, opened after implementation and relevant checks. Separate architect,
audit and handoff cycles are not required. Never merge without explicit user
instruction.

## Grounding and alternatives

Accepted source `e56fc86` provides immutable native repositories and Layer state.
Repository owns typed object storage and ordered compare-and-advance outcomes;
Layer owns sparse checkpoints, private heads, cloning and discard. The Linux
binding unit adds ordinary paths in PR #10 and owns shared root native files.

Three coordination shapes were compared. Serializing all work behind Linux
leaves independent platform and pure-component tasks idle. Concurrent edits to
root Cargo and repository modules create shared ownership and unstable contracts.
The selected shape gives each pure component or tool its own directory, doc,
workflow and worktree, using accepted native public APIs. One later integration
unit owns changes to shared files after dependency acceptance.

Independent review `javelin-parallel-build-boundaries-r1-20261014` traced the
actual native exports. Private `ObjectId::hash` and `Manifest::decode` are not
available to callers. Several native types serialize but do not deserialize.
New codecs reconstruct validated domain values and obtain IDs through
`Repository::put_blob/put_manifest`. Errors remain opaque to sibling components
because Linux adds variants. Standalone nested crates build with an empty
`[workspace]`, their own lockfile and target ignore; no root workspace edit is
part of this launch.

## Bounded ownership

| Unit | Draft PR | Branch | Deliverable |
| --- | --- | --- | --- |
| merge | [#12](https://github.com/Dhruv2mars/javelin/pull/12) | `feat/mechanical-merge-engine` | Pure deterministic three-way merge with explicit structural/content conflicts and actual native-manifest caller tests. |
| protocol | [#15](https://github.com/Dhruv2mars/javelin/pull/15) | `feat/repository-protocol` | Bounded versioned codec and verified object transfer/reference receiver between real native repositories. |
| collaboration | [#13](https://github.com/Dhruv2mars/javelin/pull/13) | `feat/review-permission-model` | Pure author-owned revision/exact-approval/applied-permission model; no durable store or publication ticket. |
| windows | [#14](https://github.com/Dhruv2mars/javelin/pull/14) | `test/windows-sparse-backend` | One bounded real sparse-backend attempt addressing rename and permission-enforced snapshot gaps, with honest refusal evidence. |
| macos | [#11](https://github.com/Dhruv2mars/javelin/pull/11) | `test/macos-sparse-backend` | One hosted macOS prerequisite and feasible mounted-backend attempt, without laptop or host-security changes. |
| packaging | [#17](https://github.com/Dhruv2mars/javelin/pull/17) | `build/native-development-packages` | Three-OS development CLI archives, checksums and clean-temp installation smoke; no public release. |
| benchmarks | [#16](https://github.com/Dhruv2mars/javelin/pull/16) | `perf/native-storage-baseline` | Reproducible release-native API cost and multiprocess contention measurements; no scale or filesystem-throughput claim. |

All native components and tools start from exact accepted `e56fc86`.
Windows starts from `cc94e22`, the corrected prior evidence handoff, which does
not establish backend acceptance. macOS starts from hub `c5c0ebe`. All bases
are unmerged. TASKS records exact thread IDs, worktrees and owned paths.

## Interfaces and integration gates

- Merge takes native manifests and an explicit blob loader. It returns retained
  entries, new bytes and explicit conflicts. Its caller stores new bytes.
  Stack/refresh persistence and owner-controlled resolution remain later work.
- Protocol owns versioned core object-transfer/publication messages and bounded
  decoding. The reference receiver verifies objects before advancing World.
  HTTP service, authentication, provider deployment and collaboration frames
  are outside this unit.
- Collaboration is a pure state machine. A revision names exact base, manifest
  and optional supplied context reference. Context object kinds are unresolved.
  Approval follows that exact revision. Current permission evaluation returns a
  result, never a transferable grant. Durable storage and rechecking state at
  the atomic publication boundary require later integration.
- Platform proof attempts keep complete copies and clonefile controls separate
  from sparse-backend evidence. Privileged setup is limited to disposable CI;
  no laptop security changes are authorized. Missing prerequisites and required
  behavior remain INCONCLUSIVE or NOT VERIFIED.
- Packaging verifies development CLI distribution only. Benchmarks measure
  native API behavior only. Neither establishes complete product workflows,
  release limits or GA.

Protocol, collaboration and merge owners exchange interface notes directly and
report unresolved native API requirements to this hub. No owner edits another
unit's files. Tests and runtime do not invoke or interpret Git. GitHub checkout
and developer PR metadata are development setup.

A future single integration PR may add root workspace membership and remove
standalone workspace markers when those components are accepted. It also owns
any justified core export changes. Whole-Layer Publish, automatic capture,
trace objects and complete hosting remain explicitly open.

## Continuation

Completion and check events wake their owner or this hub. The hub independently
reads real artifacts, records limits, and selects the next ready bounded unit.
The existing hourly hub continuation covers unattended gaps. No extra scheduler
or polling daemon is introduced. Routine unchanged status stays quiet.

