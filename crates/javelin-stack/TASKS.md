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

Verified on local macOS: 109 native tests passed (one ignored process actor),
5 adapter tests and 35 unchanged merge-engine tests passed. Six newly merged
CLI tests also passed. Formatting, warnings-denied Clippy for native and adapter,
and release builds passed. Retained logs: `/tmp/javelin-native-stack-checks/`.
Hosted platform checks remain for CI; no mounted/runtime/CLI behavior is added.
