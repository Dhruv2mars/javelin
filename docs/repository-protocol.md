# Repository protocol message codec

Status: bounded codec and reference sender/receiver. Not hosting acceptance.
Native dependency is hub-accepted exact
`3a54021b15ac5002c21f492b6bed07740c682421` from PR #18, now merged into main.
PR #15 targets main. Original planning source was
accepted Layer state `e56fc86e3415217de2b960a7ea0dbe6d2eb450b1`; historical evidence
below retains its original source labels.

## Bounded outcome

`crates/javelin-protocol` defines version 1 of a provider-independent message codec,
a reference receiver, and a reference sender over the public native `Repository` API.
It covers version negotiation, have/want, bounded blob and manifest batches, and
Publish as `{operation, expected, manifest}` returning the exact advance outcome.

Out of scope: HTTP, TLS, authentication, permissions, policy, public endpoints,
provider deployment, notifications, checkpoints, traces, Publish Requests and every
collaboration format. Runtime and tests never invoke or interpret Git.

## Caller API

```rust
use javelin_protocol::{push, serve, Outcome};
// Receiver: one session over any byte stream pair.
serve(&mut destination_repo, reader, writer)?;
// Sender: transfers what the receiver lacks, then publishes.
let outcome: Outcome = push(reader, writer, &source_repo, manifest_id, &operation, expected)?;
```

`javelin-protocol serve <project>` and
`javelin-protocol push <project> <manifest> <operation> <expected>` expose the same
calls on stdin/stdout. Push reports `accepted <n> <manifest> <operation>`,
`stale <actual>` or `error <code> <detail>` on stderr. Carrying the bytes between the
two processes is the transport's job; tests relay them as a network would.

`codec::{encode, decode, read_frame, write_frame}` and `Message` are public for
transports and tests. `ProtocolError { code, detail }` carries a stable string code.

## Format decision

Two architects designed structurally distinct candidates against the same brief
and public API. Candidate A (Codex `gpt-6.1-sol`, high) used length-prefixed JSON
bodies with base64 blobs, serde, bounded visitors and a nesting pre-scan. Candidate B
(Claude `claude-opus-5-5`, medium) used a hand-written binary body with raw IDs and
blob bytes. Self-scores were A 22/25 and B 18/25; A scored debuggability higher.

The parent selected B. Every limit is a direct integer compare before a field is
copied, without serde visitor or nesting-scanner machinery. The binary codec needs
only `javelin`. The sender uses `serde_json` to reconstruct the native canonical
manifest after a capped native read. Blob payloads travel raw, which fits the contract's "compact
binary messages where useful". Corrections taken from A: Want names object kind,
the outcome is a separate DTO, limits are smaller, and errors close the session.
Error codes are strings so new native `Error` variants pass through unchanged.

`tests/size.rs` encodes one fixture both ways: 200 blobs of 2048 bytes and a
200-entry manifest. Binary blobs take 416,805 bytes against 563,231 for JSON with
base64; raw content is 409,600. The binary manifest takes 11,831 bytes against
31,907 for JSON and 28,387 for native canonical bytes. The JSON figures come from
building candidate A bodies in the test; no candidate A implementation exists.
Weaker debuggability is accepted. Exact tag tables and round-trip tests document
the format.

## Wire format, version 1

A frame is a `u32` big-endian body length (1..=16 MiB), then the body. The first
body byte is the tag. Integers are big-endian. IDs are 32 raw bytes, parsed through
public `ObjectId::from_bytes`; encoding uses `as_bytes()`. Clean EOF at a frame boundary ends a session. EOF anywhere
else is `truncated`. A body that ends inside a field or has trailing bytes is `malformed`.

| Tag | Message | Body after tag |
|---|---|---|
| `01` | Hello | `"JVLP"`, min `u16`, max `u16` |
| `02` | HelloOk | version `u16` |
| `10` | Want | n `u32`, n × (kind `u8`: 1 blob, 2 manifest; id) |
| `11` | Missing | same as Want; the subset the receiver lacks |
| `20` | Blobs | n `u32`, n × (id, len `u32`, bytes) |
| `21` | Manifests | n `u32`, n × (id, e `u32`, e × (path len `u16`, UTF-8 path, entry `u8`, id)) |
| `22` | Stored | count `u32` |
| `30` | Publish | operation (len `u8`, ASCII), expected `u64`, manifest id |
| `31` | Outcome | `0`, number `u64`, manifest id, operation; or `1`, actual `u64` |
| `7f` | Error | code (len `u8`, UTF-8), detail (len `u16`, UTF-8, ≤ 1024) |

