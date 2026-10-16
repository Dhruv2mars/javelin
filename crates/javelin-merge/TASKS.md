# Bounded merge integration checklist

Done means `origin/main` is merged into the existing PR #21 branch, the bounded
engine and tests are preserved, root `TASKS.md` matches main byte-for-byte, and
the integrated component passes a locked build and tests before push.

- [x] Merge main at `df60ab31076f9ffcc447220cf26d7b89fe0755f1` and resolve the root checklist conflict using main.
- [x] Keep this unit's checklist scoped to its crate.
- [x] Verify engine/test hashes and inspect lockfile integration.
- [x] Run diff checks and the locked component build/tests against current native APIs.
- [x] Commit and push the existing PR branch, then report to the hub for merge.

Current-main integration includes `dd4c9155f10cc589549c00832168a649542e51c1` after
finishing the saved `df60ab3` merge. Engine/test hashes are unchanged; the only
component lock integration is the native crate libc dependency. The locked
all-targets test command built the current native dependency and passed all 35
component tests. The PR diff check and exact-main root checklist comparison pass.
