# ADR 0002: Content-addressed trees

Status: accepted

Javelin stores immutable BLAKE3-addressed blobs and deterministic trees. Trees encode kind, executable bit, name, object identity, and symlink target. zstd-compressed object files use atomic writes.

