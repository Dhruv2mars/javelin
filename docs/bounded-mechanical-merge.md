# Bounded mechanical merge

This unit extends the pure component from PR #12 at `61ecdd9`. It changes no
native storage, Stack, refresh, hosting, or Publish API. Its unmerged native
dependency remains `e56fc86`; this unit does not independently accept that core.

## Grounding and design sketch

`merge` receives three already-loaded native `Manifest` values. It first resolves
exact entry equality, deletion, additions and entry types. Only regular files
whose content changed on both sides call the loader, in sorted path order.
The loader returns an allocated `Vec<u8>`; the engine neither reads storage nor
hashes or decodes native objects. A NUL marks a binary content conflict. The old
line engine uses exact RawMyers matches, copies whole regions from one changing
side, and requires unique anchors kept by every minimal diff between different
sides' edits. It establishes mandatory anchors by removing each candidate base
line and repeating the exact diff. There is no resource outcome in the old API.

Two whole shapes were compared before implementation:

1. Keep RawMyers and reserve its worst-case work, including every repeated diff,
   before calling it. This minimizes matching changes but has a cubic worst-case
   allowance in line count. Tight deterministic bounds refuse ordinary files
   with scattered edits even when they would complete with a different algorithm.
2. Compute one exact prefix LCS table per side, with rolling suffix rows. For base
   row `i`, the best matching that omits it is
   `max_j(prefix[i,j] + suffix[i+1,j])`. The row is mandatory precisely when that
   value is below the full LCS length. Trace back the same prefix table for one
   exact matching. Keep the existing region and unique-separator rule. Reserve
   table cells and a conservative work allowance that includes compared bytes,
   table initialization, row scans, uniqueness and region processing before DP.

The public result is `Result<MergeOutcome, MergeError<E>>`, with distinct
`Load(E)` and `Resource(ResourceLimit)` variants. A refusal returns no partial
outcome and is never a semantic conflict. Fixed published limits cover manifest
entries, individual and aggregate path bytes, individual and aggregate loaded
bytes, per-version line count, peak table cells and aggregate work. Successful
outcomes expose reserved work for evidence. No wall-clock deadline determines
domain behavior. Independent Codex architect `bounded-merge-pr21-architect-r1`,
resumed after the provider limit, recommended shape 2. Its comparison identified
RawMyers compaction and fallback allocation as additional costs shape 1 must
bound. Shape 2 is selected. Fixed limits avoid a configuration API.

The engine can reject a returned oversized blob before scanning it or loading the
next blob, but cannot constrain the loader's earlier allocation, IO, CPU or error
behavior. The same applies to the caller's prior manifest allocation. Later native
bounded object APIs must enforce repository-read limits before allocation. Callers
must also bound concurrent calls and must not persist partial results on refusal.

## Evidence and limits

The limits are inclusive. They count repeated entries/paths in all three manifests
and repeated loader calls, rather than deduplicating them. They also apply when
entry equality would otherwise resolve without loading bytes.

| Resource | Limit | Admission point |
| --- | ---: | --- |
| Manifest entries, all three combined | 4,096 | before union allocation |
| One path, UTF-8 bytes | 1,024 | before cloning/parsing paths |
| Aggregate path bytes | 131,072 | before union allocation |
| One returned blob, logical bytes | 1,048,576 | before scan or next loader call |
| All returned blobs, cumulative | 8,388,608 | before scan or next loader call |
| Lines per loaded text version | 8,192 | before line vectors or DP |
| Peak DP cells | 4,194,304 | before either DP allocation |
| Work units per merge call | 536,870,912 | reserve before matching |

A line includes its exact terminator. A nonempty unterminated tail counts as one
line. Binary detection precedes line matching, after the byte guards. A binary
file within the byte caps remains a semantic Binary conflict. It is never run
through DP. Retained object IDs need no blob load; the caller remains responsible
for limits when it later reads those objects.

The work allowance is a deterministic upper estimate of loop visits and byte
comparisons, not a claim about CPU instructions or milliseconds. Metadata reserves
64 times the sum of (path byte length + 1) squared. With at most 4,096
entries, that covers the bounded BTree comparisons, clones, and parsing each slash
prefix in the structural pass. Each loaded blob reserves 4 times its byte length
plus 32 for scanning and counting. Let n,m,k be line counts and b,o,t byte counts.
For each base/side pair reserve:

    5*(n+1)*(m+1) + 3*min(n*o,m*b) + 4*(n+m+1)

