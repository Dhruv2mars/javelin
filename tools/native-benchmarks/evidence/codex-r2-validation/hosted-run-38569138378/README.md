# Hosted validation of the Codex r2 corrections

Owned workflow `Native storage baseline` succeeded in run
https://github.com/Dhruv2mars/javelin/actions/runs/38569138378 on exact head
`3acc6608199267ba97c950ec1793b1f20609d17f`. Its tool source matches the local tested
and Codex-reviewed `ebd30833974b08968271d3466eab0717773a5d00` source. Later retained
evidence does not change these measured-source labels.

Ubuntu 24.04 and macOS 15 each passed format, clippy, debug and release tests,
release build, three bounded `ci` runs, all verifier checks and artifact upload.
Each test profile passed 7 unit and 22 integration tests. The Windows 2025 job
only built and linted the benchmark, with no Windows measurement.

Downloaded artifacts `native-baseline-ubuntu-24.04-1` and
`native-baseline-macos-15-1` were independently verified locally. Each of the six
runs passes all 33 checks. Raw files were hashed before and after verification;
all hashes matched. Exact commands, budgets, machine/OS/CPU/memory/filesystem,
Rust/compiler and executable hashes are retained in each `run.json`.
`workflow-result.json` contains job and step source proofs;
`downloaded-verification.json` contains artifact IDs, per-run checks and provenance.
Artifacts expire 14 days after 2026-10-14; trimmed immutable raw files are committed.

Each run contains 2,616 raw records, 100 contended World operations and 80 blob puts,
with zero errors. Ubuntu accepted/stale outcomes were 54/46, 56/44 and 53/47;
macOS outcomes were 71/29, 61/39 and 62/38. These are successful tool validation
runs, not a new performance comparison or evidence of actual retry exhaustion.
Original retained runs and their four real setup busy/reopen/retry chains are unchanged.

All six runs name native source SHA-256
`590fc27e71a59c0018e36a3185b57d12dbc73e28959265b218147d72ef8453e6`
and tool source SHA-256
`1c43e507deacce6915fbd14d305bf703081c8f0689abfd52452de1eb2ff14e72`.
Native files, fixtures and root Cargo files remain unchanged from `e56fc86`.

The PR check rollup had six SUCCESS results and one SKIPPED Macroscope check.
The separate Native Rust workflow also reported success on all three OSes at the
same head, as recorded in `native-workflow-result.json`. It adds no Windows
benchmark evidence. The independent Codex review remains scoped to `ebd3083`.

All 17 committed raw run directories pass the corrected verifier, as listed in
`../all-retained-verification.json`. Along with the six prior downloaded copies
and two prior audit runs, 25 proof directories have been verified locally.
Tool validation is VERIFIED within this bounded public-API scope. Actual retry
exhaustion and latency-growth causation are NOT VERIFIED. Version-1 setup bytes
are inferred from the fixed source generator, not retroactively captured.
Windows measurement, stress, mounted-filesystem behavior, power loss, realistic
hot/many-repository workloads, Gate 6 and GA remain open. PR16 remains draft and unmerged.
