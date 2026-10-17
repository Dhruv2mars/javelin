# Repository protocol tasks

This checklist moved from root TASKS on 2026-10-16 to avoid shared-status
conflicts. Root TASKS now follows main exactly. Earlier source labels and
verification records below remain historical.

## Workstream: repository protocol message codec

Owner: separate T3 implementation thread commissioned by hub
`e659350e-23d8-4473-98e8-d3609b784340`. Branch `feat/repository-protocol`, draft PR
originally against `feat/native-layer-checkpoints`. Planning source is exact accepted Layer state
`e56fc86e3415217de2b960a7ea0dbe6d2eb450b1`; that base is an unmerged accepted
dependency, so this PR cannot claim independent acceptance. Owned paths only:
`crates/javelin-protocol/**`, `docs/repository-protocol.md`,
`.github/workflows/repository-protocol.yml`, and this section.

Done means `cargo test --locked` in `crates/javelin-protocol` passes a versioned
codec, a reference receiver on the public native API, adversarial refusals that
leave World unchanged, and a runnable sender/receiver process test between two
separate repositories with a lost reply and retry. The same suite passes in its own
hosted workflow. No HTTP, authentication, public endpoint, provider deployment,
checkpoint, trace or collaboration format is claimed.

- [x] Open the draft PR, link it, and report planning source and owned paths to the hub.
- [x] Compare JSON framing and compact binary candidates with the actual caller.
- [x] Implement negotiation, have/want, bounded blob and manifest batches, and publish.
- [x] Use validated wire DTOs; never deserialize native serialize-only types.
- [x] Refuse malicious, malformed, oversize and truncated input, and changed input under a reused operation ID; identical replay returns the durable outcome.
- [x] Prove separate-process transfer, lost reply and retry between two repositories.
- [x] Record API gaps for the hub instead of copying private native internals.
- [x] Obtain independent review and hosted three-platform evidence.
- [x] Hub: public bounded read or existence check, content-ID and raw ObjectId byte APIs accepted at `3a54021` (gaps 1-3).

Draft PR: https://github.com/Dhruv2mars/javelin/pull/15, linked in T3 Code.
Binary candidate B was selected over JSON candidate A; the rationale and fixture sizes
are in `docs/repository-protocol.md`. Locally, Rust 1.99.0 passes 21 tests with
formatting and Clippy clean. `bun scripts/mutate.ts` reports 24/24 guard removals
detected. Hosted runs passed on all three OSes for `58fadbf` (38505510520) and
`d2530fe` (38507397981). Review r1 (Codex) and r2 (Claude, after a Codex usage-limit
failure) are addressed. Native read allocation remains hub API gap 3.
Final tested source `02c7427ef18bedf933ab4a75bc8d7d46cd9dce3b` passed hosted run
38508849025 on Linux, macOS and Windows: 21 tests per OS, release build, and a PATH
git-shim control of without=0 and with=97. Head `29457a8` added documentation only.

### Historical hub Codex r2 corrections and dependency wait

Done for this correction pass means the documentation matches the audit's actual
allocation and storage evidence, corrected refusal fixtures pass locally, only
owned paths change, and the hub receives the committed result. This does not close
the HIGH Want allocation gap or provide fresh hosted acceptance.

- [x] Read fresh audit `acceptance.md`, `experiments.log` and `framing-experiment.log` in `/tmp/javelin-hub-audit-pr15-codex-r2/notes`.
- [x] Correct retained-object claims: the failed request does not publish; valid prefixes and mismatched actual bytes can persist and later legitimate requests may reference them.
- [x] Narrow sub-4-KiB allocation evidence to four fixtures; add the legal 16-MiB outer-frame counterexample.
- [x] Document existing UTF-8 Error codes in unfrozen version 1 and assert actual round-trip behavior.
- [x] Keep Publish as foundation CAS against current World; record that Layer binding requires newly negotiated semantics, since v1 rejects trailing bytes.
- [x] Verify corrected fixtures, formatting and Clippy locally and record the result.
- [x] Commit corrected fixtures and notify the hub of local evidence, retaining PR #15.
- [x] Wait for the hub's accepted native-object-access exact SHA before any dependent rebase or migration. PR #18 implementation alone is not acceptance.
- [x] After acceptance, migrate typed availability, cap-before-allocation reads, pre-store content-ID checks and ObjectId bytes within the same PR/owned paths.
The earlier requirement for another review and three-OS artifact-audit round was
superseded by the Oct 16 user workflow. Historical `02c7427` proofs do not cover new changes.