The table term covers initialization, prefix and suffix recurrence, omission scans,
and traceback. The byte term bounds equality in both recurrences and traceback.
Each pair compares a subset of all line pairs; summing shorter-line lengths is at
most either n*o or m*b. Additionally reserve:

    n*(n+m+k+b+o+t) + 16*(n+m+k+b+o+t+1)

This covers the existing repeated uniqueness scans, region equality, line-vector
processing and output copies/growth. Both pair reservations and this allowance
are charged together before either matching table is allocated. After entry/path/byte/line admission,
all formula operands are bounded: lines at most 8,192, bytes at most 1 MiB,
and paths at most 1,024 bytes across at most 4,096 entries. Their u64 products
and sums cannot overflow. Incremental usage sums saturate and refuse on excess. Work and loaded
bytes are cumulative across paths. A refused reservation leaves no partial public
outcome. Usage.work_units reports the reserved estimate, including short-circuited
work, not a profiler count.

Each DP cell is u32, so the prefix table payload is at most 16 MiB. Tables are
computed sequentially, retaining only matches and mandatory bits from the first
side. Two rolling rows cost at most 65,544 bytes at the line cap. Logical loaded
payload in flight is at most 3 MiB; accumulated output is at most the cumulative
loaded-byte allowance because every output region is copied once from a side or
base. Line references, steps and output references are bounded by the line caps;
metadata node counts and cloned strings are bounded by the entry/path caps.
Allocator headers, capacity rounding and caller-owned storage are not counted as
exact RSS. In particular a returned Vec can have capacity far above its length.
The caller must bound that prior allocation and any concurrent merge calls. The
engine bounds its own matching allocation and processed logical payload; it cannot
retroactively bound caller memory or prevent process-wide allocation failure.

Local initial debug/release checks pass 2 unit tests, 8 repetitive/oracle tests,
16 native repository tests and 9 resource tests. Original 20,000 every-pair cases
remain clean in 3,868 cases. Added 30,000 move/swap/rotation cases exercise 22,291
ambiguous matchings and 2,682 clean outcomes, with no oracle skips or weakened
assertions. The three ordinary Rust/config/Markdown fixtures combine and match all
minimal pairs. Mandatory rows and traceback also match independent exhaustive
subsequence masks for all 1,600 pairs of words over three tokens up to length 3.
These are small-input empirical checks, not a universal correctness proof.

Old-source reproduction uses the exact library SHA256
882af7f23031254aa602a273aab10c477ed0670224f6fab69a3203fe3ad29ee0 in
/tmp/javelin-bounded-pr21/old. Release 2,000-line scattered edits took 4,771.514 ms,
with input bytes 18,890/18,890/19,690. The 4,000-line case was externally killed at
30 seconds. No timeout is used as domain behavior. Initial new-source release
2,000-line scattered edits reserve 394,938,298 units and took 56.917 ms on this
Apple Silicon host. 4,000 and 8,000 lines deterministically refuse TableCells
before matching, requiring 16,008,001 and 64,016,001 cells. Moved, repeated and
dissimilar cases exercise the same sizes; their sizes, elapsed time, units and
outcomes are printed by tests/resources.rs. Timing is observational only.

A separate developer-only old/new harness in /tmp/javelin-bounded-pr21/differential
compared all 592,620 changed triplets over four tokens up to three lines. Both
engines had 3,432 clean cases, with identical clean bytes and no newly clean cases.
This is finite compatibility evidence, not general completeness or correctness.
All 29 guard/correctness mutants compiled and tests rejected each. The three
production source modules were restored byte-for-byte. Final local logs at /tmp/javelin-bounded-pr21/local-final verify source at
29abd0e. Debug/release, fmt, Clippy -D warnings, the native example and all 29
compiling mutations pass. Restored library SHA256 is
c84959b77c7ae2e833adb2a0db9d17757c16391212024070b123d3f104f561ed;
resources is 7c288610a7a26118418a41e6c18bf4e1abd60c031f82cb85e09af402d9283da2;
matching is e22509c63576e3605b29f6ba09d0baccfce378e67ed1a9208776e39ed781f140.
Independent implementation review and hosted evidence are recorded below.
General correctness beyond the small oracle is
INCONCLUSIVE. This is not production Stack or a product gate.

## Independent source review

