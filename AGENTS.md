# Javelin

Javelin replaces Git and GitHub for the agentic era. GA means developers use it instead of
Git and GitHub for their daily work. Core model: an immutable World of versions, isolated
Layers that agents edit as plain cloned directories, and an atomic Publish onto the World.
Start with `docs/architecture.md`; read `docs/build-contract.md` before changing product behavior.

## Repo map

- `src/`: the `javelin` library (repository, World, Layers, capture, Stack, Publish).
- `crates/`: `javelin-cli`, `-collaboration`, `-merge`, `-protocol`, `-stack`.
- `tests/`: integration tests. `tools/`: benchmarks, probes, native packaging.

## Checks

CI runs these on macOS, Linux and Windows, with a failing `git` first on `PATH`:

    cargo fmt --all -- --check
    cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
    cargo test --locked --workspace --all-targets --all-features

## Rules

- Javelin development may use Git. Javelin runtime, storage, tests and user workflows must
  never invoke or interpret Git.
- Preserve accepted behavior, not old code.
- One task per branch, worktree and owning thread. Implement, run checks, open a regular PR
  (no drafts or planning PRs), link it in T3 Code, report briefly, finish. No routine extra
  review, audit or handoff rounds.
- Small commits prefixed `feat:`, `fix:`, `test:`, `chore:` or `refactor:`.
- Never push to another thread's branch or edit another thread's worktree.
- The hub thread coordinates and owns `TASKS.md`. Update `TASKS.md` in your own worktree;
  the hub consolidates.
- The hub may merge a PR once all its checks pass. Merge with `--merge`.
- An explicit user pause overrides autonomy and scheduled continuations until the user resumes.
- Reuse existing T3 threads; settle finished ones.
