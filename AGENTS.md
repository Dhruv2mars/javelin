# Development rules

- Define "done" as a self-verifiable outcome before starting.
- Keep solutions concise and simple.
- Use one branch per task. Open a draft pull request first.
- Use small commits with `feat:`, `fix:`, `test:`, `chore:`, or `refactor:` prefixes.
- Use `bun` by default unless an existing lockfile or project toolchain requires another tool.
- Javelin development may use Git. Javelin runtime, storage, tests, and user workflows must never invoke or interpret Git.
- Never merge a pull request without explicit user instruction.

