//! Signed access tokens: a compact JWS (RFC 7515) with `alg` fixed to `EdDSA`
//! (Ed25519, RFC 8032). The verifier picks its key from the `kid` header among keys it
//! was given, never from the token, and checks the signature before it reads a claim.

use std::{collections::BTreeMap, error::Error, fmt};

use base64ct::{Base64UrlUnpadded, Encoding};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{Scope, opaque};

/// Longest accepted lifetime, from `iat` to `exp`. Access tokens last one hour (D6).
pub const MAX_LIFETIME: u64 = 3600;
/// Largest token accepted. Checked before any decoding.
pub const MAX_TOKEN_BYTES: usize = 4096;
/// How far `iat` may lie ahead of the verifier's clock, in seconds.
const CLOCK_SKEW: u64 = 60;
const ALG: &str = "EdDSA";

/// The signed payload. `sub` is the principal, `dev` the device, `repo` the repository ID
/// or `*` for discovery. `layer` narrows the token to one Layer. `jti` names the token for
/// revocation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claims {
    pub sub: String,
    pub dev: String,
    pub repo: String,
    pub scopes: Vec<Scope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    pub iat: u64,
    pub exp: u64,
    pub jti: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    kid: String,
}

/// Why a token was refused. Every variant means the caller is not authenticated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenError {
    Malformed(&'static str),
    UnknownKey,
    BadSignature,
    Expired,
    NotYetValid,
    LifetimeTooLong,
}

impl TokenError {
    /// The human-readable reason, without the code.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Malformed(reason) => reason,
            Self::UnknownKey => "token key ID is not trusted",
            Self::BadSignature => "token signature is invalid",
            Self::Expired => "token has expired",
            Self::NotYetValid => "token is not valid yet",
            Self::LifetimeTooLong => "token lifetime exceeds one hour",
        }
    }
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason())
    }
}

impl Error for TokenError {}

/// The verification keys a node trusts, by key ID. Rotation adds the new key, and then
/// removes the old one once its tokens have expired.
#[derive(Clone, Debug, Default)]
pub struct KeySet {
    keys: BTreeMap<String, VerifyingKey>,
}

impl KeySet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Trusts `key` under `kid`, replacing any key already under that ID.
    pub fn trust(&mut self, kid: &str, key: VerifyingKey) -> Result<(), TokenError> {
        if !opaque(kid) {
            return Err(TokenError::Malformed("key ID is not an opaque ID"));
        }
        self.keys.insert(kid.to_string(), key);
        Ok(())
    }

    /// Stops trusting `kid`. Returns whether it was trusted.
    pub fn untrust(&mut self, kid: &str) -> bool {
        self.keys.remove(kid).is_some()
    }

    fn get(&self, kid: &str) -> Option<&VerifyingKey> {
        self.keys.get(kid)
    }
}

/// Signs tokens with one key. Held by the control plane, never by a node.
pub struct Issuer {
    kid: String,
    key: SigningKey,
}

impl Issuer {
    pub fn new(kid: &str, key: SigningKey) -> Result<Self, TokenError> {
        if !opaque(kid) {
            return Err(TokenError::Malformed("key ID is not an opaque ID"));
        }
        Ok(Self {
            kid: kid.to_string(),
            key,
        })
    }

    pub fn issue(&self, claims: &Claims) -> Result<String, serde_json::Error> {
        let header = Base64UrlUnpadded::encode_string(&serde_json::to_vec(&Header {
            alg: ALG.to_string(),
            kid: self.kid.clone(),
        })?);
        let body = Base64UrlUnpadded::encode_string(&serde_json::to_vec(claims)?);
        let signing_input = format!("{header}.{body}");
        let signature = self.key.sign(signing_input.as_bytes());
        Ok(format!(
            "{signing_input}.{}",
            Base64UrlUnpadded::encode_string(&signature.to_bytes())
        ))
    }
}

/// Checks `token` at time `now` (Unix seconds) against the trusted keys. Proves only that
/// the control plane signed the claims and that they are current. It does not check
/// revocation, repository, role or scope; [`crate::Access`] does.
pub fn verify(keys: &KeySet, token: &str, now: u64) -> Result<Claims, TokenError> {
    if token.len() > MAX_TOKEN_BYTES {
        return Err(TokenError::Malformed("token is too large"));
    }
    let mut parts = token.split('.');
    let (Some(head), Some(body), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(TokenError::Malformed("token is not three segments"));
    };
    let header: Header = decode(head).ok_or(TokenError::Malformed("token header is not valid"))?;
    if header.alg != ALG {
        return Err(TokenError::Malformed("token algorithm is not EdDSA"));
    }
    let key = keys.get(&header.kid).ok_or(TokenError::UnknownKey)?;
    let signature = Base64UrlUnpadded::decode_vec(signature)
        .ok()
        .and_then(|bytes| Signature::from_slice(&bytes).ok())
        .ok_or(TokenError::BadSignature)?;
    key.verify_strict(format!("{head}.{body}").as_bytes(), &signature)
        .map_err(|_| TokenError::BadSignature)?;

    let claims: Claims = decode(body).ok_or(TokenError::Malformed("token claims are not valid"))?;
    if !opaque(&claims.sub) || !opaque(&claims.dev) || !opaque(&claims.jti) {
        return Err(TokenError::Malformed(
            "token identifiers are not opaque IDs",
        ));
    }
    if claims.exp <= claims.iat {
        return Err(TokenError::Malformed("token lifetime is empty"));
    }
    if claims.exp - claims.iat > MAX_LIFETIME {
        return Err(TokenError::LifetimeTooLong);
    }
    if claims.exp <= now {
        return Err(TokenError::Expired);
    }
    if claims.iat > now + CLOCK_SKEW {
        return Err(TokenError::NotYetValid);
    }
    Ok(claims)
}

fn decode<T: DeserializeOwned>(segment: &str) -> Option<T> {
    let bytes = Base64UrlUnpadded::decode_vec(segment).ok()?;
    serde_json::from_slice(&bytes).ok()
}
