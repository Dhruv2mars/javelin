//! Versioned, provider-independent repository protocol over the public native API.
//! Version 1 covers negotiation, have/want, bounded object batches and Publish.
//! Transport, authentication and hosting are outside this crate.

pub mod codec;
pub mod receiver;
pub mod sender;

pub use codec::{Kind, Message, Outcome};
pub use receiver::serve;
pub use sender::push;

/// Fixed version 1 limits. Both sides enforce them before allocating.
pub mod limits {
    pub const MAX_FRAME: usize = 16 << 20;
    pub const MAX_BLOB: usize = 8 << 20;
    pub const MAX_BLOBS: usize = 1024;
    pub const MAX_WANT: usize = 4096;
    pub const MAX_MANIFESTS: usize = 16;
    pub const MAX_ENTRIES: usize = 65_536;
    pub const MAX_PATH: usize = 4096;
    pub const MAX_DEPTH: usize = 256;
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