Fresh read-only Codex review `bounded-merge-pr21-source-review-r1` examined
`edc92938f422a26b5b78952c71e6d909603a35b1` in its own exported scratch. It found
no material source findings, so no finding regression or source fix was needed.
The three production modules and committed tests are byte-identical at reviewed
source, local tested source `29abd0e` and hosted tested source `884d51c`.
The parent checked the actual report, harness and logs at
`/tmp/javelin-pr21-review-FgkoAb`, and compared the source bytes directly.

The reviewer independently ran all 35 debug/release tests and Clippy for original
targets. A separate subsequence-mask oracle checked the actual matching module
for 30,770 base/side pairs. Its exhaustive public-API check covered 1,727,880
changed triplets over three tokens through length four, with 6,528 clean outcomes
and 9,264 minimal-script pairs. All clean pairs were independent and produced the
exact emitted bytes and order; there were zero violations or oracle skips.
Injected loader errors at each of the three positions preserved the payload and
stopped the next call. The reviewer rechecked the exact production hashes above
after execution. It did not certify hosted results or rerun mutations.

This is independent execution by a separate Codex agent within the same model
family. It is finite evidence, not a proof for arbitrary files or a claim that all
previously accepted merges remain accepted. Those general claims are INCONCLUSIVE.

## Hosted exact-source evidence

[Mechanical merge run 38613209944](https://github.com/Dhruv2mars/javelin/actions/runs/38613209944),
attempt 1, checked out exact PR head
`884d51c24f584bbb38c08532bb65c58f5276fb3f`. The component tree is
`4eafc41f4eca246a00f8cd85226c22cc34345607`. A later documentation-only
head does not change these tested component bytes. Native Rust run
[38613210052](https://github.com/Dhruv2mars/javelin/actions/runs/38613210052)
also passed its three jobs at the same head. That is compatibility evidence for
this branch, not independent acceptance of the underlying core.

| Runner | Architecture | Job | Artifact |
| --- | --- | ---: | ---: |
| ubuntu-24.04 | X64 | 116021571030 | 11918793626 |
| macos-15 | ARM64 | 116021570713 | 11918848764 |
| windows-2025 | X64 | 116021570947 | 11917909187 |

Each mechanical job passed fmt, all-target Clippy with `-D warnings`, 35 debug and
35 release tests, the real native repository example, and all 29 compiling guard
mutations. Every source module was restored to its original hash. Rust was 1.99.0
and Bun was 1.3.13. The example stored clean manifest
`f0d748da511a8540b64683e5855752dd4cb8f3b0c2a715b95523211c7e5c7d97` and reported
`Content` for the conflicting path on all three runners.

The owner downloaded raw checkout logs and all three artifacts to
`/tmp/javelin-bounded-pr21/hosted-38613209944`. A repeatable Bun verifier at
`/tmp/javelin-bounded-pr21/verify-hosted.mjs` checks the exact checkout, ten file
hashes against the current component, debug/release counts, oracle counts, typed
resource outcomes, all 29 compiled mutation failure logs, restored source hashes
and the stored example result. The raw log is the adjacent
`hosted-38613209944-raw.log`. Linux/macOS hashes match LF bytes; Windows hashes
match the same committed text with CRLF. The verifier output is retained in
`/tmp/javelin-bounded-pr21/verified-38613209944.json`.

The independently downloaded artifact ZIP digests match GitHub metadata:

- Linux: `dcc6fe9f824b9113283d9b9801ac29116811b0f3d92ed7c0644e592dbe3127af`.
- macOS: `3826931b6f65706fb4c491d0aaa87f04b55217b536139676ab5ecb07ee8c6fda`.
- Windows: `ee4e90419d33d31fcb46c253044881e2d5d6522092ed7163ba3b3d96aa0b2486`.

Artifacts expire on 2026-10-28 UTC. Release scattered 2,000-line observations were
29.166 ms on Linux, 65.872 ms on macOS and 66.718 ms on Windows, each reserving
394,938,298 work units for the same 18,890/18,890/19,690 input bytes. These are
single-run observations, not a latency guarantee. Larger admitted-size/work
combinations refuse deterministically rather than relying on those timings.

VERIFIED at this component boundary: fixed admission limits, typed whole-call
resource refusal, tested exact matching/separator behavior, ordinary-file caller
fixtures, the stated finite oracle domains, local and hosted checks and source
restoration. No blocking finding remains in this unit's executed checks.
INCONCLUSIVE: universal correctness or preservation outside the tested domains.
NOT VERIFIED here: bounded repository reads before caller allocation, concurrent
caller memory, production Stack/refresh, whole-Layer Publish, hosting, native core
acceptance, additional architectures or GA. These require later hub-owned work.
