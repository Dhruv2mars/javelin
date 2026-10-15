# macOS mounted backend suitability

This unit measures the unchanged PR #11 feasibility backend. It does not authorize
native binding or accept a product gate. The backend source is `39a065b`, inherited
through docs-only `61aae6f`. Draft PR #20 targets `test/macos-sparse-backend`.

## Fixture design comparison

1. A Bun supervisor launches the inherited CLI and isolated Python ordinary-path
   workers. A small C helper calls FSEvents, kqueue and clonefile directly. Python
   handles held descriptors and bounded raw RPC calls under a disposable account.
   Each worker has a process group, deadline and raw output. This reuses the exact
   locked backend and needs no additional Rust dependency graph.
2. A separate Rust consumer invokes the public overlay crate and NFS listener,
   with Bun orchestration. It could expose counters directly but introduces a new
   server executable and an additional lockfile. Its instrumentation could differ
   from the inherited CLI and conceal lifecycle defects.

Choose approach 1. Use the inherited server executable unchanged. Measure upper
logical and allocated bytes plus process I/O counters where the host exposes them.
Observed allocated bytes do not by themselves measure transient copied bytes.
A bounded raw RPC observer records READDIRPLUS cookies separately from ordinary
mounted traversal. Neither replaces the other.

The hosted job has a 20-minute limit. Build and probes run within a 14-minute
supervisor budget. Commands have individual deadlines and process-group TERM/KILL
termination. Cleanup runs before sealing evidence and is retried by a workflow trap.
No mount, account or host security operation runs on the developer laptop.

Fixtures start with 32 then 512 and 2,048 tiny C source files. The configured ceiling
is 50,000 entries and 1 GiB file contents per active fixture. The 1 GiB truncate
fixture is a separate tier after small cases, so its bytes do not overlap the small
fixture. Larger tiers stop when time or byte budgets expire and remain INCONCLUSIVE.
Ordinary APFS and direct clonefile controls are controls only. They are not Layer proof.

## Evidence status

Planning only. No hosted result exists yet. Restart identity, alias whiteouts,
paginated mutation, resource costs, FSEvents and cross-user isolation remain unverified.
The current backend does not persist its in-memory generation or handle IDs.
The prior audit found source-level gaps in these areas. This unit records actual
behavior and preserves failures rather than changing the backend.
