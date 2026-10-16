# Development rules

- Define "done" as a self-verifiable outcome before starting.
- Keep solutions concise and simple.
- Do not overcomplicate or overengineer the solution.
- Use one branch per task. Implement the change, run appropriate checks, then open a regular pull request. Do not create draft PRs or require a planning PR before implementation.
- Keep one flow: the owning thread implements, checks, opens and links its PR, reports the result briefly, and finishes. Do not add routine separate review, audit, acceptance or handoff rounds.
- Use small commits with `feat:`, `fix:`, `test:`, `chore:`, or `refactor:` prefixes.
- Use `bun` by default unless an existing lockfile or project toolchain requires another tool.
- Javelin development may use Git. Javelin runtime, storage, tests, and user workflows must never invoke or interpret Git.
- Never merge a pull request without explicit user instruction. The user authorized
  the hub on 2026-10-16 to merge ready PRs during this development marathon.

## Autonomy

- Keep going when the next step does not need user input.
- Include status notes with the next action.
- Ask only when you cannot continue without the user.

## Long tasks

- Keep a checklist in `TASKS.md`. Check completed items and add newly discovered work.
- Use subagents only when parallel work materially helps the task. Keep verification proportional to the change; no mandatory architect or independent review cycle.
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
- Keep T3 Code organized: reuse the existing task thread, create a new thread only for distinct work, and settle completed threads. Keep the central hub available for coordination.
- Report the result, relevant checks and any remaining blocker briefly. Do not require an evidence-only commit or formal handoff document.
- An explicit user pause overrides standing autonomy and scheduled continuation. Resume only on explicit user instruction.
- Earlier implementation is optional. Preserve the accepted behavior, not old code.

@RTK.md

