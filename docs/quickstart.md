# Local developer quickstart

This covers the merged, from-source local CLI. Repository and Layer metadata work
on macOS, Linux, and Windows. Writable ordinary filesystem paths currently require
Linux OverlayFS and an administrator-configured helper. The flow below uses explicit
checkpoints. Optional foreground [`workspace watch`](cli.md#foreground-automatic-checkpoints-on-linux)
is available on Linux; it does not start automatically or run as a background autosave
daemon. There is no GA release or complete hosting workflow.

A **World** is the repository's accepted immutable version: initialization starts
at empty World 0. A **Layer** holds isolated work over a World. A **checkpoint** is
an immutable snapshot of that Layer's changes. Clone starts another Layer from a
checkpoint; Stack combines a source checkpoint into a target Layer; Publish accepts
the whole checkpointed Layer as the next World and makes that Layer read-only.

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
adopt an existing project. Edit files inside a mounted Layer to capture them in
the Linux flow below. Metadata commands do not invoke or interpret Git and need
no Git repository. `state`, `history`, and `diff` read durable snapshots, not live
edits; diff is a path inventory rather than a textual patch.

Run Layer/workspace commands from the repository directory, or pass `--repo DIR`;
there is no ancestor search. See the [full CLI reference](cli.md) for JSON output,
all commands, exit codes, installation commands, and retry rules.

## Linux ordinary-path workflow (configured hosts only)

**Prerequisite:** use a disposable Linux host with OverlayFS, `/usr/bin/sudo`, and
an administrator-configured, root-owned `javelin-linux-layer-helper` invoked through
`sudo -n`. The helper and its ancestors must be canonical, root-owned, and not
group/other writable. The absolute canonical runtime root must be under a permitted
parent in root-owned `/etc/javelin/linux-layer-parents`.
Read [Linux setup](cli.md#linux-setup) and the
[runtime trust model and limits](linux-layer-runtime.md#trust-model-and-limits)
before proceeding. These commands do not install the helper or grant privileges;
production helper installation remains open. Do not run mount verification on a
developer laptop. macOS/Windows workspace commands return `unsupported` today.

On that configured Linux host, first run the build/metadata section. Continue in
its `project` directory with the `javelin` variable still set. Replace both example
paths with your configured values; use a fresh runtime root for this repository.

```sh
runtime=/var/lib/javelin/quickstart
helper=/usr/libexec/javelin/javelin-linux-layer-helper
"$javelin" workspace init --root "$runtime" --helper "$helper"
view=$("$javelin" workspace mount work --root "$runtime" --helper "$helper")
printf 'hello\n' > "$view/hello.txt"
# Stop editor/compiler writers before each checkpoint.
"$javelin" workspace checkpoint work --operation work-save-1 --root "$runtime" --helper "$helper"
"$javelin" layer diff work
"$javelin" layer history work
```

Copy the immutable checkpoint hash from that output into `work_checkpoint` below.
The existing file outside the view is still untracked. Only `hello.txt` was captured.

```sh
work_checkpoint=REPLACE_WITH_WORK_CHECKPOINT_HASH
"$javelin" layer clone copy --checkpoint "$work_checkpoint"
copy_view=$("$javelin" workspace mount copy --root "$runtime" --helper "$helper")
cat "$copy_view/hello.txt"
printf 'from copy\n' > "$copy_view/copy.txt"
test ! -e "$view/copy.txt"
"$javelin" workspace checkpoint copy --operation copy-save-1 --root "$runtime" --helper "$helper"
```

The clone initially reads `hello`; its new file is private. Copy its new checkpoint
hash into `copy_checkpoint`. Both Layers are now checkpointed. Stop target tools
and leave the target view before unmounting; unmount alone never checkpoints edits.
For this fresh sequence, `work` is revision 1 and Stack produces revision 2. Confirm
those revisions in the output; use actual retained revisions if you added steps.

```sh
copy_checkpoint=REPLACE_WITH_COPY_CHECKPOINT_HASH
"$javelin" workspace unmount work --root "$runtime" --helper "$helper"
"$javelin" layer stack work --revision 1 --source "$copy_checkpoint" --operation work-stack-1
"$javelin" workspace rebind work --revision 1 --to-revision 2 --root "$runtime" --helper "$helper"
view=$("$javelin" workspace mount work --root "$runtime" --helper "$helper")
cat "$view/copy.txt"
# Run ordinary editor/compiler tools in "$view"; for example:
printf 'ready\n' > "$view/ready.txt"
"$javelin" workspace checkpoint work --operation work-save-2 --root "$runtime" --helper "$helper"
"$javelin" workspace publish work --operation work-publish-1 --root "$runtime" --helper "$helper"
"$javelin" history
"$javelin" layer state work
"$javelin" layer publication --operation work-publish-1
"$javelin" check
```

Stack and rebind are separate durable actions. If Stack reports a conflict or stale
result, stop and resolve it explicitly; do not run the assumed rebind. Rebind
refuses uncaptured edits, empty directories, and held writers. A failed rebind
does not undo Stack; preserve the old view and retry the exact old/new pair after
addressing the reported condition. See [Stack/rebind details](cli.md#stack-and-explicit-workspace-rebind).

Publish has no implicit checkpoint and refuses uncaptured edits or empty directories.
Close writable handles first; a held writer can return `busy`, and writers during
freeze may receive `EROFS`. Default Publish attribution is human and requires no
trace; [agent attribution](cli.md#whole-layer-publish) uses explicit context/trace
or a missing-trace override. This local command does not supply hosted authentication
or additional repository policy checks.

Successful Publish creates World 1 in this fresh example and leaves the view
readable but read-only. Continue with `layer create next` from the current World
and mount `next`. Reuse the same operation ID and exact inputs after an uncertain
result; choose a new ID for new work. Retain hashes/revisions from output for retries.
See [recovery and discard](cli.md#retry-and-discard) before removing private work.
Unmount retains private edits; discard removes them. Do not edit hidden runtime
storage. Ordinary paths provide isolation, not a security sandbox.

## Verification scope

- [x] Build and run the metadata walkthrough in a unique temporary directory on
  macOS/Rust 1.93.0. Git control exited 97 and set its marker; product commands
  left the marker absent. All 13 existing CLI tests passed; local links/anchors,
  shell snippet syntax and whitespace checks passed.
- [x] Validate Linux examples against current `--help`, CLI tests and the existing
  [passing disposable Linux run 39013214105](https://github.com/Dhruv2mars/javelin/actions/runs/39013214105).
  Its [actual-binary tests](../crates/javelin-cli/tests/cli_layers.rs) cover ordinary
  paths, checkpoint/clone, Stack/rebind/tools/checkpoint/Publish, retries and refusals.
  The [workflow](../.github/workflows/layer-cli.yml) configures the helper only on
  disposable hosted Linux and checks a failing Git control plus mount cleanup.

Linux mounts were not rerun for this documentation change. This walkthrough does
not establish release packaging/signing, minimum OS support, writable macOS/Windows
workspaces, a background autosave daemon, project adoption, or complete hosting.
