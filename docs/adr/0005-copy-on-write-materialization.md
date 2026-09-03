# ADR 0005: Copy-on-write materialization

Status: accepted

Javelin materializes immutable root caches, then clones files into Managed views with native copy-on-write where supported. Streamed copy is the correct fallback. Hardlinks are forbidden because in-place writes can cross Layer boundaries.

