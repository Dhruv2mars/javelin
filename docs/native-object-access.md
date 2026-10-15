# Bounded native object access

This unit starts at exact hub-accepted tested
`8137111cb00a227f0f225ef4debe7ca1bf891e98` from unmerged PR #10. Draft
[PR #18](https://github.com/Dhruv2mars/javelin/pull/18) targets
`feat/linux-layer-workspaces`. It does not independently accept that dependency.
Format 2, full graph validation and existing durable data remain unchanged.

## Design comparison before implementation

Candidate A queries stored kind and byte length as metadata hints. It can refuse
an oversized read before allocating the payload, but a hint can describe damaged
content. A second bounded verification call adds another API and requires callers
to understand when a hint is sufficient.

Candidate B verifies the kind-tagged content hash through SQLite incremental
BLOB reads, with a fixed chunk buffer. It reports presence only after hash
verification. Work still grows with the object's byte length, but Rust payload
memory does not. Bounded byte reads check the stored length before allocating;
bounded manifest reads also decode and check their own canonical bytes.

The selected shape is B. It needs only rusqlite's `blob` feature and no storage
migration. A consistent read transaction covers row metadata, incremental reads
and verification. Independent Codex design review
`native-object-access-design-r1-06a1093` compared both candidates and recommended
dedicated kind-specific methods to avoid another
public enum. The hub confirmed the object-only guarantee. The separate reviewer
uses the same model family, which limits review diversity.

The signatures settled before implementation are:

```rust
impl ObjectId {
    pub fn for_blob(bytes: &[u8]) -> Self;
    pub fn for_manifest(manifest: &Manifest) -> Self;
    pub const fn from_bytes(bytes: [u8; 32]) -> Self;
    pub const fn as_bytes(&self) -> &[u8; 32];
}

impl Repository {
    pub fn has_blob(&self, id: ObjectId) -> Result<bool>;
    pub fn has_manifest(&self, id: ObjectId) -> Result<bool>;
    pub fn read_blob_bounded(&self, id: ObjectId, max_bytes: usize)
        -> Result<Vec<u8>>;
    pub fn manifest_bytes_bounded(&self, id: ObjectId, max_bytes: usize)
        -> Result<Vec<u8>>;
}
```

## Verification boundaries

`has_blob` and `has_manifest` stream and verify stored kind and byte identity only.
They do not decode manifests, inspect symlink targets or validate referenced objects. A
correctly hashed noncanonical manifest, or a canonical manifest with missing or
damaged references, can therefore have verified object bytes. That is insufficient
to publish it. The publisher remains the authority for graph validation.

`read_blob_bounded` verifies the returned blob's kind and hash.
`manifest_bytes_bounded` verifies kind, hash and canonical manifest encoding. It
does not read referenced blobs or check symlink target contents. Its allocations
grow with the capped manifest bytes and decoded entries, never with referenced
blob payloads. The cap limits input byte length, not total allocator usage.

Missing objects return `false` from availability and `MissingObject` from reads.
Wrong kinds and damaged bytes are errors.
An over-limit read returns `ObjectTooLarge` before a payload-sized Rust allocation.
Refusal does not certify the oversized object's hash or graph. Zero and exact
limits are valid. Over-limit refusal precedes hash and requested-kind checks.
Within the cap, hash damage takes precedence over a requested-kind mismatch.
SQLite errors remain storage errors. Each result describes one consistent read
snapshot and does not reserve or authorize a later publication.

Existing `read_manifest`, `manifest_bytes`, `check`, `history`, initialization and
publication keep their existing full graph checks. Successful object-only access
must not conceal corruption from those operations.

`for_blob` and `for_manifest` compute the existing canonical kind-tagged IDs
without storing anything. `for_manifest` hashes canonical bytes of the typed
manifest and does not validate references. Byte conversion copies the digest
without hex parsing; arbitrary digest bytes do not prove an object exists.

## Evidence and memory accounting

The pre-fix 32 MiB blob read on Darwin arm64, Rust 1.93.0, requests a
33,554,432-byte Rust allocation. SQLite's C allocator high-water counter reaches
36,221,280 bytes with 2,568,544 bytes live before the operation. Raw baseline:
`/tmp/javelin-native-object-access/legacy-memory.txt`.

`tests/native_object_alloc.rs` measures the calling thread's Rust allocation
requests, largest request and net live peak during each call. The net counter
starts at zero and includes deallocations during that call; it is not the entire
live heap. The streaming verifier also uses a fixed 64 KiB stack buffer, outside
these heap counters. SQLite's process-wide C allocator counters
are measured separately in this single-test executable. Neither counter includes
all process memory. RSS includes fixture setup, code, stack, allocator retention
and other runtime memory, so it cannot establish a per-operation memory limit.
No total process-memory guarantee follows from these counters. Verification still
reads and hashes every byte of the requested object. It does not bound CPU work
or repeated requests. Manifest parsing and canonical re-encoding can allocate
more than the encoded byte cap.

At exact tested source `3a54021b15ac5002c21f492b6bed07740c682421`, the
following Rust request measurements match on all three hosted OSes:

- Stored 32 MiB blob availability and refusal at an 8 MiB cap: 120 total bytes,
  largest request 64 bytes, net live peak 64 bytes.
- Stored manifest larger than 32 MiB, availability and refusal at an 8 MiB cap:
  the same 120 / 64 / 64 bytes.
- Small manifest with eight references to a 32 MiB blob: bounded canonical read
  allocates 8,209 total bytes, largest request 1,024 bytes, net peak 2,985 bytes.
  The existing full graph read allocates 268,445,738 bytes cumulatively, with a
  33,554,432-byte largest request. Object-only availability uses 120 / 64 / 64.
- An exact 8 MiB blob read allocates an 8,388,608-byte output, with 8,388,728
  total requested bytes. A canonical manifest with a 1 MiB path and a 32 MiB
  reference requests 6,293,989 bytes, largest request 2,097,156 bytes, net peak
  4,195,347 bytes. This demonstrates parser overhead and reference independence.

SQLite C allocation peaks for 32 MiB blob availability are 2,266,064 bytes on
Linux, 2,435,968 on macOS and 2,267,296 on Windows; live baselines are
2,200,464 / 2,337,664 / 2,201,696 respectively. These are observed fixture
counters, not API limits. Cumulative process RSS peaks, including fixture setup
and legacy reads, are 139,348 KiB on Linux and 208,519,168 bytes on macOS.
Windows RSS is not measured. All three OSes measure Rust and SQLite C allocations.

## Source and platform proof

The exact source passes local formatting, warnings-denied Clippy, locked tests
with all targets/features, and a release build on Darwin arm64, Rust 1.93.0.
There are 75 passing tests and one ignored child-process entry point. A standalone
caller compiled against the production release library passes pre-store claim
refusal, raw digest roundtrip, typed availability and bounded reads. Release
binary inspection finds no `JAVELIN_TEST_BOUNDARY` markers.

[Native run 38570590391](https://github.com/Dhruv2mars/javelin/actions/runs/38570590391)
checks out exact `3a54021` and runs Rust 1.99.0. Actual job steps and all eighteen
artifact logs were inspected. Formatting, Clippy, locked tests and release builds
pass on Linux, macOS and Windows, with 75 / 75 / 60 passing tests and 17 / 1 / 1
ignored entries. All nine new API regressions and the allocation test run on every
OS. Linux ignores sixteen gated mount/actor entries plus the repository process
actor. Windows additionally omits ten Unix Linux-binding tests, three Unix helper
tests, storage-symlink rejection and deleted-working-directory behavior. Those
omissions are not Windows acceptance evidence.

Native artifacts are Windows `11898226917`, Linux `11898011943` and macOS
`11896787938`, expiring 2026-10-28 UTC. Each independently downloaded ZIP matches
its host SHA-256 digest; all six files per ZIP match the extracted logs byte for
byte. Raw paths:

- `/tmp/javelin-native-object-access/ci-38570590391/`
- `/tmp/javelin-native-object-access/native-run.json`, `native-jobs.json` and
  `native-artifacts.json`
- `/tmp/javelin-native-object-access/artifact-inspection.json` and
  `archive-verification.json`
- `/tmp/javelin-native-object-access/full-test.log`, `allocation.log`,
  `clippy.log`, `fmt.log`, `release-build.log` and `public-release-caller.log`

The separate inherited
[Linux runtime run 38570590258](https://github.com/Dhruv2mars/javelin/actions/runs/38570590258)
is NOT VERIFIED: 15 of 16 tests pass, but the common probe observes no
`watch-create` event. Its summary records cleanup complete. The failure is
retained under `/tmp/javelin-native-object-access/linux-runtime-38570590258/`
and `linux-runtime-failure.log`. Linux source and workflow are unchanged here;
this does not close the dependency's already inconclusive file-watch behavior.

Final independent source review remains pending. Usage failures in final review
rounds 1 and 2 are not completed reviews; round 3 carries the original brief,
design ruling and partial evidence. The hub alone accepts the tested native SHA.

This unit does not verify a protocol adapter, hosting, whole-Layer Publish or GA.
