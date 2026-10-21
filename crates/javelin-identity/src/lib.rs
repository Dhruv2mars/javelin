//! Scoped access tokens, key verification, applied permissions and the revocation set
//! for one repository node. Verification is offline: the node holds public keys and the
//! applied permissions, and never calls the control plane. The control plane signs tokens
//! and pushes permissions and revocations. This crate holds the node's side only.
//! See `docs/online-design.md` sections 4.7 and 8 (U7).

mod access;
mod token;

pub mod test_keys;

pub use access::{Access, Grant, Need, Refusal, Revocation, Scope};
pub use token::{Claims, Issuer, KeySet, MAX_LIFETIME, MAX_TOKEN_BYTES, TokenError, verify};

/// An opaque ID: 1 to 128 ASCII letters, digits, `-`, `_`, `.` or `:`, the same rule as
/// `javelin-collaboration`.
fn opaque(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}
