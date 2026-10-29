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
javelin init [project-directory | --repo DIR] [--json]
javelin adopt [project-directory] --operation ID [--json]
javelin adopt PROJECT --repo DIR --operation ID [--json]
javelin history [project-directory | --repo DIR] [--json]
javelin check [project-directory | --repo DIR] [--json]
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
javelin layer refresh ID
javelin layer update ID [--as NEW]
javelin layer discard ID --revision N --operation ID

javelin workspace create ID [--world N]
javelin workspace open ID
javelin workspace path ID
javelin workspace refresh ID [--resolutions FILE]
javelin workspace discard ID --operation ID
javelin workspace publish ID --operation ID [--actor human|agent]
    [--context HASH | --trace FILE] [--allow-missing-trace]
javelin workspace recover

javelin attach https://HOST[:PORT]/REPOSITORY [project-directory]
javelin sync
javelin status
javelin request open ID --layer LAYER --operation OP
javelin request revise ID --revision N --operation OP [--layer LAYER]
javelin request comment ID --body TEXT --operation OP [--revision N]
javelin request approve ID --revision N --operation OP
javelin request show ID [--revision N]
javelin request list
javelin request publish ID --revision N --operation OP [--base WORLD]
```

All `checkpoint`, `layer`, `workspace`, `request`, `sync` and `status` commands accept
`--repo DIR` and `--json`; `init`, `history` and `check` take the directory as an argument or
as `--repo DIR`. The default
repository is the current directory, with no ancestor search.
`--allow-missing-trace` is a flag. Other options take separate values
and may appear before or after positional arguments. Unknown, repeated, and
irrelevant options are errors. Use `./name` for a path that starts with `-`.

`init` creates an empty immutable World 0. Existing project files remain untouched
and untracked. It does not adopt an existing project; use `adopt` for that.

`adopt` creates the repository and publishes the directory's files as World 1 through
the reserved Layer `adoption-<hash>`, named from the operation ID. World 0 stays empty,
as `init` defines it. Root
`.javelinignore` applies, and `.git` is never read or imported. The project directory
is never written. Later edits reach a World only through a Layer, such as
`workspace publish`. Retry an interrupted adoption with the same `--operation ID`; a
completed adoption replays its receipt for that ID. Any other existing repository, or
an adoption under a different ID, is refused (exit 2) with no change. Files that change
during capture stop the adoption before Publish, so nothing is published from a
moving directory. `adopt PROJECT --repo DIR` imports into DIR, an attached repository,
instead: see "Publish in an attached repository". Layer capture is unchanged: a `.git` directory that appears inside a
Layer is still captured. `layer create` uses the
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
published Layer return `layer_published`. A successful `workspace publish` reclaims
the Layer's directory, and a retry or the next workspace command finishes an
interrupted reclaim. Further edits require a new Layer.

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
the base `source` (`cached`, `advanced`, `derived` or `materialized`) and separate base and
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

Owners resolve conflicts with `--resolutions FILE`: a JSON object from each conflicting
path to `"ours"`, `"theirs"`, or `{"file": PATH, "executable": BOOL}` for supplied bytes.
`ours` keeps the target's entry and `theirs` takes the source's entry, where an absent
entry means removal. Choices are recorded as one immutable resolution object that the
Stack binding names. Retrying with the same file replays the original outcome. A
different resolution under the same operation ID is `operation_id_reused`. A choice for
a path that does not conflict is `invalid_input`. Collisions and aliases are never
resolved by keeping both paths.

Stack changes only native state. The target's workspace directory does not receive
the stacked work, and capture never reverts it (see "Daemonless checkpoint").
`javelin workspace refresh ID` brings the directory to the stacked head. It three-way
merges local edits against the head and writes only paths the head changed. Local edits
that do not touch those paths are kept, and the next capture includes them. When a local
edit and a head change touch the same path or a file and directory collide, refresh
reports `refresh_conflict` with the paths and changes nothing. A path both sides renamed to different names is a conflict on both destinations. A local file that blocks head paths below it, or a local spelling that holds a head path's name on this volume, lists both sides: keeping the local side cancels the head writes it blocks, and taking the head removes it. Each path is replaced by
an atomic rename, so an interrupted refresh leaves whole files and running refresh again
converges. Refresh does not capture first. Afterwards `workspace publish` publishes the
stacked work with the local edits.

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

Publish refuses rather than leave edits behind. When the directory conflicts with
the head, Publish returns `refresh_conflict` and publishes nothing. Resolve the paths
with `workspace refresh ID --resolutions FILE`, then publish again.

Files that keep changing during
capture return `busy`; a Layer whose head came from Stack returns `invalid_input`
(see "Stack"). Ignored untracked files are not published, and a successful Publish
reclaims the directory, so they are deleted with it.
Edits made after the capture, while Publish runs, are not part of it. A Layer with
no workspace directory publishes its latest checkpoint.

The default attribution is human and requires no trace. For agent work use
`--actor agent --context HASH` to reference an existing native Blob, or
`--actor agent --trace FILE` to store exact opaque bytes through the native Blob
API. Keep a trace file outside the Layer: a successful Publish reclaims the
directory, so a retry that reads a trace from inside it fails with `io_error`.
Retry with the receipt's `--context HASH` instead. Trace files are read into memory;
this is not a streaming importer. No
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
`workspace refresh` reports conflicts and changes nothing. There is no
conflict-resolution command yet; resolve the paths by editing the directory.

Reuse the same operation, Layer and attribution after uncertainty. A completed
retry returns the original receipt, even after later checkpoints or World
advances. Changed trace bytes, context, actor or override with a used operation
returns `operation_id_reused`. Keep raw trace bytes unchanged or retry using its
receipt context hash. A refusal without a durable receipt reserves no operation.
`layer publication --operation ID` inspects that original receipt on every
platform; unknown IDs return success with `receipt: null`. Conflict/stale
inspection retains exit 3. Receipt inspection never publishes.

After success the Layer is published and its directory is reclaimed: the files
leave the workspace, `workspace open` returns `layer_published`, and `workspace path`
reports `exists: false`. Further work needs a new Layer (`workspace create`, or
`layer clone --checkpoint HASH` then `workspace open`). Publishing it again under
a new operation ID records a `layer_stale` receipt.

## Publish in an attached repository

In an attached repository the node's World is the World. `workspace publish` uploads the
Layer to the node as `request open` does (created there over the remote World its base holds,
then the blobs and changes the node lacks) and publishes it there with `PublishLayer`. The
local World never moves on its own: afterwards the clone follows the node to its head, as
`javelin sync` does, so it is at the new World and `sync` has nothing to refuse. The Layer is
then closed here: its directory is reclaimed, and `layer state` reads it as discarded, since
the World that published it is the node's.

The node publishes directly when the caller's role and the repository's rules allow it:
Maintain, on a repository that requires no approvals. Otherwise nothing is published and
the command exits 1 with `insufficient_role` (Read or Review) or `review_required` (approvals
required), naming `javelin request open ID --layer LAYER --operation OP`. The Layer keeps
its directory, so it can be proposed as it is. A token without the Publish scope gets
`forbidden`. These refusals are outcomes, so a queued Publish they answer leaves the outbox
instead of waiting there. Retrying the same operation replays the refusal; once the rules
allow it, publish with a new operation.

`javelin adopt PROJECT --repo DIR --operation ID` imports the project directory into the
attached repository DIR as one Publish under the same rules, through the reserved
`adoption-<hash>` Layer that `adopt` uses. Root `.javelinignore` applies, `.git` is never
read, and PROJECT is never written. A first import needs a World that holds no files, so it
only adds them; for a repository with files, use a workspace. A retry with the same ID
delivers or replays the first attempt. JSON adds `project` and `files`.

The Publish goes through the outbox (D7). JSON reports `published` (the node's `number`, `id`
and `tree`), `head` (the node's head the clone followed to), and `closed`. While the node is
down the Publish is queued, exit 0 with `queued: true`, when this clone knows the node's head
for the Layer (from its uploaded captures, see Streaming checkpoints, or a request opened
from it here); `javelin sync` delivers it, follows the node and closes the Layer. A Layer the
node has not seen from this clone cannot be queued, as its captures cannot, and is refused
with `node_unavailable`. A conflict with the node's World exits 3
with `paths`: `javelin layer update LAYER` moves the work onto the node's head as a successor
Layer (see "Moving a Layer onto a newer World"); edit out any markers and publish that. Retry
an uncertain result with the same operation.

### World numbers in an attached repository

Every command in an attached repository shows the node's World numbers and IDs, so clones
of one repository agree on them: `history`, `workspace create` and `path` (`Base World N`),
`status`, `layer radar`, `checkpoint`, `sync`, the Publish above and `request` output. `--world N` on
`layer create` and `workspace create` is the node's number, and must be a World this clone
holds. The local store keeps its own sequence, one World per attach or sync, and records the
remote version each one holds in that World's operation (`attach-<number>-<id>`,
`sync-<number>-<id>`), written in the same transaction as the advance, so the map is as
crash-safe as the World. `history` lists the Worlds this clone holds, each with the node's
`number` and `id`, its `tree`, and its local number under `local`; a World published here by
an older client, which the node lacks, shows `number: null`. Local World IDs are not the
node's: attach keeps a bounded cache, not the history before it, so the local chain cannot
hash to the node's IDs. Outside an attached repository nothing changes.

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
Run `javelin workspace refresh ID` to bring the directory to the stacked head.

A Stack on the attached node moves the node's head only. `javelin layer refresh ID`
reads that head and, when the local Layer is behind it, makes it the local head as a
Stack result, then refreshes the Layer's directory if it has one. The directory is not
captured first, so uncaptured edits are kept. JSON reports `status` (`current` or
`imported`), the node's `seq` and `head`, the local `head`, and the `workspace` refresh
report. A local Layer that holds captures the node lacks is refused with
`layer_diverged`, and a repository with no `attach.json` with `invalid_input`.

A Layer follows its owner across devices. The node keeps a writer lease per Layer: the first
device to write to it holds it, and writes from any other device are answered `layer_stale`
without changing anything. `javelin layer take ID [--operation OP]` moves the lease to this
device, then brings the Layer here: it is created over the node's base if this repository
lacks it, moved to the node's head as `layer refresh` does, and its directory is opened, or
refreshed when it exists. The base is found by its tree, not its number, since this
repository numbers its Worlds from its attach; a newer base is fetched first, and an older one
this repository lacks is refused with `invalid_input`. The node is the World's authority, so
the fetch never overwrites local divergence: when this repository's World head is not the
attached head (a clone that published locally with an older client), the take is refused with
`local_world_diverged`, naming both heads; take the Layer from a fresh `javelin attach`
directory. A take that stopped after the World followed the node but before
`attach.json` recorded it is finished by the next take or sync: a head made by
`sync-<number>-<id>` that the node confirms (same chain ID and tree) is written to
`attach.json` first. The fetch, the divergence check and that recovery are `javelin sync`'s
own (see Sync below). Those checks, and the refresh of an
existing local Layer, run before the lease moves. A retried `--operation OP` replays the
recorded take; JSON `holds_lease` and `lease` (`this_device`, `another_device`) say who holds
it now, and the text says plainly when another device does. Only the Layer's owner can take the lease (`forbidden` otherwise),
and a token limited to another Layer cannot. Without `--operation` a fresh ID is used, which
is safe: taking a lease this device holds changes nothing. JSON reports the node's `seq` and
`head`, whether the Layer was `created`, the refresh `status`, and the directory `root`.

The old device's queued appends then replay into `layer_stale`. They stay in the outbox with
that outcome: `outbox replay` exits 1 with `code: layer_stale` and lists them under `stale`
(Layer, operation, the node's head, and the local head), and `outbox status` lists them too.
Keep their work with `javelin layer clone NEW --checkpoint LOCAL_HEAD`.

### Streaming checkpoints

In an attached repository every capture that records a checkpoint, from `checkpoint` or from
a `layer` or `workspace` command, uploads it to the node: the blobs and changes object the
node lacks, then a `LayerAppend` after the node's head. The records go into the outbox first,
then the queue is delivered in journal order within a budget of 2 seconds
(`JAVELIN_UPLOAD_BUDGET_MS` changes it). A node that is down, slow or unreachable leaves them
queued; the capture still succeeds with its usual exit code, and the next capture,
`javelin sync` or `javelin outbox replay` delivers them. A queued Publish or revision stops
that delivery, since `sync`, `outbox replay` and `request` report their outcomes. JSON adds
`capture.upload`: `{"status": "uploaded"}`, or `status` `queued`, `layer_stale`,
`not_queued`, `local` or a node refusal such as `forbidden`, with a `detail` saying what to
do; text output adds an `Upload` line. A repository with no `attach.json` adds nothing.

The clone keeps what it knows of each Layer on the node in `.javelin/uploads.json`: the
node's head once the queued appends land, which `Repository::foreign_capture` names before
the node answers, and the journal position it was learned at. The head is read from the node
the first time a Layer uploads, after `layer take`, `layer refresh`, `request open` and
`revise`, and again after an append the node did not accept. Only then is the writer lease
read: a device that does not hold it queues nothing and reports `layer_stale`, and its
checkpoints stay local until `javelin layer take` moves the lease back or `layer clone`
keeps them. A Layer that is new to the node and captured while the node is down cannot name
its parent yet, so its upload is not queued (`not_queued`); `javelin sync` and the next
capture upload it. A Layer whose base is not a World fetched from the node stays local.

Uploads follow the path audiences (#99). A Layer never holds a hidden entry, so an upload names
plain content only. The node refuses an append that changes a path outside the writer's
audiences with `forbidden`; that refusal decides the record, so it does not hold up the
queue, the checkpoint stays local, and the next capture tries again from the node's head.
The bytes of such a path may already have reached the node as the writer's own upload; the
node serves them to nobody outside the path's audience.

`javelin status` reads, without contacting the node, where each active Layer's checkpoints
are: `uploaded` (the node holds the latest), `queued` with a count, `not_uploaded`,
`layer_stale`, a refusal code, or `local`. JSON lists them under `layers` with the Layer's
`revision`, `checkpoint` and `base` (its World, in the node's numbers), beside the `attachment` and the outbox's `pending` count.

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

## Public frontier

```text
javelin frontier --node https://HOST[:PORT] --repository ID [--json]
javelin delay [DURATION] --node https://HOST[:PORT] --repository ID [--json]
javelin embargo --operation ID [--until UNIX] --node https://HOST[:PORT] --repository ID [--json]
javelin embargo release VERSION --node https://HOST[:PORT] --repository ID [--json]
```

A reader outside a hosted repository sees it at its public frontier: the newest version
older than the disclosure delay with no unreleased embargo at or below it. Members see the
head (`docs/direction.md`, Superposition).

`frontier` prints the frontier. A member also sees the delay and each unreleased embargo;
anyone else sees only whether the public view is delayed. It reads with this device's
credential for the repository or `JAVELIN_TOKEN`, and signed out without one.

`delay` shows the delay, or sets it with Maintain. `DURATION` is seconds, or a count
followed by `s`, `m`, `h` or `d`; `0` turns the delay off. Each version becomes public on
its own once the delay has passed since it was published. A version already public stays
public when the delay is raised.

`embargo --operation ID` embargoes the Publish with that operation ID, with Maintain. Send
it before the Publish: readers outside the repository then stay below the version it makes
until Unix time `--until`, or until `embargo release VERSION`, and every version waiting
behind it becomes public at once (each still after the delay). Repeating the same embargo is
safe; a Publish already decided cannot gain one. Changes print the disclosure as a member
sees it. A refusal exits 1 with the node's code.

## Hosted repositories: sync and Publish Requests

`javelin attach` fetches a hosted repository's head into an empty repository and records
it in `.javelin/attach.json`. Like a clone, it makes the project directory when it is missing,
once the node has answered. From then on the node is the World's authority, and commands
show its World numbers (see "World numbers in an attached repository"). A Layer created over
one of those Worlds can be published to the node or proposed to it.

Credentials come from `javelin login` (the device's sign-in, refreshed through the control
plane) or `JAVELIN_TOKEN`. Reads of a public repository's Worlds (attach, sync) need
neither, and show a caller without a role the public frontier, not the head (see Public
frontier). Layers and Publish Requests, `request list` and `request show` included, need
a role in the repository.

### Sync

`javelin sync` brings the local World to the node's head:

1. It replays the outbox first, so a queued revision or Publish reaches the node before the
   fetch.
   When nothing is left stalled, it then uploads every active Layer whose latest checkpoint
   the node lacks and no capture queued, as a capture would (see Streaming checkpoints).
2. It fetches the versions after the attached head, verified from that head's ID (the trust
   anchor), so the work follows the number of new versions, not the length of history. A
   node whose chain does not pass through the attached head is refused with
   `chain_mismatch`, and nothing is written. Hidden entries arrive as attach brings them:
   the ones the node opens for this token with their content, the rest as commitments left
   out of the rolling base.
3. It stores the tree chunks and blobs it lacks, advances the local World and the rolling
   base, and rewrites `attach.json` last. A sync interrupted after the advance is finished
   by the next sync or `layer take`, once the node confirms the head the advance names.

Layers keep their bases, and Publish composes them onto the node's head. Sync then lists the
node's requests: it keeps each current revision's base for a Publish queued later (see
`request publish`), and retires every active Layer here that proposed a request the node has
published (see "Retired Layers" below). JSON reports `from` and `head` (remote heads),
`advanced`, `local_head`, `base`, `outbox` (`delivered`, `stalled`, `pending`), `uploads`, one
entry per Layer it uploaded (`layer`, `status`, `detail`), and `retired`. A record that still cannot be delivered
returns exit 1 with its `code`, after the fetch. A node that cannot be reached is
`node_unavailable`, and nothing changes.

Sync never replaces local history. The local head must be a World that attach or sync took
from the node, holding the attached tree, with no Publish receipt. `workspace publish` in an
attached repository publishes on the node, so only a clone that published locally with an
older client moved away from the attachment; sync refuses it with `local_world_diverged`
and exit 1, before the outbox replays. A delivered direct Publish closes its Layer, as
`workspace publish` does; JSON lists those Layers under `closed`. The message names the published Layer's last checkpoint:
`javelin layer clone NEW --checkpoint HASH` makes a Layer from it, and `javelin request open`
proposes that Layer to the node. To follow the node again, attach a fresh directory.

### Publish Requests

A Publish Request proposes one Layer's work to the node. It has one author, and each
revision is an immutable checkpoint of the author's Layer on the node. An approval applies
to exactly one revision; a new revision needs new approvals.

- `request open ID --layer LAYER --operation OP` captures the Layer's workspace, uploads its
  changes to the node (creating the Layer there over the remote World its base holds, then
  the blobs and changes it lacks), and opens request ID with them as revision 1. Request IDs
  have 1 to 128 ASCII letters, digits, `-`, `_`, `.` or `:`.
- `request revise ID --revision N --operation OP` submits the Layer's current changes as
  revision N + 1. N is the current revision, so a revision prepared against an older one is
  refused with `stale_revision`. Only the author revises (`not_author`). The Layer is the
  one the request was opened from here; `--layer` names another.
- `request comment ID --body TEXT --operation OP [--revision N]` comments on the request, or
  on revision N.
- `request approve ID --revision N --operation OP` approves exactly revision N. The author
  cannot approve their own request (`self_approval`), and approvers need the Review or
  Maintain role.
- `request show ID [--revision N]` prints the request, every revision with its base,
  checkpoint and approvals, the approvals Publish needs, the comments, and the paths
  revision N (default: the current one) changes, each marked `A` (added), `M` (modified) or
  `D` (deleted) against the revision's base. JSON has the same fields, with each path's
  `change` (`added`, `modified`, `deleted`). The base's tree is read from the root down to
  each path, from chunks this clone holds, else from the node, which serves this token only
  what it may read; a path under a hidden entry is `changed`, since whether the base had it is
  hidden too. A revision whose checkpoint changes a path this token may not read shows
  `checkpoint: null` and no paths, as the node treats that checkpoint: never stored.
- `request list` prints every request on the node with its Layer, author, state, current
  revision and that revision's base.
- `request publish ID --revision N --operation OP` publishes exactly revision N's
  checkpoint, not the Layer's later head. The node checks at Publish that N is current and
  has enough approvals under the current permissions: `stale_revision` (exit 3),
  `not_enough_approvals` (exit 1). A conflict with the World returns exit 3 with `paths`.
  The base is the revision's. This clone remembers it from the last `request show`, `request
  list` or `sync` that saw the revision, and from its own `open` and `revise`, in
  `.javelin/request-bases.json`; otherwise it is read from the node. So a Publish queues while
  the node is down without `--base WORLD` (a remote number from `request show`), which is
  needed only when this clone has never seen the revision. After a Publish, the Layer this
  clone proposed the request from is retired (see "Retired Layers").

Every write takes an operation ID. A retry with the same ID returns the original outcome,
including after a lost reply; the node refuses the same ID with different input as
`operation_id_reused`.

What waits for the node follows the outage rules (`docs/online-design.md`, D7):

| Command | Node unavailable |
| --- | --- |
| `request revise`, `request publish`, attached `workspace publish` and `adopt --repo` | Queued in the outbox: exit 0 with `queued: true` and `code: node_unavailable`. `javelin sync` or `outbox replay` delivers it. |
| `request open`, `approve`, `comment`, `show`, `list` | Refused with `node_unavailable` (exit 1). Nothing is queued. |

A queued revision names the checkpoint the node will record for it, computed from the
node's Layer head this clone last saw, which `request open` and `revise` keep in
`.javelin/requests.json`. If another device moves that Layer on the node in the meantime,
the queued revision is refused with `missing_object` when it is delivered; run
`javelin layer refresh LAYER` and revise again. Retrying a queued command with the same
operation ID delivers what was queued, unchanged; later edits need a new operation ID.

Refusals print the node's code: `not_author`, `self_approval`, `stale_revision`,
`unknown_revision`, `not_enough_approvals`, `not_open`, `already_approved`,
`unchanged_revision`, `insufficient_role`. A direct Publish adds `review_required` (see
"Publish in an attached repository"). `request open` and `revise` also refuse with
`unresolved_conflict` (exit 1) while a file `layer update` marked still holds markers, as
does a direct `workspace publish`.

### Moving a Layer onto a newer World

`javelin layer update ID [--as NEW]` moves a Layer's work onto the node's current World, after
a Publish conflict or whenever the author wants the newer World under it. It captures the
Layer's directory, brings the local World to the node's head as `sync` does (refused with
`local_world_diverged` in a repository that published locally), and three-way merges the
Layer's work onto that World with the merge Stack uses. A Layer's base never changes, here
or on the node, so the result is a successor Layer over the node's head, `ID.2` (then `ID.3`,
and so on; `--as NEW` names it), whose first checkpoint holds the merge and whose directory is
opened. The old Layer is then retired as `workspace discard` retires one: it leaves `layer
radar`, its directory goes to trash, and its checkpoints stay in `layer history`.

A region of a text file both sides changed is written into the successor's file between
markers, with the rest of the file merged:

```text
<<<<<<< Layer ID
the Layer's lines
||||||| remote World 2
the base's lines
=======
the World's lines
>>>>>>> remote World 3
```

Other conflicts (binary files, a path one side deleted, a file and a symlink) keep the Layer's
side. Every conflict is listed by path with its kind and how it was written; the command then
exits 3 with `ok: false` and `code: conflict`, and exits 0 when the merge was clean. Resolve
by editing the files; the next command for the Layer captures them. `request open`, `request
revise` and a direct `workspace publish` refuse with `unresolved_conflict` while a marked file
still holds a marker line. A
conflict that cannot be written at all, because the paths would collide as a file and a
directory whichever side is kept, refuses with `invalid_input` and changes no Layer. A Layer
already over the node's head reports `status: current` and changes nothing.

`request revise ID` without `--layer` follows the request's Layer to its latest successor, so
the request is revised from the updated Layer as it is:

```sh
javelin layer update greet             # conflict in hello.txt; work is in Layer greet.2
$EDITOR .javelin/layers/greet.2/hello.txt
javelin request revise greet-1 --revision 1 --operation ada-revise-2
```

The update is recorded in `.javelin/updates.json` before the successor is made. Each step
repeats safely, and the old Layer is retired only after the successor's directory is open, so
a crash at any point loses nothing: running the same `layer update ID` again finishes that
update against the same World, and running it after it finished reports it again. If the old
Layer changed between the crash and the retry, the retry refuses and leaves both Layers.

JSON reports `layer`, `status` (`updated` or `current`), `successor`, `from` and `base`
(remote heads), the successor's `head` and directory `root`, `retired`, and `conflicts`
(`path`, `kind`, `written`: `markers` or `layer_side`).

### Retired Layers

A published request's work is in the World, and a published Layer is read-only
(`build-contract.md`). So after `request publish` succeeds, and after a `sync` that sees the
request published, each Layer here that proposed it is retired: the request's Layer, the one
this clone last revised it from, and their `layer update` successors, whichever is active
here, on every device that holds one. A Layer counts only when its history holds one of the
request's revisions, since another person's Layer may share its ID. Its directory is captured
first, so edits made after the published revision stay in its history, then the Layer is
discarded and its directory trashed and reclaimed. It leaves `layer radar`, and `layer history`
still reads it. JSON lists each under `retired` with `later_work: true` when it held work
after the published revision; `javelin layer clone NEW --checkpoint HEAD` keeps that work. A
Layer published directly with `workspace publish` is closed by that command instead (see
"Publish in an attached repository").


### Worked example

Ada authors and Ben maintains; each is signed in on their own machine with `javelin login`.

```sh
# Ada: attach, work in a Layer, and propose it.
javelin attach https://node.example.com/acme-vault ~/vault
cd ~/vault
javelin workspace create greet
printf 'hello\n' > .javelin/layers/greet/hello.txt
javelin request open greet-1 --layer greet --operation ada-open-1

