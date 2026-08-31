# Storage format

## Layout

```text
project/
├── javelin.toml
├── .javelinignore
├── tracked files
└── .javelin/
    ├── store.sqlite3
    ├── objects/
    ├── materialized/
    ├── views/
    ├── conflicts/
    ├── temp/
    ├── locks/
    ├── monitor/
    └── trash/
```

`.javelin/`, `.javelin-view`, and foreign VCS metadata are always excluded. Named views contain a JSON `.javelin-view` marker with format, canonical project path, and Layer ID.

## Object IDs

```text
BlobId = BLAKE3("javelin:blob:v1\0" || raw bytes)
TreeId = BLAKE3("javelin:tree:v1\0" || canonical tree bytes)
```

Object paths use the first two hexadecimal characters as shard directory and the remaining 62 as filename.

Writers hash input before creating a temporary object. Existing content IDs and duplicates already staged in the same scan skip compression and durable writes. A scan stages new compressed objects, synchronizes their files concurrently, atomically renames them into place, then synchronizes every touched shard directory once. Creating a new shard also synchronizes the object-directory root. SQLite state can reference the resulting Tree only after this object batch is durable.

## Object file

Every object file starts with:

```text
4 bytes  magic "JVL1"
1 byte   kind, 1 Blob or 2 Tree
8 bytes  big-endian uncompressed length
rest     zstd stream, compression level 3
```

Readers validate magic, kind, decompressed length, and domain-separated hash. `fsck` streams all object payloads, including unreachable objects and objects listed only in metadata.

## Tree encoding

Tree entries are flat complete path states sorted by normalized UTF-8 path bytes. Encoding:

```text
u32 entry count
repeat:
  u32 path byte length
  path bytes
  u8 kind, 1 file, 2 directory, 3 symlink
  u8 executable flag
  u16 object-ID byte length
  object-ID ASCII bytes
```

Directories have no object ID and no executable bit. Files and symlinks reference Blob objects. Duplicate, unsafe, non-UTF-8, internal, absolute, parent-traversal, and case-colliding paths are rejected.

Javelin tracks bytes, path, kind, empty directories, symlink target, and executable bit. It does not track timestamps, ownership, ACLs, extended attributes, device nodes, sockets, or process state.

## SQLite

Database settings:

- WAL journaling
- foreign keys enabled
- 10-second busy timeout
- full durable synchronization
- transactional schema migrations

Schema concepts:

```text
world, versions
layers, layer_checkpoints, views
contributions, publish_attempts, publish_queue
conflicts, conflict_resolutions
validations, validation_runs, version_validations
provenance_sessions, provenance_events, provenance_attachments
checkpoint_provenance, contribution_provenance
claims, events, discard_records
object_metadata, schema_migrations
```

World pointer changes, Contribution append, target state append, validation links, provenance links, and acceptance event occur in one transaction during Publish.

## Policy files

`javelin.toml` has format version `1`. `.javelinignore` supports comments, ordinary globs, directory globs, and `!` re-inclusion. Exact secret patterns seed new Worlds while `.env.example` remains trackable.

A Layer scans with policy stored in its Synchronized Reference. A policy edit takes effect after acceptance. This makes adding a formerly ignored file a two-Contribution operation.

## Retention

Discarded Layer Checkpoints and reachable objects remain for seven days by default. Raw provenance remains for 30 days by default. `gc` expires claims, purges eligible raw payloads to tombstones, purges eligible discarded Layers, then removes unreachable objects.

Accepted World state, active Layer state, retained Discard state, validation output, Conflict evidence, and unpurged provenance attachments remain reachable.

## Compatibility

Schema migrations are append-only and recorded in `schema_migrations`. Object and Tree domains contain explicit `v1` identifiers. Unknown config, marker, object, provenance, event, and JSON schema versions fail with diagnostics instead of guessing.