Error-code lengths count bytes. Encoding refuses codes above 255 bytes.
Non-ASCII codes such as `é` round-trip. Version 1 remains unfrozen;
this documents existing behavior rather than freezing external compatibility.

Manifest entry kinds are 0 regular, 1 executable regular, and 2 symlink. Paths must
be strictly ascending by bytes, which matches the native sorted map and rejects duplicates.

The first message must be Hello. The receiver answers HelloOk 1 when 1 lies in
`[min, max]`, otherwise `unsupported_version` with the supported range. Any other
first message is `unexpected_message`. Every refusal is sent as Error, then the
receiver closes the session.

## Limits

Fixed version 1 limits are 16 MiB per frame, 8 MiB per blob, and 1,024 blobs, 4,096
Want items or 16 manifests per batch. Manifests may hold 65,536 entries, paths 4,096
bytes, and path depth 256 segments.

The codec enforces each limit at its boundary:

- `read_frame` checks the frame length from the header before allocating a buffer.
- `decode` refuses bodies above the frame limit on entry.
- Counts are checked against their limit before decoding. Collections grow only as
  elements decode, so decode memory stays proportional to the received frame.
- Lengths are checked before bytes are copied.
- `encode` computes the size before it allocates the output: exact for every message
  except Error, where it reserves the maximum detail length. It checks every count,
  length, depth and the total first.

`tests/alloc.rs` uses a counting allocator to show that its four specific oversize
encode, decode, frame-header and count fixtures each allocate less than 4 KiB.
This is not a bound for every refusal. A legal 16 MiB outer frame containing an
oversized inner blob allocates the 16 MiB frame buffer before refusing the blob.
The outer-frame allocation stays within the wire budget. Blobs above 8 MiB cannot
transfer in version 1; streaming or chunking needs a new version.

Want uses `has_blob` and `has_manifest`, which stream kind and byte-hash
verification with fixed Rust working memory. Manifest availability does not decode
canonical bytes or inspect references. The sender uses `read_blob_bounded` with an
8 MiB cap and `manifest_bytes_bounded` with a 16 MiB encoded-byte cap. The latter
checks its own canonical representation without reading graph payloads. It does
not return a typed manifest, so the sender reads `BTreeMap<String, FileEntry>` from
those already-validated bytes and reconstructs through public `RepoPath::parse` and
`Manifest::new`. No private hashing or decoder implementation is copied, and no
legacy full-read fallback remains.

An encoded-byte cap is not a parser heap, RSS or CPU limit. Native canonical parsing
and public reconstruction allocate maps, paths and entries. Want still hashes all
stored bytes, with work linear in size, and repeated requests repeat that work.
SQLite uses a separate C allocator that the Rust counters do not measure.

Historical hub Codex audit r2 on `02c7427` reproduced a 55-byte Hello-plus-Want
session causing a 33,554,432-byte largest Rust allocation for a stored 32 MiB blob.
The eight-reference manifest case caused 268,446,277 cumulative Rust allocation
bytes, not peak RSS, excluding SQLite C allocations. The migrated regression keeps
both cases, plus legacy native-read positive controls. Fresh measurements and
source identity are recorded below; historical measurements are not migration proof.

## Receiver rules

- Want must not repeat an item. Typed availability returns false for a missing
  object. Wrong-kind or byte-hash corruption refuses the session. A present manifest
  is not a certificate of canonical encoding, graph validity or readiness to publish.
- Blob IDs are computed with `ObjectId::for_blob` before `put_blob`. A mismatch
  refuses with `id_mismatch` before the mismatched bytes are stored.
- Manifests are rebuilt with `RepoPath::parse`, `FileEntry` and `Manifest::new`.
  `ObjectId::for_manifest` must equal the claim before `put_manifest`, which still
  validates all referenced content, including missing/wrong-kind/corrupt blobs.
- A refused batch does not publish. Its valid prefix may remain as immutable
  objects, and later valid requests may reference them. There is no batch rollback
  or retention cleanup. Pre-store checks prevent storing the mismatched item.
- `expected` is the World head the caller compares against at publication. It is not
  necessarily a Layer's or revision's original base. Composing nonintersecting work
  onto a newer head, and binding Layers, revisions, authorization or provenance to a
  Publish, are later versioned integration work. This command claims none of them.
  Such semantics need a newly negotiated version or message. Appending fields to
  version 1 Publish is incompatible because the decoder refuses trailing bytes.
- Publish parses through `OperationId::parse` and `VersionNumber::new`. It then calls
  `compare_and_advance`, which revalidates the whole graph. Only that call writes a
  World reference. Its result is adapted through `Outcome::from_native`, an explicit
  DTO. No serialize-only native type is deserialized.
