//! U14, the hosted acceptance suite: contract gate 4 (`docs/build-contract.md`, "Verification
//! gates") and the outage matrix (`docs/online-design.md` section 5), against the self-hosted
//! target. Each test starts a real control plane in a temporary directory, signs in through the
//! approval page's HTTP endpoints with a software passkey, creates repositories, and drives the
//! self-hosted node the control plane provisioned: its gate, its review partition, and its
//! loopback HTTP server, behind a fault-injecting proxy where a test needs one.
//!
//! The control plane has no membership API yet, so reviewers other than the creator get roles by
//! a permissions push straight to the node, and their tokens come from the public test key the
//! node is told to trust. The creator's tokens are the control plane's own.
//!
//! `tools/acceptance/run.ts` runs this file with a failing `git` first on PATH. When
//! `JAVELIN_ACCEPTANCE_REPORT` names a file, each assertion appends one JSON line to it:
//! `VERIFIED` once its checks pass, or `NOT VERIFIED` with the reason it cannot be proved here.
//! A failing test fails the run, and an assertion it never reported is FAILED as well.

mod flows;
mod gaps;
mod gate4;
mod outages;
mod support;
mod target;
