# macOS sparse Layer backend attempt

Status: planning. No sparse macOS backend is verified by this document.

## Source and dependencies

- Starts from hub commit `c5c0ebec93cb80308043192628ed452ff6f5d66d` on `chore/ga-build-plan`.
- PR #5 (`tools/workspace-probe/`) verified only directory and clonefile controls on
  macOS. Those controls validate a probe and can never count as sparse proof.
- PR #5 and PR #7 are unmerged dependencies. A pinned developer checkout of their
  source may be used in CI as setup. That is not acceptance of either PR.
- The PR #5 laptop record (macOS 26.6.2, SDK 27): the official FSKit sample build
  exited 69 because the Xcode license was unaccepted, and zero signing identities
  existed. This unit does not repeat laptop setup, accept licenses or change the host.

## Done

A reproducible hosted macOS attempt with committed source, recorded SDK/host,
license, signing and extension-activation prerequisites, raw job archive and cleanup
evidence, plus a source-backed comparison of at least two sparse ordinary-path
approaches. Result is VERIFIED only if a real sparse mount passes ordinary-path checks;
otherwise INCONCLUSIVE with the exact missing prerequisite.

## Scope

Owned: `tools/macos-sparse-backend/**`, this document,
`.github/workflows/macos-sparse-backend.yml`, and this unit's TASKS section.
Not claimed: CLI, native database binding, autosave, product Gate 2 or GA.
Product runtime and tests never invoke or interpret Git.
