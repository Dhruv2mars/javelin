# Layer CLI

The CLI provides native Layer metadata on macOS, Linux, and Windows. Writable
ordinary paths use the existing Linux OverlayFS runtime and configured mount
helper. Workspace commands return `unsupported` on macOS and Windows.
Checkpoints can be explicit or captured by an opt-in foreground Linux watch command.
Whole-Layer Publish uses the safe Linux runtime.

## Build and install

The root `javelin` package remains the library. `crates/javelin-cli` owns the
`javelin` executable and its actual-binary tests. The CLI can depend on standalone
adapters that also use the library without creating a library dependency cycle.

From the checkout root:

```sh
cargo build --locked --release
cargo run --locked -p javelin-cli -- --help
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo install --locked --path crates/javelin-cli --bin javelin
```

Default root build, test and lint commands select both the library and CLI.
The release executable remains `target/release/javelin` (`javelin.exe` on Windows),
and installation still provides `javelin`. The Linux helper remains in the root
package. Use `-p javelin` for library-only checks and `-p javelin-cli` for CLI-only
checks. Merge and Stack adapters are root workspace members and default check
targets; they use the root lockfile. Other standalone packages retain their own
workspaces.

## Commands

```text
javelin init [project-directory] [--json]
javelin history [project-directory] [--json]
javelin check [project-directory] [--json]

javelin checkpoint ID [--path DIR]

javelin layer create ID [--world N]
javelin layer state ID
javelin layer history ID
javelin layer publication --operation ID
javelin layer diff ID [--revision N]
javelin layer clone ID --checkpoint HASH
javelin layer stack ID --revision N --source HASH --operation ID
javelin layer discard ID --revision N --operation ID

javelin workspace create ID [--world N]
javelin workspace open ID
javelin workspace path ID
javelin workspace discard ID --operation ID
javelin workspace recover

javelin workspace init --root ABS --helper ABS
javelin workspace mount ID
javelin workspace path ID
javelin workspace unmount ID
javelin workspace checkpoint ID --operation ID
javelin workspace watch ID [--quiet-ms N] [--duration-ms N]
javelin workspace publish ID --operation ID [--actor human|agent]
    [--context HASH | --trace FILE] [--allow-missing-trace]
javelin workspace discard ID --operation ID
javelin workspace rebind ID --revision OLD --to-revision NEW
javelin workspace recover
```

All `checkpoint`, `layer` and `workspace` commands accept `--repo DIR` and `--json`. The default
repository is the current directory, with no ancestor search. Clone-backed
`workspace` commands take no `--helper`. The Linux OverlayFS `workspace` commands
require `--root ABS` and `--helper ABS`, and passing `--helper` selects them. `--allow-missing-trace` is a flag. Other options take separate values
and may appear before or after positional arguments. Unknown, repeated, and
irrelevant options are errors. Use `./name` for a path that starts with `-`.

`init` creates an empty immutable World 0. Existing project files remain untouched
and untracked. It does not adopt an existing project. `layer create` uses the
current World head unless `--world N` selects an exact version. A Layer ID and
an operation ID each contain 1 to 128 ASCII letters, digits, `-`, `_`, or `.`.
Checkpoint hashes come from `layer state`, `layer history`, or checkpoint output.

`layer state` and `history` report durable native revisions, not live filesystem
changes. `layer diff` compares the latest checkpoint with its exact base World.
It never captures edits. `--revision N` selects a retained active revision.
After discard, the default diff reads the last active revision. Plain output
uses `A`, `M`, and `D` with JSON-quoted paths. JSON includes exact before and
after entries, including file mode, kind, and content object IDs. It is a path
diff, not a textual patch.

Format 3 repositories can also contain published Layers. `state` reports the
published World, operation ID, and final head. `history` retains the active
revisions followed by the terminal publication record. Diff and clone continue
to use the final immutable checkpoint. New native discard operations on a
published Layer return `layer_published`. Further edits require a new Layer.

`layer clone` starts a separate Layer from the specified immutable checkpoint.
Later edits to either Layer cannot change that source checkpoint. Creating or
cloning again with the same ID and snapshot returns the original revision 0,
even if the Layer subsequently advanced or was discarded. Read `layer state`
to find its current state.

## Clone-backed workspaces

