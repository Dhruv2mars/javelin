//! Random secrets, their hashes, and the file writes that keep them private.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
};

use base64ct::{Base64UrlUnpadded, Encoding};
use sha2::{Digest, Sha256};

/// Alphabet for user codes: no vowels, no look-alike characters (0, 1, O, I, U, A, E).
const USER_CODE_ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ23456789";

pub(crate) fn bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    getrandom::getrandom(&mut buf).expect("the OS random source is available");
    buf
}

pub(crate) fn encode(bytes: &[u8]) -> String {
    Base64UrlUnpadded::encode_string(bytes)
}

pub(crate) fn decode<const N: usize>(text: &str) -> Option<[u8; N]> {
    Base64UrlUnpadded::decode_vec(text).ok()?.try_into().ok()
}

/// A bearer secret: 256 random bits behind a readable prefix.
pub(crate) fn token(prefix: &str) -> String {
    format!("{prefix}{}", encode(&bytes::<32>()))
}

/// An opaque ID: 96 random bits behind a prefix.
pub(crate) fn id(prefix: &str) -> String {
    let mut out = String::from(prefix);
    for byte in bytes::<12>() {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// A user code of eight letters and digits, shown as `XXXX-XXXX`.
pub(crate) fn user_code() -> String {
    let code: String = bytes::<8>()
        .iter()
        .map(|b| USER_CODE_ALPHABET[usize::from(*b) % USER_CODE_ALPHABET.len()] as char)
        .collect();
    format!("{}-{}", &code[..4], &code[4..])
}

/// The canonical form of a typed user code, or `None` if it is not one.
pub(crate) fn canonical_user_code(typed: &str) -> Option<String> {
    let code: String = typed
        .chars()
        .filter(|c| *c != '-' && !c.is_whitespace())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (code.len() == 8 && code.bytes().all(|b| USER_CODE_ALPHABET.contains(&b))).then_some(code)
}

/// The SHA-256 of a secret, hex. Only hashes are stored, so state never holds a live token.
pub(crate) fn hash(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Writes `bytes` to `path` through a temporary file, so a crash leaves the old or the new
/// content, never a mix. The file is private to its owner on Unix.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(tmp, path)
}
