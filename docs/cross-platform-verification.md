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
The release step builds the root binary. Tests and lint include all current Cargo
targets and features. This gate does not run filesystem probes outside the root
package. Workflow checkout is developer setup. Javelin runtime and tests do not
invoke or interpret Git.

## Evidence and limits

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
the scaffold. The rerun trigger is the hub's `native-core-complete <SHA>` message
after it independently verifies native PR #4 Gate 1 at that named final commit.
Rebase this same PR onto that SHA and record fresh three-OS checks against that
exact reviewed core. Do not rewrite or change the native owner's baseline files.
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
