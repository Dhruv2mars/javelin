# PR22 scoped checklist

- [x] Preserve preregistration b493add and completed Codex design feedback.
- [x] Implement only owned tool/doc/workflow paths; native e56 unchanged.
- [x] Pass Clippy, 4 Rust release tests, 28 Bun verifier/monitor tests.
- [x] Preserve incomplete Mac and Ubuntu be39689 journal-race evidence.
- [x] Preserve and exclude stopped, incorrectly labelled and interrupted partial runs.
- [x] Verify completed Mac db992f0-controls-v2, 180 children and 231 observations.
- [x] Verify Mac profile pass, same binary digest as controls. CPU sample stacks observable; heap ownership/peak attribution inconclusive.
- [x] Inspect Ubuntu run38903121501 at db992f0, job116979996185, artifact12046239153. Both controls/profile evidence verify with fixed verifier c75324b. Original job failed only at old empty-output reporting bug; Linux CPU/heap attribution inconclusive.
- [x] Read actual results and keep existing PR22 regular for hub integration.

Resume on October 16 found no live native-profiles processes. No completed
measurement was repeated. Root TASKS remains untouched in this continuation.
Measured producer source remains db992f0542b877a2635ed358ff09627656e166a8.
Verifier-only corrections do not relabel these measurements.

Reporting correction: reproduced empty stdout from a failed profiler using a
separate copy of actual Mac profile data. The fixed verifier classifies it
INCONCLUSIVE; original raw bytes remain unchanged. Workflow now runs explicitly
to avoid repeating completed measurements for verifier-only changes.

## Concrete PR review corrections

- [x] Confirm no live benchmark process; reuse completed Mac/Ubuntu raw evidence.
- [x] Reproduce copied metadata tamper acceptance on a separate retained-data copy.
- [x] Bind saved build metadata to the single raw metadata event.
- [x] Add successful Mac/Linux, missing/blocked profiler and loop/metadata tamper regressions; all 39 tests pass.
- [x] Reverify all retained successful data and the previously accepted tamper; scoped correction is ready to push.

Root TASKS is restored byte-for-byte from current origin/main in this correction.
The existing PR16 base remains unchanged.

Root TASKS snapshot: dd4c9155f10cc589549c00832168a649542e51c1. The producer and measured source labels stay
db992f0. Completion uses a brief hub report and watcher release, without another
measurement or review round.
