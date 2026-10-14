# Bounded mechanical merge

This unit extends the pure component from PR #12 at `61ecdd9`. It changes no
native storage, Stack, refresh, hosting, or Publish API. Its unmerged native
dependency remains `e56fc86`; this unit does not independently accept that core.

## Grounding and design sketch

`merge` receives three already-loaded native `Manifest` values. It first resolves
exact entry equality, deletion, additions and entry types. Only regular files
whose content changed on both sides call the loader, in sorted path order.
The loader returns an allocated `Vec<u8>`; the engine neither reads storage nor
hashes or decodes native objects. A NUL marks a binary content conflict. The old
line engine uses exact RawMyers matches, copies whole regions from one changing
side, and requires unique anchors kept by every minimal diff between different
sides' edits. It establishes mandatory anchors by removing each candidate base
line and repeating the exact diff. There is no resource outcome in the old API.

Two whole shapes are under comparison before implementation:

1. Keep RawMyers and reserve its worst-case work, including every repeated diff,
   before calling it. This minimizes matching changes but has a cubic worst-case
   allowance in line count. Tight deterministic bounds refuse ordinary files
   with scattered edits even when they would complete with a different algorithm.
2. Compute one exact prefix LCS table per side, with rolling suffix rows. For base
   row `i`, the best matching that omits it is
   `max_j(prefix[i,j] + suffix[i+1,j])`. The row is mandatory precisely when that
   value is below the full LCS length. Trace back the same prefix table for one
   exact matching. Keep the existing region and unique-separator rule. Reserve
   table cells and a conservative work allowance that includes compared bytes,
   table initialization, row scans, uniqueness and region processing before DP.

Proposed public result is `Result<MergeOutcome, MergeError<E>>`, with distinct
`Load(E)` and `Resource(ResourceLimit)` variants. A refusal returns no partial
outcome and is never a semantic conflict. Fixed published limits cover manifest
entries, individual and aggregate path bytes, individual and aggregate loaded
bytes, per-version line count, peak table cells and aggregate work. Successful
outcomes expose consumed work for evidence. No wall-clock deadline determines
domain behavior. Independent Codex architecture review is pending.

The engine can reject a returned oversized blob before scanning it or loading the
next blob, but cannot constrain the loader's earlier allocation, IO, CPU or error
behavior. The same applies to the caller's prior manifest allocation. Later native
bounded object APIs must enforce repository-read limits before allocation. Callers
must also bound concurrent calls and must not persist partial results on refusal.

## Evidence and limits

Verification is pending. General correctness beyond the small exhaustive
every-pair oracle is INCONCLUSIVE. This is not production Stack or a product gate.
