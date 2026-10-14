# Development rules

- Define "done" as a self-verifiable outcome before starting.
- Keep solutions concise and simple.
- Do not overcomplicate or overengineer the solution.
- Use one branch per task. Open a draft pull request first.
- Use small commits with `feat:`, `fix:`, `test:`, `chore:`, or `refactor:` prefixes.
- Use `bun` by default unless an existing lockfile or project toolchain requires another tool.
- Javelin development may use Git. Javelin runtime, storage, tests, and user workflows must never invoke or interpret Git.
- Never merge a pull request without explicit user instruction.

## Autonomy

- Keep going when the next step does not need user input.
- Include status notes with the next action.
- Ask only when you cannot continue without the user.

## Long tasks

- Keep a checklist in `TASKS.md`. Check completed items and add newly discovered work.
- Split large audits and migrations across subagents. Verify their results against evidence before accepting them.
- Mark unconfirmed research claims and record where you looked.

## Build coordination

- Read `docs/build-contract.md` before changing product behavior.
- The Javelin GA build hub owns the master checklist and integration decisions.
- Keep independent, dependency-ready workstreams active in parallel with explicit owned paths.
- Use completion events and the existing hub continuation to commission the next ready bounded unit.
- Each separate implementation thread owns one PR and its own worktree.
- Update `TASKS.md` in your own worktree. The hub consolidates verified results.
- Do not push to another thread's branch or edit another thread's worktree.
- Register your PR with T3 Code immediately after creating it.
- Report completion, evidence, dependencies, and blockers to the hub.
- Earlier implementation is optional. Preserve the accepted behavior, not old code.

@RTK.md

