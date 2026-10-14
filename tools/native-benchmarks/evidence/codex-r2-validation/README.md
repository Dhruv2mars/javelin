# Codex r2 validation

Tested and reviewed candidate source is `ebd30833974b08968271d3466eab0717773a5d00`.
It contains fixes at `f4166ab` and corrected documentation, on native source
`e56fc86e3415217de2b960a7ea0dbe6d2eb450b1`. Native source and fixture bytes are unchanged.
These files are validation evidence, not a new performance comparison.

Commands ran locally with `rtk proxy`, from `tools/native-benchmarks`:

```
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo test --locked --release
cargo build --locked --release
```

All passed. Debug and release each passed 7 unit and 22 integration tests.
The unit regressions recompute summaries after modifying copies of retained raw data,
so rejection cannot depend on a stale summary. Synthetic injected chains exercise
all five retried worker actions and exhausted worker-to-parent resolution. These
injected rows are tests, not measured errors.

`verification-results.json` lists 18 original proof directories rechecked by this
release executable with `native-bench verify DIR`. All passed 33 checks. Ten are
committed runs, six are downloaded copies, and two are the hub's fresh/recovered
local audit runs. SHA-256 checks of every file before and after verify matched.
Original raw files, summaries and source labels are unchanged.

`tamper-results.json` records reproductions using the unchanged `4149da9` release
executable retained in `/tmp/javelin-hub-audit-pr16-codex-r2/src`, followed by this
new release verifier. Reproduction copies are outside the hub's read-only audit
scratch, in `/tmp/javelin-pr16-codex-fixes/reproduction`.
The four previously accepted tamper cases now exit 1. Latency tampering still exits 1.
Failure names bind setup retry gaps, worker/PID reopens and nonempty/nonshrinking
disk evidence. The missing-directory old probe returned `logical=0 files=0
physical=Some(0)`; the new probe returns an error. Unit tests also exercise a
non-directory root, a non-regular entry and an unreadable child directory.
The unchanged release CLI test failed on `debug_build_allowed=false`; both test
profiles now pass with the actual build profile.

Version 2 records explicit setup inputs and parent resolution reopens. Version 1
legacy setup bytes remain inferred from the original fixed source generator and
worker/index/seed; this is not retrospective byte-level input capture.

Fresh local raw evidence, exact machine/source provenance and its command are in
`fresh-ci-ebd3083/`. Hosted validation and independent review remain pending here.
Windows measurement, stress, profiling of latency growth, mounted filesystem,
power loss, realistic hot/many-repository workloads, Gate 6 and GA are NOT VERIFIED.
