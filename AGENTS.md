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
`github` remote is a frozen archive: never push to it. Buildkite runs these on Linux for
every push (`.buildkite/pipeline.yml`, free `linux-small` queue) and reports to the trail as
a gate. The owner also runs them locally on macOS before handing off:

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
- The hub merges a trail once its Buildkite build is green, the local macOS checks pass on
  the merge result, and `entire trail merge --dry-run` passes: `entire trail approve`, then
  `entire trail merge`. Never `--force`. Builds: `bk api /pipelines/javelin/builds`.
- Windows has no runner, and Gate 2 and the workspace probes run nowhere automatically.
  Say so in every hand-off that touches platform behavior.
- An explicit user pause overrides autonomy and scheduled continuations until the user resumes.
- Reuse existing T3 threads; settle finished ones.

<!-- entire-agent:begin -->
Read .entire/agent-guide.md for this repository's workflow, source inspection, and verification guidance.
@.entire/agent-guide.md
<!-- entire-agent:end -->
