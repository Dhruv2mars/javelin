# Layer CLI

The CLI runs the same way on macOS, Linux and Windows. A Layer's writable
workspace is a plain directory cloned from its World base, captured into
checkpoints without a daemon, and published whole onto the World.

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
and installation still provides `javelin`. Use `-p javelin` for library-only checks and `-p javelin-cli` for CLI-only
checks. Merge and Stack adapters are root workspace members and default check
targets; they use the root lockfile. Other standalone packages retain their own
workspaces.

## Commands

```text
javelin init [project-directory] [--json]
javelin history [project-directory] [--json]
javelin check [project-directory] [--json]
javelin blame PATH [--world N] [--lines]

javelin checkpoint ID [--path DIR] [--actor human|agent] [--agent NAME]
    [--session ID] [--reference REF]

javelin layer create ID [--world N]
javelin layer state ID
javelin layer history ID
javelin layer publication --operation ID
javelin layer diff ID [--revision N]
javelin layer radar ID
javelin layer clone ID --checkpoint HASH
javelin layer stack ID --revision N --source HASH --operation ID
javelin layer discard ID --revision N --operation ID

javelin workspace create ID [--world N]
javelin workspace open ID
javelin workspace path ID
javelin workspace discard ID --operation ID
javelin workspace publish ID --operation ID [--actor human|agent]
    [--context HASH | --trace FILE] [--allow-missing-trace]
javelin workspace recover
```

All `checkpoint`, `layer` and `workspace` commands accept `--repo DIR` and `--json`. The default
repository is the current directory, with no ancestor search.
`--allow-missing-trace` is a flag. Other options take separate values
and may appear before or after positional arguments. Unknown, repeated, and
irrelevant options are errors. Use `./name` for a path that starts with `-`.

`init` creates an empty immutable World 0. Existing project files remain untouched
and untracked. It does not adopt an existing project. `layer create` uses the
current World head unless `--world N` selects an exact version. A Layer ID and
an operation ID each contain 1 to 128 ASCII letters, digits, `-`, `_`, or `.`.
Checkpoint hashes come from `layer state`, `layer history`, or checkpoint output.

`layer state` and `history` report durable native revisions. `layer diff`
compares the latest checkpoint with its exact base World. Each first captures the
Layer's workspace, if any (see "Daemonless checkpoint"). `--revision N` selects a
retained active revision.
After discard, the default diff reads the last active revision. Plain output
uses `A`, `M`, and `D` with JSON-quoted paths. JSON includes exact before and
after entries, including file mode, kind, and content object IDs. It is a path
diff, not a textual patch.

Format 3 repositories can also contain published Layers. `state` reports the
published World, operation ID, and final head. `history` retains the active
revisions followed by the terminal publication record. Diff and clone continue
to use the final immutable checkpoint. New native discard operations on a
published Layer return `layer_published`. Its workspace directory stays in place
but is no longer captured. Further edits require a new Layer.

`layer clone` starts a separate Layer from the specified immutable checkpoint.
Later edits to either Layer cannot change that source checkpoint. Creating or
cloning again with the same ID and snapshot returns the original revision 0,
even if the Layer subsequently advanced or was discarded. Read `layer state`
to find its current state.

## Collision radar

`javelin layer radar ID` reports, before Publish, the paths that would make the
active Layer's Publish conflict. It uses Publish's exact rule: a collision is the
same path, or an ancestor or descendant path, changed on both sides, and only
when the other side's entry actually differs from this Layer's base. Siblings in
the same directory and name prefixes (`src` and `src2/a`) never collide.

- `radar.world` holds World advances since the Layer's base: `version` is the
  current head and `paths` are exactly the paths Publish would report as a
  conflict now.
- `radar.layers` lists each other live (unpublished, undiscarded) Layer that
  would make this Publish conflict if it published first. Each entry has
  `layer` and `paths`, a sorted list of `{ours, theirs}` pairs: this Layer's
  changed path and the other Layer's colliding path.

```sh
javelin layer radar my-layer --repo /path/to/project --json
# {"ok":true,"command":"layer radar","clear":false,"radar":{"head":{...},"base":3,
#  "world":{"version":5,"paths":["src/lib.rs"]},
#  "layers":[{"layer":"other","paths":[{"ours":"src","theirs":"src/new.rs"}]}]}}
```

