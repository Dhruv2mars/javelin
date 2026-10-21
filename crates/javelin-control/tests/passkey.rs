//! Passkey enrollment, sign-in and device approval, driven by a software authenticator. No
//! browser and no hardware: the authenticator signs what `webauthn-rs` issues.

use javelin_control::{ControlError, ControlPlane, Poll, RelyingParty};
use webauthn_authenticator_rs::{
    AuthenticatorBackend, WebauthnAuthenticator, softpasskey::SoftPasskey,
};
use webauthn_rs::prelude::Url;

const T0: u64 = 1_800_000_000;
const ORIGIN: &str = "http://localhost:8080";

fn origin() -> Url {
    Url::parse(ORIGIN).expect("origin")
}

/// A software authenticator. It claims user verification, which passkey enrollment requires;
/// there is no user to verify, so the claim is the test double's.
fn authenticator() -> SoftPasskey {
    SoftPasskey::new(true)
}

fn open() -> (tempfile::TempDir, ControlPlane, String, RelyingParty) {
    let dir = tempfile::tempdir().expect("tempdir");
    let plane = ControlPlane::open(dir.path()).expect("open");
    let admin = plane.bootstrap().expect("bootstrap");
    let rp = RelyingParty::new(ORIGIN).expect("relying party");
    (dir, plane, admin, rp)
}

/// Enrolls `authenticator` as a passkey for the holder of `bearer`.
fn enroll(
    plane: &ControlPlane,
    rp: &RelyingParty,
    bearer: &str,
    authenticator: &mut SoftPasskey,
    now: u64,
) {
    let (ceremony, options) = plane
        .passkey_enroll_start(rp, bearer, now)
        .expect("enroll start");
    let credential = authenticator
        .do_registration(origin(), options)
        .expect("authenticator registers");
    plane
        .passkey_enroll_finish(rp, &ceremony, &credential, now)
        .expect("enroll finish");
}

/// Signs in with `authenticator` and returns the browser session token.
fn sign_in(
    plane: &ControlPlane,
    rp: &RelyingParty,
    authenticator: &mut SoftPasskey,
    now: u64,
) -> String {
    let (ceremony, options) = plane.passkey_signin_start(rp, now).expect("signin start");
    let credential = authenticator
        .do_authentication(origin(), options)
        .expect("authenticator signs");
    plane
        .passkey_signin_finish(rp, &ceremony, &credential, now)
        .expect("signin finish")
        .token
}

#[test]
fn passkey_sign_in_approves_a_device_code() {
    let (_dir, plane, admin, rp) = open();
    let mut key = authenticator();
    enroll(&plane, &rp, &admin, &mut key, T0);

    let start = plane.device_start(T0).expect("device start");
    let browser = sign_in(&plane, &rp, &mut key, T0 + 10);
    plane
        .device_approve_as(&browser, &start.user_code, T0 + 20)
        .expect("signed-in approval");

    match plane
        .device_poll(&start.device_code, T0 + 30)
        .expect("poll")
    {
        Poll::Approved(session) => assert_eq!(session.principal, "admin"),
        Poll::Pending => panic!("a signed-in approval left the code pending"),
    }
}

#[test]
fn admin_token_still_approves_through_the_same_path() {
    let (_dir, plane, admin, _rp) = open();
    let start = plane.device_start(T0).expect("device start");
    plane
        .device_approve_as(&admin, &start.user_code, T0)
        .expect("admin approval");
    assert!(matches!(
        plane.device_poll(&start.device_code, T0).expect("poll"),
        Poll::Approved(_)
    ));
}

#[test]
fn approval_without_a_live_sign_in_is_refused() {
    let (_dir, plane, _admin, _rp) = open();
    let start = plane.device_start(T0).expect("device start");
    assert!(matches!(
        plane.device_approve_as("jav_web_not-a-real-session", &start.user_code, T0),
        Err(ControlError::Unauthorized(_))
    ));
}

#[test]
fn sign_in_expires_with_its_session() {
    let (_dir, plane, admin, rp) = open();
    let mut key = authenticator();
    enroll(&plane, &rp, &admin, &mut key, T0);
    let browser = sign_in(&plane, &rp, &mut key, T0);
    let start = plane.device_start(T0).expect("device start");
    assert!(matches!(
        plane.device_approve_as(&browser, &start.user_code, T0 + 900),
        Err(ControlError::Unauthorized(_))
    ));
}

