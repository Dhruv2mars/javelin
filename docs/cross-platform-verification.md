# Native cross-platform verification

This gate checks the committed native Rust source on GitHub-hosted runners.
[Draft PR #6](https://github.com/Dhruv2mars/javelin/pull/6) targets
`feat/native-repository-foundation`. It depends on that branch landing and cannot
be accepted independently into `main`. PRs #1 and #2 are not dependencies.

## Gate configuration

The [workflow](../.github/workflows/native.yml) runs on pull requests and pushes
to `main` or `feat/native-repository-foundation`. Pull requests check out the exact
head commit, not GitHub's temporary merge commit. The environment log records
the checked-out SHA, runner image version, architecture, and compiler versions.

| Runner label | Expected architecture | Scope |
| --- | --- | --- |
| `ubuntu-24.04` | x64 | Linux native build and tests |
| `macos-15` | arm64 | macOS native build and tests |
| `windows-2025` | x64 | Windows native build and tests |

Rust `1.99.0` is pinned. Rustup installs the minimal profile with Rustfmt and Clippy.
Checkout `v7.0.1` and upload-artifact `v7.0.2` are pinned to release commit SHAs.
Both action manifests use Node 24. The three independent jobs have 20-minute
timeouts, keep running if another OS fails, and cancel obsolete runs for the same
ref. No cache, deployment, release upload, or additional credentials are configured.
The workflow token has only `contents: read`, and checkout does not retain it.

Each runner executes these commands with Bash's Actions-provided `-e` and
`-o pipefail`, so piping output through `tee` preserves command failures:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features -- --nocapture
cargo build --locked --release --all-features
```

Cargo uses the committed `Cargo.lock` and rejects dependency resolution changes.
The release step builds the root helper and CLI workspace binary. Default workspace
members include the root library and `crates/javelin-cli`; tests and lint include
both packages’ targets and features. This gate does not run filesystem probes
outside the root package. Workflow checkout is developer setup. Javelin runtime and tests do not
invoke or interpret Git.

## Evidence and limits

### Reviewed core follow-on

The hub sent `native-core-complete c78ea7a5d3eb9555625391e2f7346859d70f4c2a`
after independently verifying corrected Gate 1 and its follow-up review. This same
PR is rebased onto that exact reviewed core. The diff from that commit contains
only `native.yml`, this document, and the appended CI progress section in TASKS.
Source, tests, Cargo manifest and lockfile are unchanged.

**VERIFIED: reviewed core's compiled native suite on all three hosted OSes.**
[Run 38462957668](https://github.com/Dhruv2mars/javelin/actions/runs/38462957668)
checked out exact PR head `bfd2a51d75430a419272549b0e209bda6badf9d1`, whose
source and tests match reviewed core `c78ea7a5d3eb9555625391e2f7346859d70f4c2a`.
Formatting, warnings-denied Clippy, native tests, release build, and log upload
all passed on each runner. The unrelated Macroscope check was skipped and is
not native verification evidence.

| Job proof | Passing / ignored | Process / CLI / repository passing | Retained log artifact |
| --- | --- | --- | --- |
| [Linux x64](https://github.com/Dhruv2mars/javelin/actions/runs/38462957668/job/115522567286) | 26 / 1 | 8 / 4 / 14 | [11846411948](https://github.com/Dhruv2mars/javelin/actions/runs/38462957668/artifacts/11846411948) |
| [macOS arm64](https://github.com/Dhruv2mars/javelin/actions/runs/38462957668/job/115522567157) | 26 / 1 | 8 / 4 / 14 | [11846911837](https://github.com/Dhruv2mars/javelin/actions/runs/38462957668/artifacts/11846911837) |
| [Windows x64](https://github.com/Dhruv2mars/javelin/actions/runs/38462957668/job/115522567354) | 24 / 1 | 8 / 3 / 13 | [11846616918](https://github.com/Dhruv2mars/javelin/actions/runs/38462957668/artifacts/11846616918) |

Downloaded and inspected all three artifacts on 2026-10-14 local time. Each
contains the six expected log files and confirms the exact checked-out SHA,
Rust `1.99.0`, and successful release compilation. The run completed on
2026-10-13 UTC; artifacts expire on 2026-10-27 UTC. Observed hosts and image
versions match the historical scaffold table below.

The missing-tail retained-anchor regression, interrupted empty initialization,
incidental-file preservation, binary object persistence, all eight concurrency
and process recovery tests, and compiled CLI tests passed on every OS.
The suite's one ignored test is the child-process entry point exercised by the
parent process tests, not an omitted verification case.

Two tests use `cfg(unix)`: storage symlink rejection and the explicit-project-
directory CLI regression from a deleted cwd. Windows excludes both. Windows
coverage of those two behaviors is **NOT VERIFIED**, and 26 passing tests on
Windows cannot be claimed. Reviewed source and validation were not changed.

Local macOS arm64 preflight passed with pinned Rust `1.99.0`: formatting,
warnings-denied Clippy, 26 passing tests with one ignored process entry point,
and the release build. These checks do not replace hosted execution evidence.

The hub's accepted core limits remain: integrity inspection checks data rather
than authenticating schema definitions; consistent-tail deletion without a
retained operation anchor is undetectable; non-Unix parent-directory sync is
unimplemented. Actual hardware power-loss durability and performance remain
unverified. Hosted native tests do not prove sparse Layer or GA behavior.

### Historical scaffold setup

**VERIFIED: hosted scaffold CI setup only.** The starting product baseline is
`7ec2e75`, whose only integration test asserts that `javelin init` prints
`Hello, world!`. The successful baseline run proves only that the CI setup executes
the listed checks on these runner images. YAML validation and local checks cannot
prove hosted execution.

[Run 38427608862](https://github.com/Dhruv2mars/javelin/actions/runs/38427608862)
completed successfully on 2026-10-13 at checked-out commit
`fca6f758abec55f92f104d0fb0d9a42fd7b0278f`, containing the unchanged `7ec2e75`
scaffold plus this workstream's planning, workflow, and documentation. Every
formatting, Clippy, test, release-build, and log-upload step succeeded on each OS.

| Job proof | Observed Rust host | Runner image version | Retained log artifact |
| --- | --- | --- | --- |
| [Linux](https://github.com/Dhruv2mars/javelin/actions/runs/38427608862/job/115401005999) | `x86_64-unknown-linux-gnu` | `20261004.327.1` | [11829576272](https://github.com/Dhruv2mars/javelin/actions/runs/38427608862/artifacts/11829576272) |
| [macOS](https://github.com/Dhruv2mars/javelin/actions/runs/38427608862/job/115401005443) | `aarch64-apple-darwin` | `20260907.0337.1` | [11829406723](https://github.com/Dhruv2mars/javelin/actions/runs/38427608862/artifacts/11829406723) |
| [Windows](https://github.com/Dhruv2mars/javelin/actions/runs/38427608862/job/115401006060) | `x86_64-pc-windows-msvc` | `20260925.250.1` | [11829127289](https://github.com/Dhruv2mars/javelin/actions/runs/38427608862/artifacts/11829127289) |

Downloaded and inspected all three artifacts. Each contains all six expected
log files, the exact checked-out commit, Rust `1.99.0`, and the passing scaffold
test. Artifacts expire on 2026-10-27. No runner scheduling, toolchain, action
runtime, or artifact permission error occurred. No failing cloud check was
deliberately injected, so upload after an actual failure remains unconfirmed.

Local macOS arm64 checks passed with pinned Rust `1.99.0`: formatting, Clippy,
the single scaffold integration test, and the release build. Bun parsed the YAML.
These are preflight checks only, not evidence for any hosted runner.

Each job uploads `native-logs-<runner>-<attempt>` after success or failure, unless
cancelled, retaining plain-text environment, toolchain, formatting, lint, test,
and build logs for 14 days. A failed command stops subsequent checks in its job.
Checkout failures appear in GitHub's job log because no local check logs exist
yet. Timeout, cancellation, or infrastructure failures can prevent artifact upload.
Download artifacts before expiry if evidence must be kept longer. GitHub job logs
also show step conclusions and action errors under the repository retention policy.

On 2026-10-13, the repository API reported public visibility, Actions enabled,
and actions allowed. Both the availability read and actual run completed without
an Actions permission error.

Native immutable storage, World ordering and recovery are **NOT VERIFIED** by
the scaffold. The inspected reviewed-core run above supersedes that baseline
scope for its compiled tests. Native-owned files remain unchanged.
Sparse FSKit, OverlayFS, and ProjFS behavior is **NOT VERIFIED** by this gate.
The current hub contract at `8db2a5b` requires ordinary filesystem operations,
tool use, isolation, and recovery on a sparse backend. Full-copy controls do not
pass that requirement. Installation, upgrades, release architectures, complete
workflows, and GA support on all three platforms remain **NOT VERIFIED**.
These GitHub Actions runs verify Javelin development checks. Javelin's product CI
execution and accepted workflow syntax remain **NOT VERIFIED** by this work.

## Primary sources

Checked on 2026-10-13:

- [GitHub-hosted runner labels and architectures](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
- [Checkout v7.0.1 release](https://github.com/actions/checkout/releases/tag/v7.0.1) and [action runtime manifest](https://github.com/actions/checkout/blob/v7.0.1/action.yml).
- [Upload-artifact v7.0.2 release](https://github.com/actions/upload-artifact/releases/tag/v7.0.2) and [artifact retention](https://github.com/actions/upload-artifact#retention-period).
- [GitHub shell exit behavior](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#jobsjob_idstepsshell).
- [Rust stable distribution manifest](https://static.rust-lang.org/dist/channel-rust-stable.toml), dated 2026-10-01, reports Rust `1.99.0`.
- [Rustup profiles](https://rust-lang.github.io/rustup/concepts/profiles.html) and [components](https://rust-lang.github.io/rustup/concepts/components.html).
- [Cargo test and locked resolution](https://doc.rust-lang.org/cargo/commands/cargo-test.html), [Cargo release builds](https://doc.rust-lang.org/cargo/commands/cargo-build.html), and [Clippy usage](https://doc.rust-lang.org/clippy/usage.html).
