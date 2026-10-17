# macOS historical suitability tooling

This checklist belongs to PR #20. Root `TASKS.md` is preserved from main.
The original base was `61aae6f`; the PR now targets main after PR #11 merged.

Done means a bounded hosted attempt against historical backend `39a065b`, with
raw source, identity, fixture, measurement, failure and cleanup records. Tooling
readiness does not accept the backend or a product gate.

- [x] Reuse the same owner, worktree and PR after provider limits and the user pause.
- [x] Compare two designs; retain the completed Codex design report.
- [x] Build the inherited CLI without copying or modifying backend algorithms.
- [x] Run hosted source `1a1f315` as `38869992611`; preserve the raw archive and producer labels.
- [x] Inspect exact-source raw results and cleanup. Backend suitability is NOT VERIFIED.
- [x] Preserve failed setup run `38716043185` and incomplete archive run `38716401553`.
- [x] Follow the Oct 16 flow; no mandatory new architect or review round.
- [x] Mark PR #20 regular for verification tooling and report the historical failures.
- [x] Merge main after PR #11, preserving main's root checklist byte-for-byte.
- [x] Pin historical reproduction to a separate explicit developer checkout of `39a065b`.
- [x] Require successful pagination setup and an empty successful remaining scan.
- [x] Require explicit second-user denial; unrelated errors are inconclusive and protected bytes fail.
- [x] Bind cost verification to visible counts, content hashes, read output and compiler inputs.
- [x] Require both logs of every command even after inventory regeneration.
- [x] Recheck old evidence without changing it. Cost tiers narrow to INCONCLUSIVE because their visible inventories are absent.
- [x] Run focused offline regressions and syntax/diff checks without repeating mounted measurement.

Further backend repair and measurement are separate work. This tooling does not
claim that old failures describe main's permission and whiteout fixes.
