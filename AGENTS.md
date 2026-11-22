# Javelin

Javelin is third-generation version control for agentic development: the `javelin` CLI
replaces Git, and the javelin.codes forge replaces GitHub. Repositories live on nodes that run
on infrastructure the owner connects; Javelin runs no managed hosting. GA means developers use
it instead of Git and GitHub for their daily work. Core model: an immutable World of versions, isolated
Layers that agents edit as plain cloned directories, and an atomic Publish onto the World.
Start with `docs/direction.md` and `docs/architecture.md`; read `docs/build-contract.md` before
changing product behavior.

## Repo map

- `src/`: the `javelin` library (repository, World, Layers, capture, Stack, Publish).
- `crates/`: `javelin-cli`, `-collaboration`, `-merge`, `-protocol`, `-stack`.
- `tests/`: integration tests. `tools/`: benchmarks, probes, native packaging.

## Checks

The repository lives on Entire (`origin` is `entire://.../et/dhruv2mars/javelin`). The
`github` remote is a one-way, read-only mirror for CI: never push, merge or edit there.
Enrolled clones push with `git ship` (push to Entire, then `git mirror`). Every mirrored
branch push (except `archive/**`) runs GitHub Actions `CI` (`.github/workflows/ci.yml`:
Linux, macOS and Windows, the forge, the node and the workspace probes); `report.yml` posts
it to the trail as the `github-actions` check, which the trail gate requires. Buildkite also
runs the Linux subset (`.buildkite/pipeline.yml`, free `linux-small` queue). The owner also
runs these locally on macOS before handing off:

    cargo fmt --all -- --check
    cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
    cargo test --locked --workspace --all-targets --all-features

## Rules

- Javelin development may use Git. Javelin runtime, storage, tests and user workflows must
  never invoke or interpret Git.
- Preserve accepted behavior, not old code.
- One task per branch, worktree and owning thread. Implement, run checks, push the branch,
  open an Entire trail for it (`entire trail create`), get it through a Sol review, report
  the trail URL briefly, finish. No drafts, planning trails or routine extra rounds.
- Small commits prefixed `feat:`, `fix:`, `test:`, `chore:` or `refactor:`.
- Never push to another thread's branch or edit another thread's worktree.
- The hub thread coordinates and owns `TASKS.md`. Update `TASKS.md` in your own worktree;
  the hub consolidates.
- The hub merges a trail once its `github-actions` check is green, the local macOS checks
  pass on the merge result, and `entire trail merge --dry-run` passes: `entire trail approve`,
  then `entire trail merge`, then `git mirror`. Never `--force`. Runs: `gh run list -R
  Dhruv2mars/javelin`; Buildkite builds: `bk api /pipelines/javelin/builds`.
- Gate 2 runs on GitHub only after landings on main that touch its paths, or on demand
  (`gh workflow run "Gate 2" -R Dhruv2mars/javelin --ref <branch>`). Say whether it ran in
  every hand-off that touches platform behavior.
- An explicit user pause overrides autonomy and scheduled continuations until the user resumes.
- Reuse existing T3 threads; settle finished ones.

<!-- entire-agent:begin -->
Read .entire/agent-guide.md for this repository's workflow, source inspection, and verification guidance.
@.entire/agent-guide.md
<!-- entire-agent:end -->
