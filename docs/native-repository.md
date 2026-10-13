# Native repository foundation

## Initial baseline

Before implementation, `rtk cargo test` passed with zero tests and
`rtk cargo run -- init` printed `Hello, world!` without creating a repository.
`rtk cargo test --test scaffold` then passed one process test that asserted those
exact stdout bytes and an empty stderr. This baseline is captured in the scaffold
commit; the test will become a native initialization test after implementation.
The host uses rustc 1.93.0 and cargo 1.93.0 on macOS.

## Bounded outcome

This PR proves immutable native content, manifests, initial World, ordered
history, and compare-and-advance publication metadata. It adds init, history,
and integrity inspection commands. Layer creation, checkpointing, sparse
filesystem access, Stack, refresh, permissions, policies, and complete Layer
Publish are later work. Runtime and tests never invoke or interpret Git.

## Usage and shape

```text
javelin init [project-directory] [--json]
javelin history [project-directory] [--json]
javelin check [project-directory] [--json]
```

Init creates empty immutable World 0, preserves existing project files without
ingesting them, and reopens an existing native store without replacing history.
The hub accepted this boundary on 2026-10-13. Existing-project adoption through a
Layer is required later. The optional directory defaults to the current directory;
commands do not search ancestor directories.

The caller prepares bytes and a complete manifest, then requests metadata advance:

```rust
use javelin::{Repository, RepoPath, FileEntry, Manifest, OperationId, VersionNumber};
let mut repo = Repository::init(project)?;
let blob = repo.put_blob(&[0, 255, 42])?;
let manifest = Manifest::new([(RepoPath::parse("src/data.bin")?,
    FileEntry { content: blob, executable: false })])?;
let candidate = repo.put_manifest(&manifest)?;
let result = repo.compare_and_advance(&OperationId::parse("attempt-1")?,
    VersionNumber::new(0)?, candidate)?;
```

Domain values have private validated fields. `ObjectId` is a kind-tagged SHA-256
identity; `RepoPath` is a relative UTF-8 slash path; `VersionNumber` is a bounded
ordinal in this repository; `OperationId` is a stable ASCII retry key. File entries
carry a content ID and executable flag. A manifest owns sorted paths with no
duplicates or file/descendant collisions. Encoding is compact JSON with sorted
keys and fixed field order, checked by exact re-encoding. Raw bytes never pass
through text decoding. Blob and manifest readers enforce the requested kind.

`src/domain.rs` owns validation and encoding. `src/repository.rs` owns SQLite,
graph verification, history and transaction boundaries. `src/main.rs` owns CLI
parsing and output. The public methods are init/open, put/read blob, put/read
manifest, exact manifest bytes, version/history, compare-and-advance, and check.
SQL connections and storage rows remain private. Callers do not coordinate locks
or sync operations.

## Storage decision

Architect compared two structurally distinct sketches before implementation.
The external-object sketch used durable no-replace file installation, directory
sync, SQLite metadata, and pending operation binding. The embedded sketch stored
objects and metadata in one SQLite transaction boundary. An initial Claude runner
failed with an API rate limit; Codex Luna replaced it. Astra supplied the external
sketch and Sol cross-judged both. Model-family diversity was reduced by that failure.

The judge scored embedded 13/15 and external 12/15 on correctness, recovery, API
simplicity, concurrency proof, and bounded scope. The parent selected embedded
for its single recovery boundary and smaller implementation. External installation
would require unverified platform operations. We retained its durable stale
outcomes and explicit lost-acknowledgement tests. Neither proposal's score proves
runtime behavior; the tests below provide the evidence.

The chosen layout is `.javelin/repository.sqlite3` plus SQLite's transient rollback
journal. Three tables hold immutable typed objects, contiguous World versions, and
immutable operation outcomes. The greatest version number is the head. Canonical
manifest bytes are the only entry representation; no second head pointer, manifest
entry table, or commit object needs synchronization. This removes redundant data
from the embedded candidate. World versions and operation IDs are repository-local.

Preparation stores immutable objects without World references. Final advance uses
`BEGIN IMMEDIATE`, validates existing history and every candidate object inside
the transaction, compares the head, and writes the operation outcome plus any new
World together. An operation's complete input is its expected ordinal and manifest
ID. Both accepted and stale outcomes retain the exact observed result on retries,
even after later advances. Reusing the ID with other input fails. Invalid candidates
do not create operation records; the same input can be retried after missing content
is supplied. A new input after validation failure is allowed because no terminal
operation outcome exists. This differs from the external sketch's pending binding
and avoids a separate transaction/state without weakening acknowledged outcomes.

Reads of graphs hold one read transaction. SQL triggers reject updates/deletes of
immutable records; read-time hashing and graph validation detect tampering when
those triggers are bypassed. Triggers are protection against accidental mutation,
not a security boundary against a repository owner editing the database.