A clone-backed workspace is a plain directory, `.javelin/layers/ID`, cloned from
its World base on the repository's volume. No mount, helper, `sudo` or daemon is
involved, and it works on macOS, Linux and Windows. See
[workspace-approach.md](workspace-approach.md) for the design and measurements.

- `create ID [--world N]` creates the Layer, as `layer create` does, and opens its
  directory. Retrying with the same ID and World is safe.
- `open ID` materializes the directory of an existing active Layer, including one
  made by `layer clone`, by cloning its base and applying its checkpoint. An
  existing directory is returned untouched, with `created: false`.
- `path ID` prints the directory. JSON adds `exists`, `base_version` and
  `base_root`.
- `discard ID --operation ID` discards the latest active revision, then renames the
  directory into `.javelin/trash`. Retry with the same operation ID.
- `recover` finishes interrupted steps and reclaims all trash. Every other
  workspace command reclaims at most 4,096 entries.

JSON reports `clone.strategy` (`directory_clone`, `snapshot`, `file_clone` or
`copy`), `clone.fallback` (why faster strategies failed), `clone.copied_bytes`,
the base `source` (`cached`, `derived` or `materialized`) and separate base and
Layer timings. Plain output prints the strategy, and a copy adds a warning that
unchanged data was duplicated. A World whose paths collide on the volume (case,
Unicode normalization, or Windows reserved names) fails with `unsupported`.

Capture of live edits into checkpoints is separate work. Until it lands, `layer
diff` and Publish see only explicit checkpoints, not edits in the directory.

## Linux setup

The runtime requires Linux OverlayFS, `/usr/bin/sudo`, and the root-owned
`javelin-linux-layer-helper` binary. The helper and all its ancestors must be
canonical, root-owned, and not writable by group or others. An administrator
must configure a root-owned `/etc/javelin/linux-layer-parents` with permitted
runtime parents. The runtime root must be absolute and canonical under one of
those parents. Its final name uses letters, digits, `_`, or `-` and begins with
a letter or digit. The helper runs noninteractively through `sudo -n`.

This CLI does not install the helper or grant privileges. Production installation
is still open. See [Linux runtime setup and limits](linux-layer-runtime.md) for
the underlying layout, trust model, and platform evidence. Mount verification
runs only on disposable hosted Linux, never on the developer laptop.

## Explicit checkpoint flow

After the Linux prerequisites are configured, run the following with the built
`javelin` binary on `PATH`. Substitute your permitted runtime root and installed
helper path.

```sh
mkdir project
cd project
javelin init
javelin layer create work --world 0

runtime=/var/lib/javelin/work
helper=/usr/libexec/javelin/javelin-linux-layer-helper
javelin workspace init --root "$runtime" --helper "$helper"
view=$(javelin workspace mount work --root "$runtime" --helper "$helper")
printf 'hello\n' > "$view/hello.txt"

javelin workspace checkpoint work --operation work-save-1 --root "$runtime" --helper "$helper"
javelin layer diff work
javelin layer history work
```

`mount` and `path` print only the ordinary path in plain mode. `path` succeeds
only for an existing mounted, unfrozen, active view without pending work. Use
`mount` to open or resume a view. Stop tools that hold files open for writing
before checkpointing. A writer can make checkpoint return `busy`; close it and
retry the same operation ID. Successful checkpoint output includes its immutable
checkpoint hash and reports empty directories that native state cannot represent.
Writers during the frozen window may receive `EROFS`.

Use the checkpoint hash printed above to create a separate view:

```sh
javelin layer clone copy --checkpoint CHECKPOINT_HASH
copy_view=$(javelin workspace mount copy --root "$runtime" --helper "$helper")
cat "$copy_view/hello.txt"
printf 'private\n' > "$copy_view/hello.txt"
cat "$view/hello.txt"
```

The last command still prints `hello`. Uncheckpointed changes exist only in a
view's private storage. `workspace unmount` retains them, and `mount` resumes
them. The runtime stores sparse private changes over shared immutable caches.
There is no full-copy fallback or separate CLI capture implementation.

## Stack and explicit workspace rebind

