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
