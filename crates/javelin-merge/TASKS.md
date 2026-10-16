# Bounded merge integration checklist

Done means `origin/main` is merged into the existing PR #21 branch, the bounded
engine and tests are preserved, root `TASKS.md` matches main byte-for-byte, and
the integrated component passes a locked build and tests before push.

- [x] Merge main at `df60ab31076f9ffcc447220cf26d7b89fe0755f1` and resolve the root checklist conflict using main.
- [x] Keep this unit's checklist scoped to its crate.
- [ ] Verify engine/test hashes and inspect lockfile integration.
- [ ] Run diff checks and the locked component build/tests against current native APIs.
- [ ] Commit and push the existing PR branch, then report to the hub for merge.
