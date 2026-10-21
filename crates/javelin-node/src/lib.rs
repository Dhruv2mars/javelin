//! Repository node core: the self-hosted coordinator for one repository. It owns
//! the World log, Layer heads and objects in a file-backed store, and serves protocol
//! version 2 over loopback HTTP. It hosts idempotent `PublishLayer`, and the Layer
//! partition's `LayerAppend` and `LayerHead` with their changes uploads (`layer.rs`), and
//! `LayerStack` with the checkpoint reads a refresh needs (`stack.rs`).
//! See `docs/online-design.md` sections 4.2, 4.3, 4.5, 4.6 and 8 (U2, U5, U6).

pub mod auth;
pub mod checks;
pub mod fork;
mod layer;
mod log_do;
pub mod review;
mod server;
mod stack;
mod trace;
mod world_log;

pub use log_do::{CommitReply, ComposedVersion, DoLog, Publish};

pub use server::{bind_loopback, serve};
pub use stack::MERGE_LIMIT;
pub use trace::{TraceVisibility, Traces};
pub use world_log::FileWorldLog;
