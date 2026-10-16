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