- The reference sender trusts no reply. Missing must list only offered objects, each
  at most once. An Accepted outcome must have number `expected + 1`, the requested
  manifest and the requested operation. A Stale outcome must report `actual > expected`.
- Identical replay returns the durable original outcome. The same operation ID with a
  changed expected version or manifest returns `operation_id_reused`. A Publish whose
  reply is lost is retried with identical input in a new session.

## Historical evidence before native access migration

Local, macOS arm64, Rust 1.99.0, `cargo test --locked --all-targets` in
`crates/javelin-protocol`: 21 tests passed, with formatting and Clippy `-D warnings` clean.

Review r1 by Codex `gpt-6.1-sol` (high) on `58fadbf` found no unverified World
reference. It raised one high, three medium and two low findings. All were addressed
in this PR, except the high native-read allocation, which is retained as API gap 3.
Codex's round 2 attempt failed on a provider usage limit before reviewing. Claude
`claude-opus-5-5` (high) ran round 2 on `d2530fe`. It confirmed responses 2 to 6, the
23/23 mutation claim and exact encoder sizing. It found duplicate Want probe
amplification (medium). It also found three low items: a positive control colocated
with the shim, sizing and memory wording, and stale TASKS counts. All are fixed here.
The follow-up round was single-model.

- `tests/process.rs` builds two separate repositories and runs the actual binary as
  separate sender and receiver processes. The test only relays frames. It drops the
  Publish reply after the receiver commits World 1. Two retries in new processes return
  `accepted 1 <manifest> op-1`, transfer no objects, and leave history at two versions.
  Changed input returns `operation_id_reused` and a new stale operation returns `stale 1`.
  An incremental publish sends exactly one new blob. The test also checks a
  version-refusal process.
- `tests/receiver.rs` covers a valid-hash blob claim with other content, a manifest ID
  claimed for blob bytes, a mismatched manifest claim refused without publication,
  wrong-kind Want, manifest-entry and Publish references, a manifest sent before its
  blobs, and unsorted, duplicate, invalid, colliding, unknown-kind and non-UTF-8 paths.
  It also covers oversize frame, count, entry, blob, path length and depth limits,
  header and body truncation, trailing bytes, empty and unknown frames, unsupported
  versions, messages before Hello, and Publish beyond head. Each case asserts the
  refusal code sent to the peer and an unchanged World.
- `tests/sender.rs` runs the reference sender against a scripted malicious receiver.
  It refuses duplicate and unoffered Missing items before sending objects. It refuses
  Accepted outcomes with the wrong number, manifest or operation, and Stale outcomes
  that are not beyond `expected`.
- `tests/alloc.rs` measures allocation with a counting allocator. Oversize encode,
  decode, frame header and count refusals in its four specific fixtures each allocate
  less than 4 KiB. Its legal outer-frame fixture demonstrates allocation of the full
  frame before an oversized inner blob is refused.
- `bun scripts/mutate.ts` removes each of 24 protocol guards in turn. It first requires
  that the mutant compiles, then requires that the suite fails, and it does so for every one. The guards cover:
  - blob and manifest ID comparisons;
  - decoder count, frame, blob, path and depth limits;
  - encoder total, blob and depth limits;
  - path order, trailing bytes and header truncation;
  - required negotiation, version refusal, Want kind separation and duplicate refusal;
  - sender Missing deduplication and subset checks;
  - the Accepted number, manifest and operation checks, and the Stale bound.
  An earlier count-versus-remaining guard was unobservable because preallocation was
  already bounded. It was removed together with preallocation.

Hosted evidence: `.github/workflows/repository-protocol.yml` runs the same checks on
Linux, macOS and Windows. It compiles a native failing `git` shim (`git.exe` on
Windows) and places it first on PATH. A probe in a separate directory runs Rust's
`Command::new("git")`. It must not reach the shim without the PATH prefix, and must
reach it with the prefix. The test step then requires that the shim
never ran. The earlier shell-script shim at `58fadbf` could not intercept on Windows.