The audit's 55-byte Want for a 32-MiB blob causes a largest Rust allocation of
33,554,432 bytes. Its eight-reference manifest fixture causes 268,446,277 bytes of
cumulative Rust allocation, not peak RSS, excluding SQLite C allocations. Hosted
receiver exposure at that pre-migration source remained BLOCKED HIGH.
Quotas and retention remain future work.
That pass changed documentation and test fixtures only, with no runtime codec or
native source changes, no new PR, no watcher and no scheduler.

VERIFIED locally: correction fixtures `c133fab941fec7dc2eecfb81b05e1b1e0db44db4`,
Rust 1.99.0 on macOS arm64, `cargo fmt --all -- --check`,
`cargo clippy --locked --all-targets -- -D warnings` and
`cargo test --locked --all-targets` pass, 21 tests. NOT VERIFIED for these new
fixtures: fresh independent review and three-OS exact-head source proofs.
The 24 compiling-mutant proof is historical and was not rerun this pass.

### Accepted native object access migration

The hub accepted PR #18 source `3a54021b15ac5002c21f492b6bed07740c682421`,
recorded in hub `1791618`. Rebase completed at `d1fb504`; root files match exact
accepted source. PR #15 now targets main after hub integration.
Remote `5d3b59d` adds native documentation only and is not the tested native source.
Historical evidence above retains its original SHA labels and does not verify this
migration. The original bounded codec/process outcome still applies.
The Oct 16 user instruction supersedes the earlier review/audit cycle. Done for
this wave means public native-access migration, relevant behavioral checks,
a ready regular PR #15, and a brief result to the hub. No additional process rebase,
review round, artifact audit or evidence-only finishing commit is required.

- [x] Rebase onto accepted exact `3a54021`, preserving accepted TASKS prefix and only owned paths in the diff.
- [x] Replace Want probes, capped sender reads, pre-store hash checks and ObjectId byte conversions with accepted public APIs; remove old workarounds.
- [x] Check 55-byte Want allocation controls, kind/corruption/oversize/pre-store refusals and retained-prefix behavior.
- [x] Run formatting, Clippy, 25 component tests, two release process tests with Git-shim controls and seven focused migration mutations.
- [x] Prepare the checked migration for regular PR #15; the hub owns integration and merging.
- [x] Merge main `32f269e`, preserve its hub rules/checklist plus this section, and pass both focused process tests with migrated protocol bytes unchanged.

Retained limits include Manifests/Publish full graph and history CPU/memory work,
parser heap beyond the encoded-byte cap, SQLite C allocations and RSS, repeated
hashing work, quotas/retention, operation principal namespaces, and the 16-MiB outer
frame reservation. There is no HTTP, authentication, provider or GA claim. Whole-Layer
Publish is not this dependency and v1 Publish does not gain Layer fields.

### Bounded post-merge manifest preflight and batching follow-up

Done means invalid native manifests fail wire preflight before Want or object
transfer and leave the receiver untouched; real successful process pushes cross
Want count, blob count and frame byte boundaries, transfer each missing object
once in legal frames, and publish the expected manifest. Only protocol paths
are owned. Native/receiver integrity and existing graph/hosting limits remain.

- [x] Reuse the codec encoder for manifest preflight before object negotiation.
- [x] Prove the 4097-byte native path sends no objects and leaves receiver state unchanged.
- [x] Prove successful count-boundary and byte-boundary process pushes.
- [x] Run protocol formatting, Clippy, component and process checks.
- [x] Prepare the checked change for one regular PR against main; the thread opens and T3-links it as its final action.

VERIFIED locally on macOS arm64: `cargo fmt --all -- --check`,
`cargo clippy --locked --all-targets -- -D warnings`, and
`cargo test --locked --all-targets` (28 tests) pass. All five release process
tests pass with a failing Git shim first on PATH; the positive control exits
97 (without shim: 0), and the product-run invocation marker stays absent.
The count fixture offers 4097 objects and sends 4095 missing blobs in batches
of 1024/1024/1024/1023; the byte fixture sends two legal 8-MiB blobs separately.
No fresh three-OS or hosting acceptance is claimed.
