# Limitations

Javelin v1.0.0 intentionally does not provide remote repositories, hosting, network synchronization, multi-user authentication, Git import/export or compatibility, agent orchestration, model APIs, task planning, deployment, hostile-code isolation, semantic merging, CRDT editing, a custom database, a virtual Layer filesystem, package management, or distributed consensus.

Current technical limits:

- Verified release support is Apple Silicon macOS only until native CI artifacts pass elsewhere.
- The Monitor uses debounced polling plus correctness scans, not an OS event API. Status and Publish can scale with tracked path count.
- Layer creation materializes an ordinary directory. Native APFS clone semantics reduce physical copying, but creation is not constant time.
- Linux uses streamed copy in v1. Windows paths are portable in source but remain unverified.
- Dead Publish waiter cleanup uses Unix process liveness. Other platforms rely on timeout and restart recovery until native validation lands.
- Bounded automatic text integration handles UTF-8 regular files up to 1 MiB. Other divergent content becomes an explicit Conflict.
- Non-UTF-8 paths are rejected.
- Tracked portable metadata excludes timestamps, ownership, ACLs, extended attributes, devices, sockets, and process state.
- One Managed view is one writer isolation boundary. Concurrent writers inside it are not isolated from each other.
- Repair reconstructs views and caches, not missing canonical objects or arbitrary SQLite corruption.
- There is no remote telemetry. Diagnostics remain local.

Performance claims appear only in [`benchmarks.md`](benchmarks.md) with measured fixture and hardware details.
