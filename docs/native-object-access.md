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
and verification. Independent Codex design review `native-object-access-design-r1-06a1093` compared
both candidates and recommended dedicated kind-specific methods to avoid another
public enum. The hub confirmed the object-only guarantee. The separate reviewer
uses the same model family, which limits review diversity.

The signatures proposed before implementation are:

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

`has_blob` and `has_manifest` stream and verify stored kind and byte identity only. It does not
decode manifests, inspect symlink targets or validate referenced objects. A
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
limits are valid. SQLite errors remain storage errors.

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

`tests/native_object_alloc.rs` measures Rust allocation requests, largest request
and net live peak during each call. SQLite's process-wide C allocator counters
are measured separately in this single-test executable. Neither counter includes
all process memory. RSS includes fixture setup, code, stack, allocator retention
and other runtime memory, so it cannot establish a per-operation memory limit.
No total process-memory guarantee follows from these counters.

Implementation, regression review and exact-head three-OS evidence are pending.
This unit does not verify a protocol adapter, hosting, whole-Layer Publish or GA.
