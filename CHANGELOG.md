# Changelog

## 0.0.1 - 2026-09-08

- Added immutable BLAKE3 and zstd object storage backed by bundled SQLite metadata.
- Added World Versions, Local Layer, named Private Layers, nested Layers, automatic and explicit Checkpoints, Refresh, Publish, Discard, recovery, and append-only restore.
- Added deterministic path-state integration, bounded text integration, stored Conflicts, isolated World Rule execution, idempotent Publish, and FIFO target admission.
- Added versioned JSON and JSONL output, events, passive Claims, provenance sessions, attachments, redaction, search, purge, and path explanation.
- Added `fsck`, `repair`, `doctor`, retention-aware `gc`, fault injection, installed-binary acceptance, and 100-Layer stress coverage.
- Added APFS copy-on-write materialization from immutable root caches with streamed copy fallback.
- Added hash-first object deduplication and durable scan batches, reducing measured 100,000-file initialization from 1,254.604 seconds to 61.243 seconds on the benchmark host.

- Added the `javelin-cli` npm package with bundled macOS arm64, Linux x64, and Windows x64 binaries and clean-install verification.
- Keep live Publish waiters queued while the queue advances, with a five-minute timeout for stalled queues.
