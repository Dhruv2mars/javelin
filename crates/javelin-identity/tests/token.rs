use base64ct::{Base64UrlUnpadded, Encoding};
use ed25519_dalek::Signer;
use javelin_identity::{
    Claims, Issuer, KeySet, MAX_LIFETIME, MAX_TOKEN_BYTES, Scope, TokenError, test_keys, verify,
};

const NOW: u64 = 1_000_000;
const REPO: &str = "repo-1";

fn claims(iat: u64, exp: u64) -> Claims {
    test_keys::claims("alice", REPO, &[Scope::Read], iat, exp, "tok-1")
}

fn issue(claims: &Claims) -> String {
    test_keys::issuer().issue(claims).unwrap()
}

fn encode(bytes: &[u8]) -> String {
    Base64UrlUnpadded::encode_string(bytes)
}

/// Signs any header and body with the trusted test key, bypassing `Issuer`, so tests can
/// produce tokens that `Issuer` would never produce.
fn sign_raw(header: &str, body: &str) -> String {
    let input = format!("{}.{}", encode(header.as_bytes()), encode(body.as_bytes()));
    let signature = test_keys::signing_key(1).sign(input.as_bytes());
    format!("{input}.{}", encode(&signature.to_bytes()))
}

fn header(alg: &str) -> String {
    format!(r#"{{"alg":"{alg}","kid":"{}"}}"#, test_keys::KID)
}

fn body(claims: &Claims) -> String {
    serde_json::to_string(claims).unwrap()
}

#[test]
fn a_signed_token_verifies_and_returns_its_claims() {
    let claims = claims(NOW, NOW + 600);
    assert_eq!(verify(&test_keys::keys(), &issue(&claims), NOW), Ok(claims));
}

#[test]
fn a_token_signed_by_an_untrusted_key_is_refused() {
    let forged = Issuer::new(test_keys::KID, test_keys::signing_key(9))
        .unwrap()
        .issue(&claims(NOW, NOW + 600))
        .unwrap();
    assert_eq!(
        verify(&test_keys::keys(), &forged, NOW),
        Err(TokenError::BadSignature)
    );
}

#[test]
fn a_tampered_body_breaks_the_signature() {
    let token = issue(&claims(NOW, NOW + 600));
    let mut widened = claims(NOW, NOW + 600);
    widened.scopes.push(Scope::Publish);
    let mut parts: Vec<&str> = token.split('.').collect();
    let widened_body = encode(body(&widened).as_bytes());
    parts[1] = &widened_body;
    assert_eq!(
        verify(&test_keys::keys(), &parts.join("."), NOW),
        Err(TokenError::BadSignature)
    );
}

#[test]
fn only_eddsa_is_accepted_whatever_the_header_says() {
    let claims = claims(NOW, NOW + 600);
    for alg in ["none", "HS256", "RS256"] {
        let token = sign_raw(&header(alg), &body(&claims));
        assert!(
            matches!(
                verify(&test_keys::keys(), &token, NOW),
                Err(TokenError::Malformed(_))
            ),
            "{alg} must be refused"
        );
    }
}

#[test]
fn an_unsigned_token_is_refused() {
    let claims = claims(NOW, NOW + 600);
    let input = format!(
        "{}.{}",
        encode(header("EdDSA").as_bytes()),
        encode(body(&claims).as_bytes())
    );
    assert_eq!(
        verify(&test_keys::keys(), &format!("{input}."), NOW),
        Err(TokenError::BadSignature)
    );
}

#[test]
fn a_token_with_an_unknown_key_id_is_refused() {
    let other = Issuer::new("other-9", test_keys::signing_key(1)).unwrap();
    let token = other.issue(&claims(NOW, NOW + 600)).unwrap();
    assert_eq!(
        verify(&test_keys::keys(), &token, NOW),
        Err(TokenError::UnknownKey)
    );
}

#[test]
fn an_expired_token_is_refused_from_its_expiry_second() {
    let token = issue(&claims(NOW, NOW + 600));
    assert_eq!(
        verify(&test_keys::keys(), &token, NOW + 599),
        Ok(claims(NOW, NOW + 600))
    );
    assert_eq!(
        verify(&test_keys::keys(), &token, NOW + 600),
        Err(TokenError::Expired)
    );
}

#[test]
fn a_token_issued_in_the_future_is_refused_beyond_the_clock_skew() {
    let token = issue(&claims(NOW + 60, NOW + 660));
    assert!(verify(&test_keys::keys(), &token, NOW).is_ok());
    let early = issue(&claims(NOW + 61, NOW + 661));
    assert_eq!(
        verify(&test_keys::keys(), &early, NOW),
        Err(TokenError::NotYetValid)
    );
}

#[test]
fn lifetime_is_capped_at_one_hour() {
    let exact = issue(&claims(NOW, NOW + MAX_LIFETIME));
    assert!(verify(&test_keys::keys(), &exact, NOW).is_ok());
    let long = issue(&claims(NOW, NOW + MAX_LIFETIME + 1));
    assert_eq!(
        verify(&test_keys::keys(), &long, NOW),
        Err(TokenError::LifetimeTooLong)
    );
}

#[test]
fn an_empty_lifetime_is_malformed() {
    let token = issue(&claims(NOW, NOW));
    assert!(matches!(
        verify(&test_keys::keys(), &token, NOW),
        Err(TokenError::Malformed(_))
    ));
}

#[test]
fn an_oversized_token_is_refused_before_it_is_decoded() {
    let token = format!("{}.", "a".repeat(MAX_TOKEN_BYTES));
    assert!(matches!(
        verify(&test_keys::keys(), &token, NOW),
        Err(TokenError::Malformed(_))
    ));
}

#[test]
fn a_token_with_the_wrong_number_of_segments_is_malformed() {
    let token = issue(&claims(NOW, NOW + 600));
    let two: String = token.rsplitn(2, '.').last().unwrap().to_string();
    let four = format!("{token}.extra");
    for bad in [two, four, String::from("not-a-token")] {
        assert!(matches!(
            verify(&test_keys::keys(), &bad, NOW),
            Err(TokenError::Malformed(_))
        ));
    }
}

#[test]
fn a_signed_body_with_an_unknown_claim_is_malformed() {
    let mut value = serde_json::to_value(claims(NOW, NOW + 600)).unwrap();
    value["admin"] = serde_json::json!(true);
    let token = sign_raw(&header("EdDSA"), &value.to_string());
    assert!(matches!(
        verify(&test_keys::keys(), &token, NOW),
        Err(TokenError::Malformed(_))
    ));
}

#[test]
fn a_signed_body_with_a_non_opaque_subject_is_malformed() {
    let bad = test_keys::claims("bad subject", REPO, &[Scope::Read], NOW, NOW + 600, "tok-2");
    let token = sign_raw(&header("EdDSA"), &body(&bad));
    assert!(matches!(
        verify(&test_keys::keys(), &token, NOW),
        Err(TokenError::Malformed(_))
    ));
}

#[test]
fn a_token_stays_valid_until_its_key_is_removed_from_the_set() {
    let mut keys = test_keys::keys();
    let token = issue(&claims(NOW, NOW + 600));
    keys.trust(
        test_keys::NEXT_KID,
        test_keys::signing_key(2).verifying_key(),
    )
    .unwrap();
    assert!(verify(&keys, &token, NOW).is_ok());
    assert!(keys.untrust(test_keys::KID));
    assert_eq!(verify(&keys, &token, NOW), Err(TokenError::UnknownKey));
}

#[test]
fn a_rotated_key_signs_tokens_that_the_older_set_refuses() {
    let next = Issuer::new(test_keys::NEXT_KID, test_keys::signing_key(2)).unwrap();
    let token = next.issue(&claims(NOW, NOW + 600)).unwrap();
    assert_eq!(
        verify(&test_keys::keys(), &token, NOW),
        Err(TokenError::UnknownKey)
    );
}

#[test]
fn key_ids_must_be_opaque() {
    assert!(Issuer::new("has space", test_keys::signing_key(1)).is_err());
    let mut keys = KeySet::new();
    assert!(
        keys.trust("", test_keys::signing_key(1).verifying_key())
            .is_err()
    );
}
