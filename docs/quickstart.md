# Local developer quickstart

This covers the merged, from-source local CLI on macOS, Linux and Windows. A Layer's
workspace is a plain directory cloned from its World base; ordinary editors,
compilers and agents work in it directly. Javelin captures it into checkpoints when
you run a Javelin command or `javelin checkpoint`, with no mount, helper or daemon.
There is no GA release or complete hosting workflow.

A **World** is the repository's accepted immutable version: initialization starts
at empty World 0. A **Layer** holds isolated work over a World. A **checkpoint** is
an immutable snapshot of that Layer's changes. Clone starts another Layer from a
checkpoint; Stack combines a source checkpoint into a target Layer; Publish accepts
the whole checkpointed Layer as the next World and ends that Layer.

## Build and try metadata

Start in a source checkout with Rust/Cargo and a native C build toolchain available
(SQLite is bundled). The commands below use a POSIX shell and a unique temporary
directory; Windows users can use an equivalent shell and `javelin.exe`. Rust 1.93.0
was used for the macOS walkthrough below; this is not a minimum-version or OS promise.
No package install, mount, or privilege change is needed for this section.

```sh
# From the Javelin source checkout:
source_dir="$PWD"
demo=$(mktemp -d)
cd "$demo"
cargo build --locked --release -p javelin-cli \
  --manifest-path "$source_dir/Cargo.toml" --target-dir "$demo/build"
javelin="$demo/build/release/javelin"
"$javelin" --help

mkdir project
cd project
printf 'existing file\n' > existing.txt
"$javelin" init
"$javelin" history
"$javelin" layer create work --world 0
"$javelin" layer state work
"$javelin" layer history work
"$javelin" layer diff work
"$javelin" check
cat existing.txt
```

Expect World 0, an active Layer at revision 0, no checkpointed changes, and
`Integrity OK`. `existing.txt` remains unchanged and **untracked**: `init` does not
adopt an existing project. Edit files inside a Layer's workspace to capture them in
the flow below. Metadata commands do not invoke or interpret Git and need
no Git repository. `state`, `history`, and `diff` read durable snapshots, not live
edits; diff is a path inventory rather than a textual patch.

Run Layer/workspace commands from the repository directory, or pass `--repo DIR`;
there is no ancestor search. See the [full CLI reference](cli.md) for JSON output,
all commands, exit codes, installation commands, and retry rules.

## Workspace workflow

Continue in the `project` directory with the `javelin` variable still set.

```sh
"$javelin" workspace open work
work=$("$javelin" workspace path work)
printf 'hello\n' > "$work/hello.txt"
"$javelin" checkpoint work
"$javelin" layer diff work
"$javelin" layer history work
```

`workspace open` clones the Layer's directory, `.javelin/layers/work`, and reports
how; `workspace path` prints just the path. `checkpoint` captured `hello.txt` as revision 1; `existing.txt`
outside the workspace is still untracked. Copy the checkpoint hash from that output
into `work_checkpoint` below to start a second Layer from it:

```sh
work_checkpoint=REPLACE_WITH_WORK_CHECKPOINT_HASH
"$javelin" layer clone copy --checkpoint "$work_checkpoint"
"$javelin" workspace open copy
copy=$("$javelin" workspace path copy)
cat "$copy/hello.txt"
printf 'from copy\n' > "$copy/copy.txt"
test ! -e "$work/copy.txt"
```

The clone reads `hello`, and its new file stays private to it. Now publish `work`,
then a separate Layer started from World 0:

```sh
printf 'ready\n' > "$work/ready.txt"
"$javelin" workspace publish work --operation work-publish-1
"$javelin" workspace create side --world 0
side=$("$javelin" workspace path side)
printf 'side\n' > "$side/side.txt"
"$javelin" workspace publish side --operation side-publish-1
"$javelin" history
"$javelin" check
"$javelin" workspace publish copy --operation copy-publish-1 || test $? -eq 3
"$javelin" workspace discard copy --operation copy-discard-1
```

Each Publish first captures the directory, so `ready.txt` and `side.txt` need no
separate checkpoint. `work` becomes World 1. `side` started before that Publish,
but its changes do not intersect World 1, so they compose as World 2 with all three
files. `copy` also adds `hello.txt`, which World 1 already changed, so its Publish
returns a durable conflict (exit 3) and publishes nothing, even though the content
is identical; it is discarded instead. Default Publish attribution is human and requires no trace;
[agent attribution](cli.md#whole-layer-publish) uses explicit context/trace or a
missing-trace override. This local command does not supply hosted authentication
or additional repository policy checks.

Reuse the same operation ID and exact inputs after an uncertain result; choose a
new ID for new work. A published Layer's directory stays readable but is no longer
captured; continue with `workspace create next`. `workspace discard ID --operation
ID` moves a directory to trash, including edits capture could not record; see
[recovery and discard](cli.md#retry-and-discard). `layer stack` combines a
checkpoint into another Layer, but its directory is not refreshed yet, so a stacked
Layer cannot be published from its workspace ([Stack](cli.md#stack)). Workspaces
provide isolation, not a security sandbox.

## Verification scope

- [x] Run this walkthrough with the release binary in a unique temporary
  directory on macOS (APFS). Every command shown succeeds and `check` reports
  integrity OK.
- [x] Workspace open, capture, Publish, exact retry, conflict and composition are
  covered by the actual-binary tests in
  [`cli_workspace.rs`](../crates/javelin-cli/tests/cli_workspace.rs), which CI runs on
  macOS, Linux and Windows with a failing Git stand-in first on `PATH`.

This walkthrough does not establish release packaging/signing, minimum OS support,
or a background autosave daemon, or complete hosting. `adopt` (see `docs/cli.md`) is
covered by `crates/javelin-cli/tests/cli_adopt.rs`, but this walkthrough does not run it.
