# Durable Layer Stack

Done: a bounded mechanical merge from a frozen source checkpoint creates exactly
one CAS-protected target checkpoint with durable immutable source ancestry;
conflicts/refusals leave the target unchanged and exact retries survive reopen.
Root TASKS.md remains exactly base/main. The hub owns integration and merging.

- [x] Read AGENTS/RTK, master checklist, build contract and dependency APIs.
- [x] Implement atomic native ancestry and exact Stack retry storage.
- [x] Implement thin bounded merge adapter without dependency cycles.
- [x] Verify merge/conflicts, other bases, resource refusal, CAS/retries, source lifetime.
- [x] Verify ancestry corruption, rollback/process boundaries, format refusal.
- [x] Run appropriate locked checks; document signatures and format limit.
- [x] Update to current main and verify compatibility with the merged CLI.

Delivery: one regular PR, linked by the owning T3 thread; the hub merges it.

Initial local macOS validation: 109 native tests passed (one ignored process actor),
5 adapter tests and 35 unchanged merge-engine tests passed. Six newly merged
CLI tests also passed. Formatting, warnings-denied Clippy for native and adapter,
and release builds passed. Retained logs: `/tmp/javelin-native-stack-checks/`.
Hosted platform checks remain for CI; no mounted/runtime/CLI behavior is added.


## PR26 concrete findings

- [x] Reproduce missing repeated-source Stack binding and repeated ancestry hashing.
- [x] Require Stack records from the attempt itself, including stale attempts.
- [x] Reuse validated checkpoints within transaction-scoped Layer/integrity walks.
- [x] Prove fresh transactions revalidate graphs; run focused/full compatible checks.

Both regressions failed before the fixes. The shared-history fixture previously
loaded one checkpoint 100 times; now all 18 checkpoints load once per integrity
transaction, with revalidation and corruption refusal in later transactions.
After merging main `8b293ee`, 117 workspace tests passed (one ignored actor),
including all process/corruption and extracted CLI tests; all 5 adapter tests,
format checks, warnings-denied workspace/adapter Clippy, and release builds pass.
Retained logs: `/tmp/javelin-native-stack-checks/pr26-fixes/` (plus `repro-*.log`
in the parent directory). Root TASKS/Cargo files match main exactly.
