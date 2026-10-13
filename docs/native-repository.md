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