`layer stack TARGET --revision N --source HASH --operation ID` delegates to
`javelin_stack::stack_layer`. HASH is an immutable source checkpoint, not a live
Layer name. N selects the exact retained target head. Read both from checkpoint
output or `layer history`; keep those inputs and the operation ID for retries.
Compatible edits, including separated changes within one text file, combine.
Structural or overlapping edits return explicit conflicts without changing the
native Layer or World. The CLI never resolves a conflict for its owner.

Before metadata Stack, stop target tools, checkpoint **both** source and target,
and unmount the target. Unmount alone retains uncaptured edits; it does not save
them into native state. Stack only changes native metadata and does not inspect,
freeze, or select filesystem views. Checkpoint and unmount protect private edits
and prevent tools from continuing against an old view during this separate action.

For example, after source checkpoint HASH and target revision 1 are saved:

```sh
javelin workspace unmount target --root "$runtime" --helper "$helper"
javelin layer stack target --revision 1 --source HASH --operation stack-1
# Use the resulting revision reported by Stack (2 in this example):
javelin workspace rebind target --revision 1 --to-revision 2 --root "$runtime" --helper "$helper"
target_view=$(javelin workspace mount target --root "$runtime" --helper "$helper")
# Run editor/compiler tools in "$target_view", then:
javelin workspace checkpoint target --operation target-save-2 --root "$runtime" --helper "$helper"
javelin workspace publish target --operation target-publish-1 --root "$runtime" --helper "$helper"
```

Stack and filesystem selection are separate durable actions, with no shared atomic
transaction. If rebind fails, Stack's native result remains; fix the reported
workspace condition and retry the same explicit old/new pair. Rebind delegates to
`LinuxLayerRuntime::rebind(repo, old, desired)`. Both tokens are retained active
heads of the named Layer; desired must be its current native active head.
Rebind refuses uncaptured edits, empty directories and held writers without
deleting them. Checkpoint before Stack; if edits were missed, retain the old view
and resolve explicitly rather than deleting private storage. An originally
unmounted view stays unmounted after validation. Mount it explicitly to use tools.
There is no automatic refresh or automatic head selection. Foreground watch retries
busy freezes after a new quiet window.

Rebind has no additional operation ID: the runtime's durable selection is keyed
by the exact old/desired heads. A completed exact retry is a no-op even after new
private edits, provided desired is still the current native active head. Recovery
finishes recorded selections, never retargets them to a later native advance.

Stack success and stale retries return the original native outcome even after
later edits or Publish. Reusing an operation ID with another target revision or
source hash returns `operation_id_reused`; use a new ID for new work. Successful
Stack retains frozen source ancestry in the target checkpoint. Later source
edits cannot change that identity or the selected bytes. Conflicts reserve no
operation ID. A changed resolution must be checkpointed and supplied explicitly.

JSON Stack output includes `target`, `source`, `operation`, and either the native
`update` or explicit `conflicts` with path, kind, and base/ours/theirs entries.
Rebind output includes the exact `expected` and `desired` heads. Plain output
reports the outcome and immutable selection. Merge admission/load failures use
`stack_merge_error` on stderr with the adapter's resource/limit or loader message;
native storage failures retain their native error codes. Both return exit 1.
Conflict and stale Stack outcomes return exit 3 on stdout, with `ok: false`.

## Whole-Layer Publish

After editing ordinary paths and explicitly checkpointing, Publish the entire
Layer. There is no implicit checkpoint or path selection:

```sh
javelin workspace publish work --operation work-publish-1 --root "$runtime" --helper "$helper"
javelin history
javelin layer state work
javelin layer publication --operation work-publish-1
# Retry an uncertain result with exactly the same operation and attribution:
javelin workspace publish work --operation work-publish-1 --root "$runtime" --helper "$helper"
# Continue in a new writable Layer from the current World:
javelin layer create next
javelin workspace mount next --root "$runtime" --helper "$helper"
```

Publish delegates freeze, exact checkpoint matching, durable receipt binding,
recovery and read-only enforcement to `LinuxLayerRuntime::publish`. Close writable
file handles first. Held writers return `busy`. Uncaptured tracked/nonignored
edits and nonignored empty directories refuse with `invalid_input`; checkpoint
edits and remove those empty directories before retrying. Ignored untracked
artifacts stay private and remain readable in the published view. A successful mounted view remains readable and
read-only (`EROFS` on writes). `workspace path` only reports writable views;
retain the original path to read a published view or inspect immutable metadata.
Further work requires `layer create` or `layer clone --checkpoint HASH`.