The radar is a warning, not a failure: it exits 0 and sets `clear`. It captures
the Layer's workspace first, like other `layer` commands. `checkpoint` adds the
same `radar` object to its JSON, and warning lines to its text when not clear, so
the agent lifecycle hook surfaces future conflicts as soon as they exist. A Layer
that is not active refuses with `invalid_input`.

Cost is O(the Layer's changed paths × path depth) index lookups plus the
collisions found. It does not grow with repository size, history or the number of
live Layers: at 100k files, the median radar took 0.14 ms with 10 live Layers and
0.17 ms with 1,000 (`tests/radar_bench.rs`, release build, Apple Silicon). Only
Layers in this repository store are seen; the hosted World will serve the same
report across machines.

## Provenance and trace blame

Every checkpoint records who and what produced it, as declared by the harness:

| Field | Flag | Environment | Meaning |
| --- | --- | --- | --- |
| actor | `--actor human\|agent` | `JAVELIN_ACTOR` | Kind of actor |
| agent | `--agent NAME` | `JAVELIN_AGENT` | Agent or model name, e.g. `claude-code/claude-opus-5-5` |
| session | `--session ID` | `JAVELIN_SESSION` | Harness session ID |
| reference | `--reference REF` | `JAVELIN_REFERENCE` | Optional tool-call or prompt reference |

A flag overrides its variable. Flags are accepted by `checkpoint` and `layer stack`;
every other command that captures a workspace uses the variables, so a harness
sets them once for the agent's shell. An agent name implies `actor=agent`; a
session or reference alone implies `human`; a human has no agent name. Each value
is 1 to 256 bytes without control characters, otherwise `invalid_input`. With
nothing declared, checkpoints are unattributed. Provenance is caller-declared,
not authenticated identity.

Publish carries each path's provenance into World history: a path is attributed
to the checkpoint that last changed it in its Layer, so a Layer shared by several
sessions keeps each session's paths apart.

```sh
javelin blame src/lib.rs --json
# {"ok":true,"command":"blame","blame":{"path":"src/lib.rs","entry":{...},
#  "origin":{"version":{"number":7,...},"layer":"fix-parser",
#  "checkpoint":"…","provenance":{"actor":"agent","agent":"claude-code",
#  "session":"s-42","reference":"tool-7"}}}}
javelin blame src/lib.rs --lines
#     7 claude-code session s-42 ref tool-7      1| fn parse() {
```

- `blame PATH` reports the last World version that changed `PATH` at or before
  `--world N` (default: the head): its Layer, checkpoint and provenance. `entry`
  is null when that change deleted the path; `blame` is null when the path has no
  history. It is one indexed read, independent of history length.
- `blame PATH --lines` attributes every line of the file. JSON has `origins` (in
  World order) and `lines` of `{number, origin, text}`, where `origin` indexes
  `origins`. It walks only that path's own versions with a line diff.
- A rename is a delete plus an add: the new path's lines start at the renaming
  version. Stack attributes imported changes to whoever ran Stack. A version
  advanced without a Layer has no checkpoint (`no Layer` in text).

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
- `publish ID --operation ID` captures the directory, then publishes the whole Layer
  (see "Whole-Layer Publish").
- `recover` finishes interrupted steps and reclaims all trash. Every other
  workspace command reclaims at most 4,096 entries.

JSON reports `clone.strategy` (`directory_clone`, `snapshot`, `file_clone` or
`copy`), `clone.fallback` (why faster strategies failed), `clone.copied_bytes`,
the base `source` (`cached`, `derived` or `materialized`) and separate base and
Layer timings. Plain output prints the strategy, and a copy adds a warning that
unchanged data was duplicated. A World whose paths collide on the volume (case,
Unicode normalization, or Windows reserved names) fails with `unsupported`.

## Stack

`layer stack TARGET --revision N --source HASH --operation ID` delegates to
`javelin_stack::stack_layer`. HASH is an immutable source checkpoint, not a live
Layer name. N selects the exact retained target head. Read both from checkpoint
output or `layer history`; keep those inputs and the operation ID for retries.
Compatible edits, including separated changes within one text file, combine.
Structural or overlapping edits return explicit conflicts without changing the
native Layer or World. The CLI never resolves a conflict for its owner.

Stack changes only native state. The target's workspace directory does not receive
the stacked work, and capture never reverts it (see "Daemonless checkpoint").
Refreshing a directory from a stacked head is not implemented yet, so a Layer whose
head came from Stack cannot be published through `workspace publish` until then.

Stack success and stale retries return the original native outcome even after
later edits or Publish. Reusing an operation ID with another target revision or
source hash returns `operation_id_reused`; use a new ID for new work. Successful
Stack retains frozen source ancestry in the target checkpoint. Later source
edits cannot change that identity or the selected bytes. Conflicts reserve no
operation ID. A changed resolution must be checkpointed and supplied explicitly.

JSON Stack output includes `target`, `source`, `operation`, and either the native
`update` or explicit `conflicts` with path, kind, and base/ours/theirs entries.
Plain output reports the outcome and immutable selection. Merge admission/load failures use
`stack_merge_error` on stderr with the adapter's resource/limit or loader message;
native storage failures retain their native error codes. Both return exit 1.
Conflict and stale Stack outcomes return exit 3 on stdout, with `ok: false`.

## Whole-Layer Publish

`workspace publish` captures the Layer's directory, then publishes the entire Layer
onto the World. There is no path selection:

```sh
javelin workspace create work
printf 'hello\n' > .javelin/layers/work/hello.txt
javelin workspace publish work --operation work-publish-1
javelin history
javelin layer state work
javelin layer publication --operation work-publish-1
# Retry an uncertain result with exactly the same operation and attribution:
javelin workspace publish work --operation work-publish-1
# Continue in a new Layer from the current World:
javelin workspace create next
```

Publish refuses rather than leave edits behind. Files that keep changing during
capture return `busy`; a Layer whose head came from Stack returns `invalid_input`
(see "Stack"). Ignored untracked files stay in the directory and are not published.
Edits made after the capture, while Publish runs, are not part of it. A Layer with
no workspace directory publishes its latest checkpoint.

The default attribution is human and requires no trace. For agent work use
`--actor agent --context HASH` to reference an existing native Blob, or
`--actor agent --trace FILE` to store exact opaque bytes through the native Blob
API. Trace files are read into memory; this is not a streaming importer. No
interpretation, identity authentication, hosted permission, or credential is
invented. Missing agent context refuses with `missing_agent_trace` (exit 1);
retry with explicit `--allow-missing-trace` to accept a durable
`missing_agent_trace` receipt warning. Human attribution rejects agent options.
The override is refused with `--context` or `--trace`, so a receipt records it only
when a trace was actually missing.
Optional repository checks are not mandatory defaults; this local command
supplies no additional policy guard.

JSON Publish output contains the complete native `receipt`, including expected
head, attribution, locked World, warnings and outcome. Plain output reports the
World or conflict/stale result, exact checkpoint, and warnings. Conflicts and
stale receipts return exit 3 with `ok: false` on stdout; World and private edits
are retained. Conflict paths are JSON-quoted in plain output. Compatible World
advances compose using native rules; intersecting edits never silently clobber.
There is no refresh or conflict-resolution command yet.

Reuse the same operation, Layer and attribution after uncertainty. A completed
retry returns the original receipt, even after later checkpoints or World
advances. Changed trace bytes, context, actor or override with a used operation
returns `operation_id_reused`. Keep raw trace bytes unchanged or retry using its
receipt context hash. A refusal without a durable receipt reserves no operation.
`layer publication --operation ID` inspects that original receipt on every
platform; unknown IDs return success with `receipt: null`. Conflict/stale
inspection retains exit 3. Receipt inspection never publishes.

After success the Layer is published: its directory stays readable, later edits
are not captured, and further work needs a new Layer (`workspace create`, or
`layer clone --checkpoint HASH` then `workspace open`). Publishing it again under
a new operation ID records a `layer_stale` receipt.

## Retry and discard

Reuse the same Layer ID and explicit `--world N`, or the same clone ID and
checkpoint hash, after an uncertain create or clone. If World advances between
attempts, an omitted `--world` would select a different snapshot and be refused.
For Stack, discard and Publish, choose a unique operation ID before the first
attempt and reuse it after uncertainty. Changed input with a used ID returns
`operation_id_reused`. Capture generates its own operation IDs (see "Daemonless
checkpoint").

`workspace discard` is terminal: the directory moves to `.javelin/trash`, including
uncheckpointed edits that capture could not record. World history and immutable
checkpoints remain inspectable and cloneable. `workspace recover` finishes
interrupted discards and reclaims all trash.

`layer discard ID --revision N --operation ID` performs native discard against
the exact retained active revision N. It returns `stale` if the head advanced.
Use it for Layers without a workspace. It leaves an existing directory in place;
`workspace discard` also moves it to trash.

## Daemonless checkpoint

`javelin checkpoint ID` captures a plain Layer directory: the Layer's clone-backed
workspace when it exists, otherwise the current directory (`--path DIR` selects
another). It is the agent lifecycle hook: run it at turn end or tool-call end. It
works on every OS and needs no mount, helper or background process.

```sh
javelin checkpoint my-layer --repo /path/to/project --json
```

Every `layer` and `workspace` command that names a Layer first
captures that Layer's workspace directory, if it has one, so it reads current
state. JSON output then includes the same `capture` object, and text output starts
with a `Captured workspace edits` line when a checkpoint was recorded. Because the
capture can add a revision, `layer stack` and `layer discard` with an older
`--revision` return stale (exit 3); read `layer state` and retry with the new
revision. `workspace discard` uses the latest head, so uncaptured edits are kept in
history. A capture error, such as a special file in the directory, fails the
command.

Stack changes only native state, so after `layer stack` the target's directory
lacks the stacked work. Capture never reverts it: `checkpoint` refuses with
`invalid_input` and other commands skip the capture with `capture.behind: true`.
Refreshing the directory from a stacked head is not implemented yet.

A per-Layer stat cache under `.javelin/capture/` lets a rescan hash only entries
whose size, mtime, ctime, inode or mode changed, plus racy entries modified within
2 seconds of the scan. When the directory differs from the latest checkpoint, the
command records a new checkpoint under a generated operation ID derived from the
expected head and the changes. A retry, or a concurrent identical capture, replays
that operation instead of adding history. When nothing differs it prints
`unchanged: true` and records nothing. JSON output adds
`capture: {operation, update, hashed, uncertain, rebuilt, behind}`. `uncertain` lists files that kept
changing during three read attempts; they keep their prior checkpointed state and
are hashed again next time. A stale head exits 3 like other mutations.

Symlinks are recorded as links, never followed. The executable bit is read on Unix
and carried over from the prior checkpoint on Windows. Deletes and renames become
native path changes; a case-only rename is a delete plus an add. Empty directories
are not represented. FIFOs, sockets and devices, and names outside the native path
model, are refused with `unsupported`. Entries named `.javelin` are never captured.
### Ignore rules

Capture reads the root `.javelinignore` of the Layer directory. Each nonempty line
is a literal, case-sensitive root-relative native path, for example `build/`,
`packages/ui/dist/` or `private.env`. A path matches itself and all descendants;
the optional trailing slash has the same meaning. Blank lines and lines beginning
with `#` are ignored. Use LF or CRLF. Leading/trailing whitespace, absolute paths,
`.`/`..` components, globs (`*`, `?`, brackets), negation (`!`) and backslash
escapes are errors. There are no nested rule files, automatic imports or external
VCS semantics. The file must be a regular UTF-8 file of at most 64 KiB with at
most 256 rules; missing means no rules. A symlinked rule file is refused. An
invalid rule file refuses with `invalid_input` and changes nothing.

Rules hide untracked paths only: tracked files under an ignored path, their
executable bits, symlink targets and deletions are still captured, so editing
rules never silently untracks an entry. `.javelinignore` itself is always
captured. Capture never removes ignored bytes. The same matcher is available to
library callers as `javelin::IgnoreRules`.

## Output and exit status

`--json` prints one JSON object. Successful results and stale outcomes go to
stdout, with no stderr. Failures have no stdout and retain the existing stderr
shape `{"ok":false,"error":{"code":"...","message":"..."}}`.

Exit status is 0 for success (including unknown receipt lookup), 1 for runtime or storage failure including
`unsupported` and `busy`, 2 for invalid input, and 3 for a conflict or stale mutation/receipt. Stale
output has `ok: false` and the native `stale` outcome. Partial recovery has
`ok: false`, exit 1, and the full recovery report on stdout.

The existing `init`, `history`, and `check` syntax and JSON fields are unchanged.
No command invokes or interprets Git; CI runs every test with a failing Git
stand-in first on `PATH`.
