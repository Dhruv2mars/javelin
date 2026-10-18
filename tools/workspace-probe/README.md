# Run the filesystem probe

This development tool exercises ordinary paths. It does not mount filesystems or
implement a runtime API. Directory and clonefile controls can verify the probe;
they cannot verify a production sparse Layer backend.

From the repository root, run:

```sh
rtk proxy bun tools/workspace-probe/verify.mjs /tmp/javelin-probe-evidence
```

`verify.mjs` runs controls sequentially. It expects directory and macOS clonefile
success, detects deliberate hard-link leakage, checks safe report destinations,
and rejects overlapping caller paths. A 300-ms abort checks incomplete reporting.
Another abort stops a deliberately stalled external tool and its child process.
It then exercises caller-prepared directory paths. Those paths remain an
explicit control, with backend cloning INCONCLUSIVE.
If you omit its output argument, it creates a unique temporary evidence directory.

For one run:

```sh
rtk proxy bun tools/workspace-probe/probe.mjs --backend directory --out /tmp/directory.json
rtk proxy bun tools/workspace-probe/probe.mjs --backend clonefile --out /tmp/clonefile.json
```

`clonefile` calls the macOS syscall directly for each regular file. Any syscall
failure aborts setup. It never falls back to copying. `directory` deliberately
materializes the fixture using ordinary file copies.

## Exercise caller-provided paths

Use disposable paths only. The probe modifies the fixture in A, B, and the clone.
It retains caller paths, including files left by failed operations. It refuses
extra base entries and initial views that differ from the exact fixture.

Create an empty fixture directory, then seed it:

```sh
rtk proxy mkdir /tmp/javelin-base-fixture
rtk proxy bun tools/workspace-probe/probe.mjs --seed /tmp/javelin-base-fixture
```

Provision an immutable base and three initially identical isolated views through
the backend being evaluated. Provisioning, enabling, mounting, and teardown
remain the backend owner's responsibility. Write a configuration file:

```json
{
  "base": "/path/to/base-fixture",
  "layerA": "/path/to/layer-a",
  "layerB": "/path/to/layer-b",
  "clone": "/path/to/initial-clone"
}
```

Run the behavior probe, with the report outside those roots:

```sh
rtk proxy bun tools/workspace-probe/probe.mjs --backend paths --paths /tmp/paths.json --out /tmp/paths-result.json
```

The four paths must be distinct, non-nested directories. Report destinations
inside them, through a directory symlink alias, or at an existing symlink are
rejected before mutation. Output parents must already exist.
Existing output files must be singly linked regular files. Hard links, FIFOs,
and directories are rejected. Report creation uses exclusive open.

This mode proves only observed path behavior. It cannot create a clone of a
modified Layer or interrupt and restart its backend. Those checks remain
INCONCLUSIVE. Gate 2 also needs independent backend storage evidence that the
base is immutable and shared and changes and clones are sparse.

## Interpret results

JSON records exact command arguments, outputs, fixture hashes, host facts,
per-check verdicts, contract gaps, and cleanup state. Shell tools are `cat`,
`cmp`, `cp`, `mv`, `rm`, `find`, and `cc`; filesystem APIs access ordinary paths.
The compiler reads a C source and header inside each view, creates an executable
there, and that executable must print the view's private value.

Exit 0 means all behavior checks passed. It does not mean Gate 2 passed.
Exit 1 means a check failed or was inconclusive. Exit 2 means invalid setup.
Exit 124 means the wall budget expired. Inspect `behaviorVerdict`, `checks`, and
`contract` independently. Missing or partial evidence is INCONCLUSIVE.

The default wall budget is 60 seconds. `--wall-ms` permits 100 through 60000 ms.
Each external command has a 20-second timeout and 16-KiB output cap. Writer
readiness has a 5-second deadline; an orphaned writer exits after 10 seconds.
Wall expiry stops active POSIX command groups. Abort reporting has a separate
100-ms exit deadline; if reporting fails, exit 124 still means INCONCLUSIVE.
The fixture has six regular files, one directory, and one symlink, plus its root.
It contains 8,339 file bytes. Runs use three views, with no stress expansion.
Snapshot traversal rejects more than 64 entries or 256 KiB per tree. Compiler
output is not disk-size capped; only the fixed tiny source is compiled.

Timeouts can leave an owned temporary fixture. Its path is recorded as
`ownedTemporaryRoot`. Verification cleans up its deliberate abort. A genuinely
uninterruptible kernel operation can still require host intervention; that
failure mode was not induced. No component is installed, no host security
setting changes, and neither the probe nor its fixtures uses Git.

## Gate 2 proof

`gate2.mjs` drives a `javelin` binary through the user flow on one filesystem and
records a verdict for every gate-2 clause in `docs/build-contract.md`: unprivileged
run, no Javelin process between commands, ordinary tools (read, in-place write,
atomic-save and backup renames, deletes, symlinks, watchers, `rustc` builds,
case-only renames), automatic capture (including mtime-preserving, same-second and
concurrent rewrites), Worlds with case, Unicode-normalization or Windows-reserved
collisions (refused with `unsupported` where the volume cannot hold them, otherwise
materialized exactly), shared data measured by free-space deltas against a
byte-copy control, the copy fallback reporting itself, and interrupted create,
checkpoint and discard recovering on the next command.

```sh
rustc --edition 2021 -o /tmp/shim tools/workspace-probe/shim.rs
mkdir -p /tmp/g2shims && for t in git sudo mount umount; do cp /tmp/shim /tmp/g2shims/$t; done
cargo build --locked --release -p javelin-cli
cargo build --locked --release -p javelin --example gate2_seed
rtk proxy bun tools/workspace-probe/gate2.mjs --javelin target/release/javelin \
  --seed target/release/examples/gate2_seed \
  --dir /tmp/gate2-run --fs apfs --expect shared --out /tmp/gate2.json --shims /tmp/g2shims
```

`--expect` is `shared` for clone-capable filesystems and `copy` for the fallback.
`--only CLAUSE,...` limits the run; `--bulk-mb N` sizes the sharing fixture
(default 128). Exit 0 means every check is VERIFIED, 1 that one is not, and 2 that
setup failed. `--known gate2-known-failures.json` lists product bugs the proof has
already found, per filesystem, with their finding: if only those fail the exit is 3
and the verdict stays NOT VERIFIED; a listed check that passes fails the run, so the
list only shrinks. Gate 2 is proven once the list is empty. The command names and JSON fields live in one adapter, `S`, in
`gate2.mjs`. The shims are failing stand-ins for tools Javelin must never run.

Clone-backed workspaces have no Publish yet, so `--seed` names the fixture tool that
accepts each test World: `examples/gate2_seed.rs` calls the same library functions
as the workspace tests. Everything after seeding goes through the `javelin` binary.
Repositories seal World bases with mode 000; use `chmod -R u+rwx` before deleting a
run directory.

`gate2-reference.mjs` is a small model of the same command surface. It is not product
code: it shows the harness passes against an honest implementation on each
filesystem, and `gate2-controls.mjs` mutation-tests the harness by making it lie
(hard links, copies that claim clones, stat-only capture, a capture that writes
into the Layer, a lingering daemon, a torn discard, no refusal). Every lie must
fail its own check.

`.github/workflows/gate2.yml` runs all of this on APFS, ext4, XFS, btrfs, NTFS and
a ReFS Dev Drive. Linux runs under `no_new_privs`; Windows runs through
`deelevate.rs`, which drops the runner's administrator token to a standard-user,
Medium-integrity token.
