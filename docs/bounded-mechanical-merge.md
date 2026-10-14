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
Independent implementation review and hosted exact-head verification are pending. General correctness beyond the small oracle is
INCONCLUSIVE. This is not production Stack or a product gate.
