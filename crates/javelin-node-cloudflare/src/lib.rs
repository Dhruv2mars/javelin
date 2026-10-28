//! The Javelin node core on Cloudflare (D23). `javelin-node` without its `native` feature is
//! the core; this crate gives it a provider: `vfs::PageStorage` keeps the node's SQLite
//! databases and documents as page rows in a Durable Object's SQLite storage (`pages`), and
//! `service::Node` answers the Worker's admin routes and protocol version 2. Built for
//! wasm32-unknown-unknown, `worker` exports the node to the Durable Object in
//! `tools/node-cloudflare/src/worker.js`.

pub mod pages;
pub mod service;
pub mod vfs;
#[cfg(target_arch = "wasm32")]
mod worker;