Tested source `02c7427ef18bedf933ab4a75bc8d7d46cd9dce3b` passed hosted run
[38508849025](https://github.com/Dhruv2mars/javelin/actions/runs/38508849025) on
ubuntu-24.04, macos-15 and windows-2025 with Rust 1.99.0. I downloaded and inspected
all three log artifacts, which expire 2026-10-28. Each records the exact commit, 21
passing tests, formatting, Clippy and a successful release build. Each job log shows
`probe exit without shim=0 with shim=97`, and the test step confirms the shim never ran.

## Native object access migration

The hub accepted PR #18 source `3a54021` after independent r3 and source/artifact
inspection, recorded in hub `1791618`. Rebase onto that exact source completed at
`d1fb504`. Native/root source, lockfiles and workflows are unchanged by this PR.
Only the protocol crate, its doc/workflow and its appended TASKS section are owned.

This closes the three old API workarounds at the protocol callers:

1. Public `ObjectId::for_blob` and `for_manifest` check claims before storage.
2. `ObjectId::from_bytes` and `as_bytes` replace lowercase-hex conversion.
3. Typed streamed availability replaces Want payload reads; capped native sender
   reads replace the unbounded blob read and full-graph manifest read.

Native `VersionNumber`, `RepoPath`, `OperationId`, `WorldVersion` and
`AdvanceOutcome` remain serialize-only. The protocol retains explicit validated
wire DTOs. The sender's native canonical input uses the already public `FileEntry`
parser, not deserialization of those serialize-only types.

Historical correction fixtures at `c133fab941fec7dc2eecfb81b05e1b1e0db44db4` passed
21 local tests with fmt and Clippy on macOS arm64, Rust 1.99.0. That source proved
retention of mismatched actual bytes under the old API. The migrated fixture now
requires those bytes to be absent, retains the valid-prefix/later-publication
proof, and keeps the legal 16 MiB frame and UTF-8 Error-code behavior. Neither the
old 21 tests nor old 24 compiling mutants count as new proof.

Fresh `tests/object_access.rs` measures the real `serve` and `push` callers with
thread-local Rust allocation totals and largest requests, including zeroed and
reallocation requests. It excludes SQLite C allocation and does not measure RSS.
Legacy native blob and graph reads are positive controls. A 55-byte Want probes a
32 MiB blob and an eight-entry manifest pointing at it; sender fixtures refuse an
over-cap blob and manifest. Fault-injection tests disable only scratch-store
immutability triggers, then require kind/hash refusal and prove that manifest byte
availability cannot bypass final full-graph Manifests or Publish checks.

Updated mutations include restoring legacy Want and sender reads, moving storage
before ID comparison, and the original framing/reply guards. Each mutant must
compile and produce an actual failed test result; a build failure or process crash
does not count. The Oct 16 workflow uses relevant local checks and a regular PR.
It does not require another independent review or artifact-audit round. Historical hosted
results above do not establish the behavior of this migration.

Local migration checks on 2026-10-16, macOS arm64 and Rust 1.99.0: formatting,
Clippy with warnings denied, and all 25 component tests pass. Two release process
tests pass with the failing Git shim: control exits without=0, with=97, and no Git
marker during product execution. Both 55-byte Want fixtures measure 739 cumulative
Rust allocation bytes and a 288-byte largest request, with the large-read controls
passing. These counters exclude SQLite C memory and are not RSS measurements.

Seven relevant compiling mutations are detected by failed assertions:
`bun scripts/mutate.ts 'Want ' 'sender ' 'pre-store' 'encode blob'`.
The paused full mutation run was incomplete. This result does not claim a new
30-case run or reuse the historical 24-mutant result as migration proof.

## Known limits

Every limit below is retained explicitly; none is claimed as solved.

- Every object must fit in one frame. Batches may hold many objects up to the frame
  and count limits. There is no streaming, resumable transfer or large-file support.
- Want uses fixed Rust streaming memory per probe but hashes stored bytes in full.
  Repeated frames, distinct objects and sessions can amplify CPU and I/O work.
- Manifests still uses native `put_manifest` full-graph validation. A small manifest
  can reference large existing blobs and cause payload reads and allocations.
  Publish still validates the graph and complete World/attempt history, including
  on retries. These CPU/memory amplification paths remain unresolved for hosting.
- Capped reads limit encoded bytes only. Manifest parser/reconstruction heap,
  SQLite C memory and total process RSS are not bounded by that cap.
- A legal 16 MiB outer frame reserves its buffer before inner-field refusal.
- Whole-manifest transfer only. There is no delta or history negotiation, and no
  multi-version fetch or clone.
- No transport security, authentication, authorization, rate limiting or quotas.
  Receiver CPU and storage abuse within limits is unbounded across sessions.
- Valid-prefix unreferenced objects from refused sessions are retained. Quotas,
  retention and cleanup remain future work.
- Operation IDs share a repository namespace. Principal binding, permissions and
  operation-principal namespaces are not defined by this codec.
- Version 1 is not yet frozen for external compatibility. Interoperability beyond this
  crate's own sender and receiver is untested.
- Hosting, provider coordination, notifications, Publish Requests, permissions and
  GA remain unverified.