SQLite uses DELETE journaling, synchronous EXTRA, foreign keys and a five-second
busy timeout. On macOS fullfsync is enabled. EXTRA synchronizes the journal's
directory after deletion, as described in [SQLite's synchronous documentation](https://www.sqlite.org/pragma.html#pragma_synchronous).
Init also syncs the repository and project directories on Unix. SQLite locking
and sync behavior depend on the local filesystem and hardware; see
[SQLite's atomic commit assumptions](https://www.sqlite.org/atomiccommit.html).
On non-Unix hosts, parent-directory syncing remains unimplemented and power-loss
durability is unconfirmed. No platform power-loss claim follows from process-kill tests.

## Recovery and accepted tradeoffs

An empty interrupted native directory/database can resume initialization. An
unrecognized database, directory symlink, or unexpected repository-directory file
is rejected without replacement. Genesis and schema commit together, so a process
crash exposes no partial World. SQLite recovers its journal; Javelin never deletes
it. A storage error around commit can leave the result unknown. Reopen and retry
the same operation ID and input to discover the durable result.

We accept whole-object buffering, full manifests, serialized SQLite writers and
full history verification in exchange for a small correctness boundary. Large
repository performance is unmeasured. Prepared but unreferenced content is retained;
garbage collection is later work. Symlink/special-file snapshots, empty-directory
preservation, platform checkout name compatibility, traces, filesystem backends,
and Layer operations remain outside this foundation.

## Implementation reconciliation

The hub replaced the embedded candidate's initial file capture with empty World 0.
The parent removed redundant head/manifest tables and commit hashes, retained
executable flags, and used canonical JSON rather than a bespoke binary codec.
These choices preserve byte identity and ordering and keep the API sketch's
responsibility assignments intact. The independent judge required final in-transaction
candidate validation, graph snapshot reads, exact retry binding, kind separation,
and non-clobbering init races; these are implementation and verification requirements.

## Verification evidence

Verified on Darwin arm64, rustc/cargo 1.93.0. Commands run from this worktree:

```sh
rtk cargo test --locked
rtk proxy cargo fmt --check
rtk proxy cargo clippy --all-targets --locked -- -D warnings
rtk proxy cargo build --release --locked
rtk git diff --check
```

The final suite passes 22 tests with one ignored child-process entry point and
no failing tests. Formatting, Clippy with warnings denied, release build, and
diff whitespace check pass. The independent Sol reviewer ran the preceding
21-test suite and found no reproducible high/medium correctness gap. The added
test proves rollback after a storage insertion error and an exact retry after
reopen; the parent ran the full final suite. No reviewer assertion substitutes
for these observed runs.

- `tests/native_repository.rs` proves exact one-MiB binary/empty content and
  canonical manifest bytes after reopen, kind separation, invalid paths and
  duplicate/ancestor collisions, absent/corrupt proposed and accepted content,
  malformed manifests with valid hashes, immutable SQL mutation refusal,
  non-clobbering init, durable accepted/stale input binding, and storage-write
  failure rollback. Unix tests also reject storage symlinks.
- `tests/support/process.rs`, compiled into the library test binary, launches
  independent OS processes behind pipe barriers. Eight simultaneous initializers
  see one genesis. Eight different publishers produce one accepted result and
  seven durable stale results. Eight identical retries append once. Different
  inputs with one ID produce one acceptance and one identity error. A reader
  sees zero new versions/outcomes while the writer blocks before commit; killing
  it allows a clean exact retry. Kill after commit but before delivery retains
  World 1 even after World 2, and a fresh-process retry returns World 1. Kill
  after the acknowledged result also retains the exact outcome. Init survives
  kills on both sides of commit. There are no artificial sleeps. Test hooks are
  absent from the production library and CLI.
- `tests/cli.rs` drives the actual CLI in temporary projects and proves structured
  and ordinary output, restart, file preservation, corruption failures, exit 1
  for repository errors, and exit 2 for usage errors. Successful JSON goes to
  stdout; errors go to stderr with `ok: false` and a stable error code/message.

The release binary was also driven outside the test suite:

```sh
rtk proxy target/release/javelin init /tmp/javelin-native-smoke.eSisDq --json
rtk proxy target/release/javelin history /tmp/javelin-native-smoke.eSisDq --json
rtk proxy target/release/javelin check /tmp/javelin-native-smoke.eSisDq --json
rtk proxy target/release/javelin init /tmp/javelin-native-smoke.eSisDq
```

Init and history returned World 0 with empty-manifest ID
`2169214789bd2b417d915c470c6d8d4de146200334bd19930a24ebce709e763e`.
Integrity reported one World, one object, and zero operation outcomes. An existing
file containing bytes `00 ff 2a` stayed unchanged after both init calls, checked
with Python byte equality. This fixture is temporary; `tests/cli.rs` reproduces it.

`rtk proxy rg -n 'Command::new|std::process|sleep|[.]git|\bgit\b' src tests`
found only the test launches of the built Javelin CLI and current test executable.
Production code starts no processes. Runtime/tests do not inspect Git data.
The release binary also contains no `JAVELIN_TEST_BOUNDARY` control string,
checked by reading its bytes with Python.

The bounded result is VERIFIED for macOS process restart and native behavior.
Power-loss tests were not performed; SQLite documentation and process kills do
not prove hardware flush behavior. Linux and Windows were not exercised here;
the hub's separate cross-platform CI PR will run the same source tests on actual
runners. Non-Unix parent-directory sync is still unimplemented. Filesystem backend
and normal Layer editor/compiler behavior were not tested in this workstream.
No throughput, scale, network-filesystem, or GA platform support claim is made.
