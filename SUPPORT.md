# Support

Javelin is pre-release. There is no GA binary, package, installer or upgrade path yet.
The supported way to run it today is to build from source as described in the
[quickstart](docs/quickstart.md).

## Platforms

The GA contract requires macOS, Linux and Windows
([build contract](docs/build-contract.md), item 5). Today:

| Platform | Status |
| --- | --- |
| macOS (APFS) | Built and tested in CI; clone-backed Layer tests run here. |
| Linux (ext4, XFS, btrfs) | Built and tested in CI; clone-backed Layer tests run here. |
| Windows (NTFS, ReFS) | Built and tested in CI. NTFS and ReFS do not yet pass every gate 2 check: a same-size rewrite that restores its modification time is not captured. |

CI runs on x86_64 Linux and Windows runners and an arm64 macOS runner. Architectures
are not yet recorded in a release manifest, so no architecture is promised beyond what
CI runs.

## What works today

- Local repositories on one machine: `init`, World history, `check`, and Layer
  `create`, `state`, `history` and `diff`.
- Clone-backed Layer workspaces: create or open a workspace, capture its changes into
  checkpoints, publish a whole Layer onto the local World, and discard. Layer Stack
  combines a checkpoint into a target Layer.

## What is not supported

- **No hosted collaboration.** There is no hosted repository, remote attach, shared
  Publish Requests, reviews, comments, forks or control plane yet.
- **No release binaries or packages.** Build from source with `cargo`.
- **No compatibility promise.** Command output, JSON fields and on-disk formats may
  change before 1.0.
- **No project adoption.** `javelin init` does not take over existing files.
- **No background daemon.** Changes are captured when a Javelin command runs.
- **No Git import, export or migration.** Javelin does not read or write Git repositories.

## Getting help

- Bugs and questions: [open an issue](https://github.com/Dhruv2mars/javelin/issues).
- Security problems: follow [SECURITY.md](SECURITY.md). Do not use public issues.
