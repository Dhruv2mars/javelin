# ADR 0002: Content-addressed trees

Status: accepted

Javelin stores immutable BLAKE3-addressed blobs and deterministic trees. Trees encode kind, executable bit, name, object identity, and symlink target. zstd-compressed object files use atomic writes.

Writers determine identity before allocating durable storage. Existing and same-scan duplicate objects require no temporary write. A scan group-commits new objects: stage and compress, synchronize files concurrently, rename atomically, synchronize each touched shard once, then synchronize the object root when new shards were created. The database may reference a Tree only after its object batch is durable.

This preserves the original crash-ordering guarantee while removing serialized file and directory flushes from the per-path scan loop. SQLite remains the canonical metadata store; measured scan latency did not justify a database replacement.
