# Run the filesystem probe

This development tool exercises ordinary paths. It does not mount filesystems or
implement a runtime API. Directory and clonefile controls can verify the probe;
they cannot verify a production sparse Layer backend.

From the repository root, run:

```sh
rtk proxy bun tools/workspace-probe/verify.mjs /tmp/javelin-probe-evidence
rtk proxy bun tools/workspace-probe/discover.mjs /tmp/javelin-host.json
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