#[test]
fn enrollment_needs_the_admin_token_or_a_sign_in() {
    let (_dir, plane, admin, rp) = open();
    let mut key = authenticator();
    enroll(&plane, &rp, &admin, &mut key, T0);

    assert!(matches!(
        plane.passkey_enroll_start(&rp, "jav_admin_wrong", T0),
        Err(ControlError::Unauthorized(_))
    ));
    assert!(matches!(
        plane.passkey_enroll_start(&rp, "jav_web_missing", T0),
        Err(ControlError::Unauthorized(_))
    ));
    // A second passkey may be added from a live sign-in.
    let browser = sign_in(&plane, &rp, &mut key, T0);
    let mut second = authenticator();
    enroll(&plane, &rp, &browser, &mut second, T0 + 1);
}

#[test]
fn sign_in_without_any_passkey_is_refused() {
    let (_dir, plane, _admin, rp) = open();
    assert!(matches!(
        plane.passkey_signin_start(&rp, T0),
        Err(ControlError::NotFound(_))
    ));
}

#[test]
fn a_tampered_assertion_is_refused() {
    let (_dir, plane, admin, rp) = open();
    let mut key = authenticator();
    enroll(&plane, &rp, &admin, &mut key, T0);

    let (ceremony, options) = plane.passkey_signin_start(&rp, T0).expect("start");
    let mut credential = key.do_authentication(origin(), options).expect("sign");
    credential.response.signature[0] ^= 0xff;
    assert!(matches!(
        plane.passkey_signin_finish(&rp, &ceremony, &credential, T0),
        Err(ControlError::Unauthorized(_))
    ));
}

#[test]
fn a_ceremony_is_spent_by_its_finish() {
    let (_dir, plane, admin, rp) = open();
    let mut key = authenticator();
    let (ceremony, options) = plane
        .passkey_enroll_start(&rp, &admin, T0)
        .expect("enroll start");
    let credential = key.do_registration(origin(), options).expect("register");
    plane
        .passkey_enroll_finish(&rp, &ceremony, &credential, T0)
        .expect("first finish");
    assert!(matches!(
        plane.passkey_enroll_finish(&rp, &ceremony, &credential, T0),
        Err(ControlError::NotFound(_))
    ));
}

#[test]
fn a_ceremony_of_the_other_kind_is_refused_and_kept() {
    let (_dir, plane, admin, rp) = open();
    let mut key = authenticator();
    enroll(&plane, &rp, &admin, &mut key, T0);

    let (sign_in_ceremony, options) = plane.passkey_signin_start(&rp, T0).expect("start");
    let registration = {
        let (_, options) = plane
            .passkey_enroll_start(&rp, &admin, T0)
            .expect("enroll start");
        authenticator()
            .do_registration(origin(), options)
            .expect("register")
    };
    assert!(matches!(
        plane.passkey_enroll_finish(&rp, &sign_in_ceremony, &registration, T0),
        Err(ControlError::InvalidInput(_))
    ));
    // The refused finish did not spend the sign-in ceremony.
    let credential = key.do_authentication(origin(), options).expect("sign");
    plane
        .passkey_signin_finish(&rp, &sign_in_ceremony, &credential, T0)
        .expect("sign-in still completes");
}

#[test]
fn a_ceremony_expires_after_five_minutes() {
    let (_dir, plane, admin, rp) = open();
    let (ceremony, options) = plane
        .passkey_enroll_start(&rp, &admin, T0)
        .expect("enroll start");
    let credential = authenticator()
        .do_registration(origin(), options)
        .expect("register");
    assert!(matches!(
        plane.passkey_enroll_finish(&rp, &ceremony, &credential, T0 + 301),
        Err(ControlError::Expired(_))
    ));
}

#[test]
fn a_different_origin_cannot_enroll() {
    let (_dir, plane, admin, rp) = open();
    let (ceremony, options) = plane
        .passkey_enroll_start(&rp, &admin, T0)
        .expect("enroll start");
    let credential = authenticator()
        .perform_register(
            Url::parse("http://evil.example").unwrap(),
            options.public_key,
            1000,
        )
        .expect("register elsewhere");
    assert!(matches!(
        plane.passkey_enroll_finish(&rp, &ceremony, &credential, T0),
        Err(ControlError::Unauthorized(_))
    ));
}
