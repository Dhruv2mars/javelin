# Repository protocol message codec

Status: bounded initial protocol direction. Not hosting acceptance.
Source base: accepted native Layer state `e56fc86e3415217de2b960a7ea0dbe6d2eb450b1`,
an unmerged dependency. This unit cannot claim independent acceptance.

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
copied, without serde visitor or nesting-scanner machinery. It needs no dependency
beyond `javelin`. Blob payloads travel raw, which fits the contract's "compact
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
public `ObjectId::parse`. Clean EOF at a frame boundary ends a session. EOF anywhere
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
| `7f` | Error | code (len `u8`, ASCII), detail (len `u16`, UTF-8, ≤ 1024) |

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

`tests/alloc.rs` uses a counting allocator to show that each refusal allocates less
than 4 KiB. Blobs above 8 MiB cannot transfer in version 1; streaming or chunking
needs a new version.

These limits bound protocol buffers only. Native reads of objects that are already
stored are not bounded. Want probes read whole objects. A sender reads each source
blob before checking its size. A receiver repository that holds a large blob can
therefore be made to allocate that blob's size by a small Want. A manifest probe holds
roughly twice the manifest's bytes, the decoded manifest, and the largest blob it
references. The receiver refuses duplicate Want items, so one frame cannot probe the
same object repeatedly. Repeated frames and many distinct objects still cost a full
read each. This is retained API gap 3; the public API has no bounded read or existence check.

## Receiver rules

- Want must not repeat an item. Each item is probed with the typed public readers. Only `missing_object` for that exact ID
  counts as missing. A wrong-kind or corrupt object refuses the session.
- Blobs are stored with `put_blob`. The returned ID must equal the claim, otherwise
  `id_mismatch`.
- Manifests are rebuilt with `RepoPath::parse`, `FileEntry` and `Manifest::new`, then
  stored with `put_manifest`. That call refuses missing and wrong-kind blob references,
  so blobs must arrive first. The returned ID must equal the claim.
- `expected` is the World head the caller compares against at publication. It is not
  necessarily a Layer's or revision's original base. Composing nonintersecting work
  onto a newer head, and binding Layers, revisions, authorization or provenance to a
  Publish, are later versioned integration work. This command claims none of them.
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

## Evidence

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
  claimed for blob bytes, a mismatched manifest claim that is never publishable,
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
  decode, frame header and count refusals each allocate less than 4 KiB.
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

## API gaps for the hub

These gaps were worked around through the public API only. Nothing private was copied.

1. No public content-ID function. The receiver must store bytes before learning their
   ID, so a mismatched claim leaves an unreferenced immutable object. It is never
   referenced or published, but nothing collects it. A public
   `ObjectId::for_blob(&[u8])` and `ObjectId::for_manifest(&Manifest)` would allow
   comparison before storing.
2. No raw byte conversion for `ObjectId`. The codec converts through lowercase hex.
   `ObjectId::from_bytes([u8; 32])` and `as_bytes()` would remove that detour.
3. No public existence check or bounded read. Want reads whole objects, and
   `manifest_bytes` also reads every referenced blob to revalidate the graph. Review
   r1 reproduced a 55-byte session that caused a 32 MiB allocation for a stored blob.
   A typed `has_blob`/`has_manifest`, or a size-bounded read, is needed to close this.
4. Native `VersionNumber`, `RepoPath`, `OperationId`, `WorldVersion` and
   `AdvanceOutcome` are serialize-only, as intended. This crate keeps its own
   validated DTO.

## Known limits

Every limit below is retained explicitly; none is claimed as solved.

- Every object must fit in one frame. Batches may hold many objects up to the frame
  and count limits. There is no streaming, resumable transfer or large-file support.
- Want probe memory and work are bounded only by stored object sizes and the number
  of distinct objects requested (API gap 3). Rate limiting is transport work.
- Whole-manifest transfer only. There is no delta or history negotiation, and no
  multi-version fetch or clone.
- No transport security, authentication, authorization, rate limiting or quotas.
  Receiver CPU and storage abuse within limits is unbounded across sessions.
- Unreferenced objects from refused sessions are retained.
- Version 1 is not yet frozen for external compatibility. Interoperability beyond this
  crate's own sender and receiver is untested.
- Hosting, provider coordination, notifications, Publish Requests, permissions and
  GA remain unverified.