The default attribution is human and requires no trace. For agent work use
`--actor agent --context HASH` to reference an existing native Blob, or
`--actor agent --trace FILE` to store exact opaque bytes through the native Blob
API. Trace files are read into memory; this is not a streaming importer. No
interpretation, identity authentication, hosted permission, or credential is
invented. Missing agent context refuses with `missing_agent_trace` (exit 1);
retry with explicit `--allow-missing-trace` to accept a durable
`missing_agent_trace` receipt warning. Human attribution rejects agent options.
Optional repository checks are not mandatory defaults; this local command
supplies no additional policy guard.

JSON Publish output contains the complete native `receipt`, including expected
head, attribution, locked World, warnings and outcome. Plain output reports the
World or conflict/stale result, exact checkpoint, and warnings. Conflicts and
stale receipts return exit 3 with `ok: false` on stdout; World and private edits
are retained. Conflict paths are JSON-quoted in plain output. Compatible World
advances compose using native rules; intersecting edits never silently clobber.
No refresh or conflict-resolution command is added in this unit.

Reuse the same operation, Layer and attribution after uncertainty. A completed
retry returns the original receipt, even after later checkpoints or World
advances. Changed trace bytes, context, actor or override with a used operation
returns `operation_id_reused`. Keep raw trace bytes unchanged or retry using its
receipt context hash. A refusal without a durable receipt reserves no operation.
`layer publication --operation ID` inspects that original receipt on every
platform; unknown IDs return success with `receipt: null`. Conflict/stale
inspection retains exit 3. Receipt inspection never publishes.

First Publish requires a mounted runtime view. There is no metadata-only Publish
command. Unmounting a published view retains the durable publication; a remount
is read-only. Runtime recovery reconciles published views and reports partial
failures. An unmounted view can only be observed through runtime metadata until
remounted: there is no claim of physical enforcement against an independently
held or out-of-band path. Do not edit hidden runtime storage. Production helper
installation and macOS/Windows writable workspace support remain open.

## Retry and discard

Reuse the same Layer ID and explicit `--world N`, or the same clone ID and
checkpoint hash, after an uncertain create or clone. If World advances between
attempts, an omitted `--world` would select a different snapshot and be refused.
For checkpoint and discard, choose a unique operation ID before the first attempt
and reuse it after uncertainty. Changed input with a used ID returns
`operation_id_reused`. A checkpoint retry returns its original durable outcome
with `replayed: true`; it does not capture newer live edits. Use a new ID for a
new checkpoint.

`workspace recover` resolves pending operations using their recorded IDs, aborts
interrupted cache staging, thaws abandoned freezes, and removes discarded views.
It reports individual failures and exits nonzero if any remain. Keep the same
repository and runtime root when recovering. Do not edit the runtime's hidden
backing directories or binding records.

For a mounted Layer, close tools and leave its directory before discarding:

```sh
javelin workspace discard work --operation work-discard-1 --root "$runtime" --helper "$helper"
javelin workspace recover --root "$runtime" --helper "$helper"
```

Runtime discard uses the view's bound native head. A busy unmount refuses before
discard, and a stale head returns `stale` while retaining edits. Discard is
terminal and removes private view storage, including uncheckpointed edits.
World history and immutable checkpoints remain inspectable and cloneable.

`layer discard ID --revision N --operation ID` performs native discard against
the exact retained active revision N. It returns `stale` if the head advanced.
Use it for Layers without views, or to retry a runtime discard after its view
has already been removed. For that retry, N is the active revision immediately
before the discarded event in `layer history`. Reuse the original operation ID.
Native discard does not unmount or remove a view. If used on a Layer with a view,
stop tools first and run `workspace recover` to remove the private storage.

## Daemonless checkpoint

`javelin checkpoint ID` captures a plain Layer directory, by default the current
directory (`--path DIR` selects another). It is the agent lifecycle hook: run it at
turn end or tool-call end. It works on every OS and needs no mount, helper or
background process.

```sh
cd my-layer-directory
javelin checkpoint my-layer --repo /path/to/project --json
```