# Ben: attach a second clone, review, and comment on revision 1.
javelin attach https://node.example.com/acme-vault ~/vault
cd ~/vault
javelin request list
javelin request show greet-1          # revision 1 changes hello.txt
javelin request comment greet-1 --revision 1 --body 'Say it louder' --operation ben-c1

# Ada: revise. Ben's approval, had he given one, would not carry over.
printf 'HELLO\n' > .javelin/layers/greet/hello.txt
javelin request revise greet-1 --revision 1 --operation ada-revise-1

# Ben: approve exactly revision 2, publish it, and sync.
javelin request approve greet-1 --revision 2 --operation ben-approve-2
javelin request publish greet-1 --revision 2 --operation ben-publish-2
javelin sync                           # World 1 is now this clone's head
javelin workspace create next          # next/hello.txt reads HELLO

# Ada: sync too.
javelin sync
```

If the node is down when Ada revises, the revision is queued and the command exits 0 with
`queued: true`; her next `javelin sync` delivers it. Ben's approval and comment, by
contrast, wait until he can reach the node.

When a Publish conflicts, the World changed the same paths after the revision's base. The
author runs `javelin layer update LAYER`, resolves the markers it writes, and submits the
result with `javelin request revise ID --revision N --operation OP`. A revise from a device whose Layer was
taken by another one (`javelin layer take`) is refused with `layer_stale`, and the message
names the `javelin layer clone NEW --checkpoint HASH` that keeps that device's work.

### The whole day, end to end

[`tools/acceptance/daily-workflow.sh`](../tools/acceptance/daily-workflow.sh) runs the daily
loop against any node URL and prints each command with its output: two people, two clones,
an import, a request reviewed and revised (the old approval stops counting), a node restart
with a queued Publish, a conflict and its recovery, `layer take` to a second device, and every
clone synced to the same World with identical files. Its header lists the credentials and
restart hooks it takes. `crates/javelin-cli/tests/daily_workflow.rs` runs the same loop,
and the script itself, with the real `javelin` and `javelin-node` binaries.

A self-hosted setup from nothing:

```sh
export JAVELIN_CONTROL_DIR=~/javelin-control
javelin control init                       # prints the admin token once
javelin control serve 127.0.0.1:7979 &     # the approval page and the control plane's API
export JAVELIN_CONTROL_URL=http://127.0.0.1:7979
javelin login                              # approve: JAVELIN_ADMIN_TOKEN=... javelin control approve CODE
javelin repo create acme/app --operation create-app --visibility private --publish requests
javelin-node serve ~/javelin-control/nodes/REPOSITORY REPOSITORY 127.0.0.1:7878 &
javelin attach http://127.0.0.1:7878/REPOSITORY ~/app
```

`repo create` prints the `javelin-node serve` line with the repository's ID. To bring in a
teammate, invite them, and they sign in with the code:

```sh
javelin member invite acme/app cora --role review   # prints a one-time code, valid 7 days
javelin login --invite CODE                          # on cora's machine: signs in as cora
javelin repo settings acme/app --required-approvals 1
```

### Members

A repository's members and their roles (`read`, `review`, `maintain`) live in the control
plane, which pushes every change to the repository's node. The node reads its permissions
for each request, so a change decides the member's next request, with no restart, on a
self-hosted node and on Cloudflare alike. REPOSITORY is `OWNER/NAME` or the repository's ID.
The caller is this device's sign-in, or `JAVELIN_ADMIN_TOKEN`; changes need the maintain role,
and any member may list.

- `member list REPOSITORY` prints the members, the epoch the node applies, the approvals
  Publish needs, and pending invites.
- `member add REPOSITORY ACCOUNT [--role ROLE]` grants a role (default `read`) to an account
  the control plane already has: one an invite made, a passkey's, or `admin`. An unknown name
  is `not_found`; invite them instead. Adding someone who already holds that role changes
  nothing.
- `member role REPOSITORY ACCOUNT ROLE` changes a member's role.
- `member remove REPOSITORY ACCOUNT` removes a member. Their tokens stop working at the node
  with the next request (`forbidden`), and the control plane gives them no new ones. Work they
  queued while the node was down is refused when it replays (`forbidden`) and stays in their
  outbox; nothing of it reaches the node.
- `member invite REPOSITORY ACCOUNT [--role ROLE]` prints a one-time code for a new account
  named ACCOUNT. `javelin login --invite CODE` makes the account, signs that device in as it
  and grants the role. A name that already has an account is refused (`conflict`): add it
  instead. A new invite for the same name replaces the pending one, and `member remove`
  cancels it. On a Cloudflare node, which only a maintainer's control plane reaches (with
  `JAVELIN_NODE_ADMIN_TOKEN`), the invite still makes the account and the membership, and
  `login --invite` says the node has not applied the role yet; the maintainer's next
  `member add` for that account pushes it.
- `repo settings REPOSITORY --required-approvals N` sets the approvals a Publish Request needs.

A repository always keeps one `maintain` member, so the last one cannot be removed or
lowered. Each change moves the epoch on by one; a command whose push did not reach the node
is saved, and running it again pushes it. A role change moves which audiences a member reads
and whether they read the head or the public frontier; it never changes a version ID or
where the frontier is.

A second device signs in with `javelin login`, approved from a device already signed in to
the same account with `javelin control approve CODE` (no admin token).

## Output and exit status

`--json` prints one JSON object. Successful results and stale outcomes go to
stdout, with no stderr. Failures have no stdout and retain the existing stderr
shape `{"ok":false,"error":{"code":"...","message":"..."}}`.

Exit status is 0 for success (including unknown receipt lookup and a queued revision or
Publish), 1 for runtime or storage failure including `unsupported` and `busy` and for a
node's refusal, 2 for invalid input, and 3 for a conflict or stale mutation/receipt. Stale
output has `ok: false` and the native `stale` outcome. Partial recovery has
`ok: false`, exit 1, and the full recovery report on stdout.

The existing `init`, `history`, and `check` syntax and JSON fields are unchanged.
No command invokes or interprets Git; CI runs every test with a failing Git
stand-in first on `PATH`.
