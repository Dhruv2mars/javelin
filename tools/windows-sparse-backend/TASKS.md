## Windows sparse backend workstream

Scoped checklist for PR #14, relocated here from the root `TASKS.md` when `origin/main`
(`dd4c915`) was merged; the root file is the hub's and now records this unit itself.

Owned by `test/windows-sparse-backend`, based on unmerged `feat/windows-projfs-proof`
at exact `cc94e22b8a316b4308305218aab003c172c96e42`. That base is a dependency, not
acceptance; PR #9 accepted only its bounded collection, and its backend stays INCONCLUSIVE.
Owns only `tools/windows-sparse-backend/`, `docs/windows-sparse-backend.md`,
`.github/workflows/windows-sparse-backend.yml` and this section.
Done means one bounded disposable hosted Windows attempt, with exact tested source,
job, artifact digest, raw records, cost and cleanup, that gives an honest per-scope
verdict for: the directory-rename root cause (projected, local and full directories);
binary/read/write/save/delete/symlink/compiler on separate native fixtures;
ACL-enforced base and frozen-snapshot protection against an ordinary user clearing
attributes; two clones sharing one immutable snapshot with private writes; actual
mid-write kill versus completed closed-handle preservation; hydration/full-copy cost
recorded without overclaim. Windows CRLF/fsync checks stay separate from the unchanged
common probe. A real platform refusal ends the attempt with a documented verdict.
No laptop install, privilege, reboot or security change. No Windows product acceptance
unless the entire minimum backend passes.

- [x] Read instructions, hub contract, PR #9 evidence and review corrections; define done.
- [x] Open exactly one draft PR against `feat/windows-projfs-proof`, link it, report planning SHA to hub. PR #14, planning `34b0aee`, message `windows-sparse-backend-pr14-planning-v1`.
- [x] Ground rename/ACL/cache behavior in primary Microsoft docs. ProjFS placeholder directories never become full (cache-state doc); VFS for Git documents partial-directory rename as unsupported (0x80070032); PR #9 log has no PRE_RENAME for `dir`. Actual SDK `cfapi.h` declarations are captured by the hosted run.
- [x] Compare at least two filesystem shapes with independent review; record limits. Cloud Files sync root per Layer chosen; ProjFS rename matrix kept as control; container layers rejected. Two read-only reviews (Codex gpt-6.1-sol high, Claude Opus 5.5 medium) agreed; neither executed code.
- [x] Implement the bounded native backend and separate checks in the owned directory. `sparse.cpp` Cloud Files provider/capture/audit, pinned unchanged PR #9 `projfs-provider.cpp`, `checks.mjs`, `run.ps1`, `user-run.ps1`.
- [x] Run one bounded hosted attempt; retain raw archive and provenance; audit offline. Eight attempts (bound extended from six for harness defects); final `8712866` run `38508454070`, digest `c7b9b68d…d999`. `rtk proxy bun tools/windows-sparse-backend/verify-evidence.mjs` passes and rejects a tampered raw file.
- [x] Independent read-only review; fix material findings; report verdicts to hub. Codex round hit its usage limit; Claude Opus 5.5 high reviewed `fdd0f4b` (same family). One high and three medium findings fixed and rerun in attempt 8; remaining lows documented.
- [x] Hub audit corrections (docs/TASKS only, no rerun): grant probe ran `icacls /grant runneradmin:F`, not the proof user, so the attempt 8 fix did not take effect (WRITE_DAC refusal still holds); attempt 5 concluded `failure`; capture PARTIAL guard, junction walk bound, untrusted manifest names/blob digests and owner-editable marker recorded as known limits.
- [x] Bounded fixture VERIFIED on Cloud Files: rename matrix, native ordinary ops, ACL base and frozen snapshot (including the frozen root itself; grant probe named the administrator, not a self-grant), two clones of one snapshot, mid-hydration and mid-write kills vs closed-handle preservation, dehydrated untouched peers. ProjFS base-directory rename refused (error 50, no PRE_RENAME).
- [ ] Unchanged common probe: CRLF compiler, Bun directory fsync EPERM, final integrity and path boundary NOT VERIFIED (probe portability; not owned here).
- [ ] INCONCLUSIVE: storage cost at scale, metadata preservation, nested symlinks, AV hydration, power loss, concurrency, provider-only kill during a write, capture of clones, production registration, native binding, autosave, Windows acceptance, Gate 2, GA.

