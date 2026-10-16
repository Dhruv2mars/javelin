## Filesystem verification workstream

Owned by `test/layer-filesystem-contract`, based on `chore/ga-build-plan`.
Done means a committed bounded probe that uses ordinary paths and external tools,
reports per-capability verdicts, and records host prerequisites and a provisional
next-step recommendation. A production backend recommendation remains unconfirmed
until a real sparse Layer sharing immutable base content passes Gate 2. Directory
and clonefile controls verify the probe only. This PR may finish with a VERIFIED
probe and an INCONCLUSIVE sparse-backend result.
Only this workstream's checklist is updated here. Global gates remain unverified.

- [x] Read build instructions and define the self-verifiable outcome.
- [x] Open exactly one draft PR and register it with T3 Code: [#5](https://github.com/Dhruv2mars/javelin/pull/5).
- [x] Discover host filesystem, FSKit, SDK, signing, and installation prerequisites. FSClient executes; Xcode build exits 69 for unaccepted license; zero valid signing identities. No Layer mounted.
- [x] Build a backend-agnostic behavior probe and explicit directory/clonefile controls.
- [x] Exercise bytes, mutations, traversal, symlinks, watches, compiler access, isolation, and process interruption. Both controls pass eleven checks on this host.
- [x] Verify the probe catches a broken isolation control and records unsupported capabilities. Hardlink leakage fails; caller-prepared paths report clone provisioning INCONCLUSIVE.
- [x] Verify review repairs for canonical output paths, fresh watchers, clone write readback, and wall-time bounds. Hub independently reproduced controls and regressions at `0f15db2`.
- [x] Use a unique verifier output directory by default, per hub concurrency review, and rerun the final verifier. Final executable source `bce352e`; omitted-output run exits 0 into unique `javelin-probe-evidence-6sxr25`.
- [x] Repair final review findings and exercise hard-link and FIFO report rejection plus actual external-tool subprocess termination on wall expiry. Directory and clonefile still pass eleven checks.
- [x] Commit reproducible host evidence, official platform sources, gaps, and the smallest next step. `docs/workspace-verification.md` and `tools/workspace-probe/evidence/macos-arm64/` preserve actual results. Production recommendation and real sparse Gate 2 remain INCONCLUSIVE.
- [x] Report final evidence and verdict to the hub, leaving PR #5 draft and unmerged. Final source `bce352ec4e5f78eb98c71facba68b872d3e11118`; probe VERIFIED, sparse backend and Gate 2 INCONCLUSIVE. Stable completion message `filesystem-pr5-completion-evidence-v1`.

## SDK header discovery correction

Done means missing or unreadable FSKit SDK headers preserve the collected host report with INCONCLUSIVE checks. No install or mount.

- [x] Read Greptile discussion4292225951 and confirm the unconditional header reads.
- [ ] Reproduce lost evidence with a focused regression, then guard each header read.
- [ ] Verify saved reports for missing and unreadable headers, preserve current main root checklist, and push existing regular PR #5.
