# PR16 review fixes

Done means the four reported defects are reproduced, fixed with focused regressions,
checked and pushed in the same PR. Root TASKS stays exactly main. Original raw
measurements and their source labels remain unchanged.

- [x] Read comments 4292228767, 4292228774, 4292228778 and 4292228787.
- [x] Merge current main and keep root TASKS exactly main.
- [x] Reproduce and reject missing ingest passes/indices and history suffixes after summary recomputation.
- [x] Reproduce and propagate every replay digest read failure.
- [x] Reproduce and fix mountpoint selection, including escaped paths.
- [x] Correct retained-source documentation after main integration.
- [x] Run formatting, warnings-denied clippy, debug/release tests (13 unit + 22 CLI each) and all 17 retained verifiers; push the same PR16.
