//! Fixed Ed25519 keys for tests and the in-process node harness. Their seeds are public,
//! so a token they sign proves nothing: production nodes trust only the keys the control
//! plane installs.

use ed25519_dalek::SigningKey;

use crate::{Claims, Issuer, KeySet, Scope};

/// The key ID of [`issuer`].
pub const KID: &str = "test-1";
/// A second key ID, for rotation tests.
pub const NEXT_KID: &str = "test-2";
/// The device every [`claims`] token is issued from.
pub const DEVICE: &str = "dev-1";

/// A signing key from a one-byte seed, so tests can build a second, untrusted issuer.
pub fn signing_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// The issuer for [`KID`], seed 1.
pub fn issuer() -> Issuer {
    Issuer::new(KID, signing_key(1)).expect("test key ID is an opaque ID")
}

/// A key set that trusts [`KID`], the verifying half of [`issuer`].
pub fn keys() -> KeySet {
    let mut keys = KeySet::new();
    keys.trust(KID, signing_key(1).verifying_key())
        .expect("test key ID is an opaque ID");
    keys
}

/// Claims for `sub` on `repo`, from device [`DEVICE`], valid from `iat` to `exp`.
pub fn claims(sub: &str, repo: &str, scopes: &[Scope], iat: u64, exp: u64, jti: &str) -> Claims {
    Claims {
        sub: sub.to_string(),
        dev: DEVICE.to_string(),
        repo: repo.to_string(),
        scopes: scopes.to_vec(),
        layer: None,
        checks: Vec::new(),
        iat,
        exp,
        jti: jti.to_string(),
    }
}
