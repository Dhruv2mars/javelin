# Benchmarks

Measured results belong here only after execution against the packaged binary. Every entry records hardware, OS, filesystem, fixture generator, command, p50/p95/p99, peak memory when available, object integrity result, and materialization backend.

Required fixtures are 1,000 files/50 MB, 25,000 files/1 GB, 100,000 files/5 GB, 100 open Layers with four changed files each, 10,000 events, and 1 GB cumulative provenance attachments.

Current measured evidence:

- The source-build 100-Layer concurrent checkpoint/Refresh/Publish fixture completed without corruption or lost accepted changes on Apple Silicon macOS. This is pre-release evidence; packaged-binary percentiles will replace it during the final gauntlet.

Javelin does not claim constant-time Layer creation. Ordinary directory materialization scales with path count even when APFS clone-on-write shares physical file data.