A per-Layer stat cache under `.javelin/capture/` lets a rescan hash only entries
whose size, mtime, ctime, inode or mode changed, plus racy entries modified within
2 seconds of the scan. When the directory differs from the latest checkpoint, the
command records a new checkpoint under a generated operation ID derived from the
expected head and the changes. A retry, or a concurrent identical capture, replays
that operation instead of adding history. When nothing differs it prints
`unchanged: true` and records nothing. JSON output adds
`capture: {hashed, uncertain, rebuilt}`. `uncertain` lists files that kept
changing during three read attempts; they keep their prior checkpointed state and
are hashed again next time. A stale head exits 3 like other mutations.

Symlinks are recorded as links, never followed. The executable bit is read on Unix
and carried over from the prior checkpoint on Windows. Deletes and renames become
native path changes; a case-only rename is a delete plus an add. Empty directories
are not represented. FIFOs, sockets and devices, and names outside the native path
model, are refused with `unsupported`. Entries named `.javelin` are never captured.
Root `.javelinignore` rules hide untracked paths only; tracked files under an
ignored path are still captured. An invalid rule file refuses with `invalid_input`.

## Output and exit status

`--json` prints one JSON object. Successful results and stale outcomes go to
stdout, with no stderr. Failures have no stdout and retain the existing stderr
shape `{"ok":false,"error":{"code":"...","message":"..."}}`.

Exit status is 0 for success (including unknown receipt lookup), 1 for runtime or storage failure including
`unsupported` and `busy`, 2 for invalid input, and 3 for a conflict or stale mutation/receipt. Stale
output has `ok: false` and the native `stale` outcome. Partial recovery has
`ok: false`, exit 1, and the full recovery report on stdout.

The existing `init`, `history`, and `check` syntax and JSON fields are unchanged.
No command invokes or interprets Git. The disposable hosted Linux actual-binary
smoke runs with a failing Git stand-in and a checked positive control.


## Foreground automatic checkpoints on Linux

After explicitly mounting a Layer, run this in a foreground terminal:

```sh
javelin workspace watch my-layer --root /configured/runtime --helper /configured/helper
# Bounded automation: stop scheduling samples after 10 seconds.
javelin workspace watch my-layer --root /configured/runtime --helper /configured/helper --duration-ms 10000
```

Nothing starts watch from init, mount or other commands. There is no daemon. Stop
with Ctrl-C; private edits remain. Output (or `--json`) is a final summary on duration
expiry, or an error on failure. Individual checkpoints appear in `layer history`
while watch runs. The summary counts captures completed or recovered by this run.

Watch reads discovered/tracked file bytes, symlink targets and executable bits, then waits 250ms
after each scan before sampling again. The default quiet window is 1000ms; choose
`--quiet-ms N` with N >= 250. At least two equal content samples spanning that
window are required. Renames and deletions appear as normal native path changes.
Unchanged content, including a rewrite of identical bytes, creates no new history.
Empty directories are not represented by native checkpoints.

A quiet sample is not an application transaction. Watch still freezes the view;
held write descriptors and shared writable mappings prevent capture. The frozen
candidate must match the quiet observation exactly. A busy freeze or a boundary
change restarts observation and retries later. Locks are released between samples,
so explicit checkpoint, status and Publish can run. Watch never publishes, Stacks,
refreshes, discards, changes World or automatically selects an external head.

On interruption after a pending capture was written, rerun the same watch command
(or use `workspace recover`). Existing runtime pending records and durable native
receipts preserve the operation ID and recover its outcome before another sample.
IO and corruption errors stop watch and remain visible; they are not silently
retried. Resolve the cause and retry. Published stops with `layer_published` and
preserves the read-only view; Discarded stops with a clear error. An externally
advanced native head stops and requires explicit rebind. Interrupted Publish,
discard or rebind must be recovered explicitly.

`--duration-ms N` must be positive. It bounds scheduling, not the time spent in an
individual scan/helper/storage call; it does not force a final checkpoint of
nonquiet edits. Sampling can miss edits made and reverted entirely between scans.
Watch observes the effective tree after root `.javelinignore` discovery filtering.
Large-tree performance and resource bounds beyond existing capture/storage limits
remain unverified.

Linux workspace discovery supports root `.javelinignore` literal paths; see
[syntax and tracked-file guarantees](linux-layer-runtime.md#ignore-aware-discovery).
