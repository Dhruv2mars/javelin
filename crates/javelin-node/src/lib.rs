//! Repository node core: the self-hosted coordinator for one repository. It owns
//! the World log, Layer heads and objects in a file-backed store, and serves protocol
//! version 2 over loopback HTTP. It hosts idempotent `PublishLayer`, and the Layer
//! partition's `LayerAppend` and `LayerHead` with their changes uploads (`layer.rs`), and
//! `LayerStack` with the checkpoint reads a refresh needs (`stack.rs`). Layer writes go
//! through each Layer's writer lease, which `LayerTake` moves (`lease.rs`).
//! See `docs/online-design.md` sections 4.2, 4.3, 4.5, 4.6 and 8 (U2, U5, U6).
//!
//! The core reaches its provider only through `storage::Storage` and
//! `storage::Coordination`, and the wire only through `wire::handle`. The default `native`
//! feature adds the self-hosted parts: the data directory, the loopback HTTP server and the
//! `javelin-node` binary. Without it the crate builds for wasm32-unknown-unknown, where the
//! Cloudflare Durable Object runs it.

pub mod admin;
pub mod auth;
pub mod budget;
pub mod checks;
pub mod disclosure;
pub mod fork;
mod layer;
mod lease;
mod outside;
pub mod review;
#[cfg(feature = "native")]
mod server;
mod stack;
pub mod storage;
mod trace;
pub mod wire;
mod world_log;

#[cfg(feature = "native")]
pub use server::{bind_loopback, serve, serve_live};
pub use stack::MERGE_LIMIT;
pub use trace::{TraceVisibility, Traces};
pub use world_log::FileWorldLog;