## Windows capture hardening, bounded follow-up to PR #14

Owned branch: `fix/windows-sparse-capture`. Dependency: unmerged
`test/windows-sparse-backend` at `8a77d4222a0324e1090dd1a03b09251fc76a67d4`.
Prior feasibility was tested at `8712866`; its bounded acceptance does not accept
this dependency independently, Gate 2, or GA.

Done means exact-source hosted Windows 2022 evidence reproduces inherited defects
before fixes and proves fail-closed capture, bounded contained traversal, validated
manifests and blob digests, protected snapshot identity, unchanged previous
snapshots and peers after refusal, and preserved prior bounded successes. The
offline verifier must reject tampering and bind the current source. Active editing
is unsupported; this unit does not claim atomic snapshots during user edits.

Owned paths: `tools/windows-sparse-backend/**`,
`docs/windows-capture-hardening.md`, `.github/workflows/windows-sparse-backend.yml`,
and this appended checklist. Existing evidence stays immutable.

- [x] Recover the same clean app-bound worktree and branch at exact dependency.
- [x] Open and T3-link draft PR #19; report planning SHA `a2528fa` and ownership to hub.
- [x] Compare two minimal identity/capture designs with independent Codex support.
- [x] Prepare hosted-only controls while leaving inherited `sparse.cpp` unchanged.
- [x] Retain run `38570489714` at `bd42718` as a failed helper compile attempt.
  Workflow success was collection only: helper compile exit 2, `NTSTATUS` missing
  in cfapi declarations, account not created, cleanup complete. No runtime
  reproduction or backend result was established.
- [x] Confirm helper header compatibility in a focused hosted compile-only control.
  Run `38695475455` at `809e71d` still failed with undeclared `NTSTATUS`;
  `ntstatus.h` supplies constants, while the inherited provider also includes
  `bcrypt.h` before `cfapi.h`. Retained this failed compile attempt unchanged.
  Run `38695682464` at `5683058` compiles all three binaries with exit 0,
  no warnings, no timeouts, no blockers, no proof account and complete cleanup.
- [ ] Reproduce defects on unchanged inherited source in disposable hosted Windows.
  Run `38696161679` at `80f03d0` establishes junction capture, editable-marker
  acceptance and malformed-manifest loading. Partial zero/short controls are
  NOT ESTABLISHED: creation failed `8007018b`, subsequent paths were missing.
  Actual nonadmin SID, cleanup, unchanged base/snapshot and prior bounded phases
  inspected. Preserve all raw records; SDK/fixture preconditions remain next.
- [x] Implement minimal fixes and focused adversarial regressions.
  Oct16 resumed flow: implementation and proportional hosted checks in this same
  PR; no new mandatory review or evidence-only finishing commit. Retained
  `38716055510` proves failed-read empty-blob acceptance on unchanged source;
  nonzero returned short prefixes remain unconfirmed. Checked reads, bounded
  traversal, validated manifests/digests, protected binding and quiescent capture
  refusal are implemented; focused Windows checks pending.
- [ ] Verify ordinary nonadmin SID, prerequisites, limits, killed children and cleanup.
- [ ] Complete the focused exact-source hosted Windows checks. The user's Oct16
  flow supersedes the earlier mandatory independent-review round.
- [ ] Run fresh-head offline verifier and tamper checks; report scope and limits to hub.

Parent integration: `ca663906f6d503ffd61fc8ce85cb61f7d6f54207`, including main
`466d9a7bb2938d29cd7017f0dd248c212928f0dd`. Root TASKS matches that main byte for
byte. Tested `b87fedb` remains historical proof. The exact-sharing-error and
negative-control change at `a93ef0b` has no Windows run yet; current-head CI after
this integration must establish it before reporting success. Backend, fixture,
workflow and retained evidence bytes are unchanged by integration.
