# Layer CLI

The CLI provides native Layer metadata on macOS, Linux, and Windows. Writable
ordinary paths use the existing Linux OverlayFS runtime and configured mount
helper. Workspace commands return `unsupported` on macOS and Windows.
Checkpoints are explicit. Whole-Layer Publish uses the safe Linux runtime. Autosave remains later work.

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
checks; standalone adapter crates retain their own workspaces.

## Commands

```text
javelin init [project-directory] [--json]
javelin history [project-directory] [--json]
javelin check [project-directory] [--json]

javelin layer create ID [--world N]
javelin layer state ID
javelin layer history ID
javelin layer publication --operation ID
javelin layer diff ID [--revision N]
javelin layer clone ID --checkpoint HASH
javelin layer discard ID --revision N --operation ID

javelin workspace init
javelin workspace mount ID
javelin workspace path ID
javelin workspace unmount ID
javelin workspace checkpoint ID --operation ID
javelin workspace publish ID --operation ID [--actor human|agent]
    [--context HASH | --trace FILE] [--allow-missing-trace]
javelin workspace discard ID --operation ID
javelin workspace recover
```

All `layer` and `workspace` commands accept `--repo DIR` and `--json`. The default
repository is the current directory, with no ancestor search. Every `workspace`
command requires `--root ABS` and `--helper ABS`. `--allow-missing-trace` is a flag. Other options take separate values
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
file handles first. Held writers return `busy`. Uncaptured edits and empty
directories refuse with `invalid_input`; checkpoint edits and remove empty
directories before retrying. A successful mounted view remains readable and
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
