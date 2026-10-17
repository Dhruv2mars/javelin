# Windows projected filesystem proof workstream

Owned by `feat/windows-projfs-proof`, based on `feat/linux-sparse-layer-proof`.
Done means one rerunnable bounded disposable GitHub-hosted Windows attempt retains
raw prerequisite results, exact executable source SHA, job and artifact provenance,
and per-scope verdicts. If ProjFS can run without reboot, implement a minimal native
provider and measure actual ordinary-path behavior and sparse private state.
If feature, API, compiler or other required prerequisites are missing, finish with
availability evidence and an INCONCLUSIVE backend verdict. Product Gate 2 and GA
remain unverified. No Windows installation or projection runs on the laptop.
Only owned Windows tools, documentation, workflow and this checklist may change. Root `TASKS.md` stays as on `main`.

- [x] Read instructions and latest hub contract; define bounded self-verifiable done.
- [x] Open exactly one draft PR against `feat/linux-sparse-layer-proof`, link it immediately, and report planning SHA to the hub. PR #9; planning `387cbaa`; stable hub message `windows-pr9-planning-v1`.
- [x] Apply architect and compare two source-backed native provider shapes before coding. Direct-tree/native root owner chosen over blob-index/per-root write manifest; retain only immutable changed/deleted snapshot model for clone. Same-model reviews have limited diversity.
- [x] Implement bounded prerequisite capture for feature, API, compiler, identity and Unix utility availability. No provider or projection is present in this first unit.
- [x] Run actual hosted Windows prerequisite jobs and inspect raw archives. Initial `4d817c5` run `38465330661` exposed a missing `<cwchar>` include, not an unavailable compiler. Corrected source `648474c` run `38465691765` confirms enabled feature, native compiler and all required exports. Retain both original archives.
- [x] Execute native provider and ordinary-path checks as a disposable non-admin CI account in run `38469723137` at `21b2dda`, with Windows job child-termination records (17 `activeAfterTermination=0`, 20 children exit 0, owned root removed). Execution and evidence only; the gate below failed.
- [ ] Ordinary-path behavior gate: NOT VERIFIED. Projected base placeholder directory rename ENOTSUP; common probe exit 1 (rename, traversal, CRLF compiler output, writer `fsync` EPERM, final integrity).
- [ ] Restart recovery: only completed, closed-handle state survived provider kill and reopen. Mid-write kill and atomic-save recovery INCONCLUSIVE.
- [ ] Sparse clone and storage cost: one clone copied only full files plus tombstones. Multi-clone sharing, ACL-enforced snapshot immutability (only the read-only attribute was proven), metadata edits, aliases and storage cost INCONCLUSIVE.
- [x] Preserve original artifact, audit source hashes offline, and document exact evidence and gaps. Eight attempts retained byte-identically; CRLF checkout transform recorded; `rtk proxy bun tools/windows-workspace-proof/verify-evidence.mjs` passes and rejects a tampered raw file.
- [x] Report VERIFIED/NOT VERIFIED/INCONCLUSIVE scopes, dependencies and blockers to the hub; leave the single PR draft and unmerged. Backend, Gate 2 and GA INCONCLUSIVE. Hub review r1-3ad8241 corrections applied to docs and this checklist only.
- [ ] Later unit, not this one: directory rename (base placeholder, local and full), common-probe Windows CRLF/fsync portability, probe/native fixture leftover isolation, ACL-enforced snapshots and mid-write kill recovery.
- [x] Integrate `origin/feat/linux-sparse-layer-proof` (`40046e1`, which contains current `main`). Root `TASKS.md` taken exactly from `main`; this checklist moved here. Windows executable sources and evidence unchanged.
- [x] After PR #7 merged (`c48209f`): retarget PR #9 to `main`, merge `main` (root `TASKS.md` byte-identical), and fix Greptile P2 findings in `backend.ps1` (keep user results on timeout) and the workflow path filter (shared probe files). The audit names both post-evidence changes; retained evidence unchanged.
