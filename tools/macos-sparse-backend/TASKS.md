# PR #11 comment fixes

Done means the reported attribute, clone publication and timestamp defects have
failing reproductions and passing regressions, with the bounded fixture's limits
retained. Root TASKS stays exactly as supplied by current main.

- [x] Read comments 4292227991, 4292228003 and 4292228021; merge current main.
- [x] Reproduce read-only CREATE and owner-held mode-000 time updates; fix attributes.
- [x] Reproduce an incomplete clone destination; protect publication and rollback.
- [x] Reproduce lost modification times; preserve copied file, directory and symlink times.
- [x] Run component checks: format, warnings-denied Clippy, 31 debug/release tests
  and release build. Keep current validation separate from old `39a065b` hosted
  evidence and PR #20 measurements. Resume preserves those passed checks.
- [x] Merge current main with root TASKS unchanged and push the same regular PR
  for hub integration. No new mounted or hosted claim.

- [x] Reproduce comment 4298776857 with a writable file beneath nested mode-0555
  directories; restore intended directory modes after copy-up success and failure.
- [x] Confirm comment 4292228021 still matches the source timestamp-preservation fix.
- [x] Run the focused regression and warnings-denied Clippy; push the same PR with
  current main integrated and root TASKS unchanged. No mounted proof repeated.
  Permission restoration after process kill remains an explicit limit.

- [x] Retain the completed reproductions for 4300860233/237/243 without repeating them.
- [x] Fix combined mode/size changes in both directions; refuse unreadable bases
  before serving without changing base modes or ACLs; encode new whiteouts with
  a version header and NUL separators while retaining legacy reads.
- [x] Pass 38 component tests, including the three new focused regressions.
- [x] Finish lint, runner syntax and CLI refusal checks; push the same regular PR
  with root TASKS unchanged and archived evidence preserved.

# Private clone staging (4300860228)

Done means clone staging is created atomically with mode 0700 and retains that
private root after publication, verified while copying is blocked and after
success under child-local umask 022 with 0755 directories and 0644 files.

- [x] Read AGENTS, RTK and the build contract; confirm the missing stage-root mode.
- [x] Reproduce exposure with the existing clone publication/rollback barrier:
  incomplete stage was 0755 instead of 0700 under child-local umask 022.
- [x] Create staging atomically as 0700 and verify private publication.
- [x] Pass the focused regression, 38 component tests (2 existing child-process
  helpers ignored), warnings-denied Clippy, formatting and whitespace checks.
- [x] Open/link one regular PR. No mounted, authorization, case/Unicode, restart,
  NFS suitability, Gate 2 or GA claim.
