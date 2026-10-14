# Native development packages

Status: planning. Nothing here is a public release, a GA claim, or Gate 5 evidence.

Packages are **development / pre-GA** builds of the native CLI at Cargo version
`0.1.0`, built from accepted source `e56fc86e3415217de2b960a7ea0dbe6d2eb450b1`
(unmerged dependency; this PR claims no independent acceptance). The CLI today
supports only `init`, `history`, and `check`. The full Layer CLI does not exist.

## Scope

Build, archive, checksum, install into a fresh path, and smoke the CLI on Linux,
macOS, and Windows. No npm, GitHub release, tag, signing, secrets, global install,
or user `PATH` change.

## Candidate shapes

| Shape | Description | Verdict |
| --- | --- | --- |
| A. Rust packager crate | `tools/native-packaging` builds, packages, and installs with one implementation per OS | Selected: one safe-extract and checksum implementation, existing toolchain, no new runtime |
| B. Shell and PowerShell scripts | Per-OS scripts around `tar`, `Compress-Archive`, `shasum`, `Get-FileHash` | Rejected: three implementations, weak traversal checks, nondeterministic archives |
| C. Bun scripts | One TypeScript implementation | Rejected: adds a toolchain the CLI build does not otherwise need on the runners |

A manual path (native `tar`/`unzip`/`Expand-Archive` plus checksum tool) is also
exercised by the download job so the archive is not only usable through our tool.

Details, evidence, and limits are added as the work lands.
