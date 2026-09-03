# Canonical glossary

| Term | Meaning |
| --- | --- |
| World | Logical project managed by one Javelin Store. |
| World Version | Immutable accepted complete state of World. Local IDs are monotonic: `v1`, `v2`, and so on. |
| Current World | One World Version selected as latest accepted state. |
| State Reference | Pointer to immutable World Version or Layer Checkpoint. |
| Private Layer | Isolated tentative state based on a State Reference and targeting World or parent Layer. |
| Local Layer | Implicit Private Layer represented by project root working view. |
| Layer Checkpoint | Immutable automatic or explicit save point inside Private Layer. |
| Layer head | Latest Checkpoint of Private Layer. |
| Origin Reference | Fixed State Reference from which Layer was created. Used for provenance. |
| Synchronized Reference | Latest target State Reference coherently incorporated into Layer head. Advances through Refresh. |
| Contribution | Frozen Layer Checkpoint proposed and accepted into target through Publish. |
| Publish | Atomic integration of Contribution into latest target after conflict handling and required verification. |
| Refresh | Coherent integration of target changes into Private Layer at safe boundary. |
| Discard | Close tentative Layer without altering target. Retain it under recovery policy until purge. |
| Conflict | Stored incompatible base, target, and private path states requiring explicit resolution. |
| World Rule | Configured verification command whose result is recorded; required rules gate Publish. |
| Managed view | Filesystem projection of immutable state plus tentative Layer changes. Reconstructable from Javelin Store. |
| Provenance session | Agent/human context supplied to Javelin and linked to Checkpoints and Contributions. |
| Claim | Passive leased declaration about intended path, symbol, or resource. Informational, not scheduling. |

