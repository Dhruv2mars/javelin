//! Repository node core: the self-hosted coordinator for one repository. It owns
//! the World log, Layer heads and objects in a file-backed store, and serves protocol
//! version 2 over loopback HTTP. Only idempotent `PublishLayer` is hosted so far; the
//! streamed checkpoint path and the Layer partition's wire messages come later.
//! See `docs/online-design.md` sections 4.5, 4.6 and 8 (U2).

pub mod auth;
mod log_do;
mod server;
mod trace;
mod world_log;

pub use log_do::{CommitReply, ComposedVersion, DoLog, Publish};

pub use server::{bind_loopback, serve};
pub use trace::{TraceVisibility, Traces};
pub use world_log::FileWorldLog;
