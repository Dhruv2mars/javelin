# Ignore-aware Linux discovery

Done: checkpoint/watch exclude ignored untracked paths; current checkpoint entries
remain tracked through edits, modes, symlinks and deletion; Publish retains ignored
private bytes read-only; rebind refuses rather than losing them. Frozen validation
and recovery remain intact. Local checks and disposable hosted Linux proof pass,
then one regular PR is opened and linked for hub integration.

- [x] Read AGENTS, RTK and build contract; keep root TASKS identical to main.
- [x] Implement bounded literal root-relative rules and current-head discovery.
- [x] Add matcher/capture and hosted watch race/dedup, preservation, Publish/rebind regressions.
- [x] Document syntax and runtime behavior.
- [ ] Run fmt, Clippy, tests, release and hosted mount/binary proof with Git control.
- [ ] Open and immediately T3-link one regular PR; report results without merging.
