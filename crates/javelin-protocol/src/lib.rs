//! Versioned, provider-independent repository protocol over the public native API.
//! Version 2 moves snapshots as derived trees: negotiation, have/want by hash,
//! bounded object batches, Publish of a tree root, and root-down fetch with an
//! optional path prefix. Transport, authentication and hosting are outside this crate.

pub mod codec;
pub mod fetch;
pub mod receiver;
pub mod sender;

pub use codec::{Kind, Message, Outcome};
pub use fetch::{Fetched, fetch};
pub use receiver::serve;
pub use sender::push;

/// Fixed version 2 limits. The codec enforces them before allocating protocol
/// buffers. A snapshot has no size limit: it is bounded per object (one blob, one
/// tree chunk) and per frame, and spans as many frames as it needs.
pub mod limits {
    pub const MAX_FRAME: usize = 16 << 20;
    pub const MAX_BLOB: usize = 8 << 20;
    /// One tree chunk, the native decoder's bound.
    pub const MAX_TREE: usize = javelin::tree::MAX_CHUNK;
    pub const MAX_OBJECTS: usize = 1024;
    pub const MAX_WANT: usize = 4096;
    pub const MAX_DETAIL: usize = 1024;
}

/// A refusal with a stable code. Native errors keep their `code()` string, so new
/// native variants pass through without a protocol change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolError {
    pub code: String,
    pub detail: String,
}

impl ProtocolError {
    pub fn new(code: &str, detail: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for ProtocolError {}

impl From<javelin::Error> for ProtocolError {
    fn from(error: javelin::Error) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

impl From<std::io::Error> for ProtocolError {
    fn from(error: std::io::Error) -> Self {
        Self::new("io_error", error.to_string())
    }
}
