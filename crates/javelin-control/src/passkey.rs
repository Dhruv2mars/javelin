//! Passkeys (WebAuthn, D5) for the browser approval page. Enrollment needs the admin token or
//! a live browser sign-in. Sign-in needs an enrolled passkey and yields a short browser
//! session, which can approve device codes. `webauthn-rs` verifies every ceremony; this module
//! keeps the records and the one-shot ceremony state, so a finish only follows its own start.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use webauthn_rs::prelude::{
    CreationChallengeResponse, CredentialID, Passkey, PasskeyAuthentication, PasskeyRegistration,
    PublicKeyCredential, RegisterPublicKeyCredential, RequestChallengeResponse, Url, Uuid,
    Webauthn, WebauthnBuilder, WebauthnError,
};

use crate::{
    ControlError, ControlPlane,
    control::{authenticate, require_initialized},
    secret,
    state::State,
};

/// Ceremonies last five minutes.
const CEREMONY_TTL: u64 = 300;
/// Browser sign-ins last 15 minutes. They exist to approve a device code, nothing more.
pub(crate) const BROWSER_TTL: u64 = 900;
/// Prefix that marks a browser sign-in token, so it is never parsed as a JWS.
pub(crate) const BROWSER_PREFIX: &str = "jav_web_";
const RP_NAME: &str = "Javelin";

/// The relying party for the browser page. WebAuthn binds every ceremony to its origin.
pub struct RelyingParty {
    origin: Url,
    webauthn: Webauthn,
}

impl RelyingParty {
    /// The relying party for a page served at `origin`, such as `http://localhost:8080`.
    pub fn new(origin: &str) -> Result<Self, ControlError> {
        let origin = Url::parse(origin).map_err(|error| {
            ControlError::InvalidInput(format!("the origin is not a URL: {error}"))
        })?;
        let host = origin
            .host_str()
            .ok_or_else(|| ControlError::InvalidInput("the origin has no host".into()))?;
        let webauthn = WebauthnBuilder::new(host, &origin)
            .and_then(|builder| builder.rp_name(RP_NAME).build())
            .map_err(|error| ControlError::InvalidInput(error.to_string()))?;
        Ok(Self { origin, webauthn })
    }

    pub fn origin(&self) -> &Url {
        &self.origin
    }
}

/// A browser sign-in that the caller keeps as a cookie.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserSession {
    pub token: String,
    pub expires_in: u64,
}

/// An enrolled passkey and the principal it signs in as.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PasskeyRecord {
    pub principal: String,
    pub passkey: Passkey,
}

/// A ceremony in flight. Each is spent by its finish call.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CeremonyRecord {
    pub expires_at: u64,
    pub step: Step,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum Step {
    Enroll {
        principal: String,
        registration: PasskeyRegistration,
    },
    SignIn {
        authentication: PasskeyAuthentication,
    },
}

/// A live browser sign-in, keyed in state by the hash of its token.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct BrowserRecord {
    pub principal: String,
    pub expires_at: u64,
}

impl ControlPlane {
    /// Starts enrolling a passkey for the caller. The caller is the admin token or a browser
    /// sign-in. Returns the ceremony ID and the options the browser passes to the authenticator.
    pub fn passkey_enroll_start(
        &self,
        rp: &RelyingParty,
        bearer: &str,
        now: u64,
    ) -> Result<(String, CreationChallengeResponse), ControlError> {
        self.store.update(|state| {
            let principal = enroller(state, bearer, now)?;
            let exclude: Vec<CredentialID> = state
                .passkeys
                .values()
                .filter(|record| record.principal == principal)
                .map(|record| record.passkey.cred_id().clone())
                .collect();
            let (options, registration) = rp
                .webauthn
                .start_passkey_registration(
                    user_handle(&principal),
                    &principal,
                    &principal,
                    Some(exclude),
                )
                .map_err(|error| ControlError::InvalidInput(error.to_string()))?;
            let ceremony = secret::id("cer_");
            insert_ceremony(
                state,
                &ceremony,
                now,
                Step::Enroll {
                    principal,
                    registration,
                },
            );
            Ok((ceremony, options))
        })
    }

    /// Finishes an enrollment. The passkey is stored only if the authenticator's response
    /// verifies against the ceremony's challenge and origin.
    pub fn passkey_enroll_finish(
        &self,
        rp: &RelyingParty,
        ceremony: &str,
        credential: &RegisterPublicKeyCredential,
        now: u64,
    ) -> Result<(), ControlError> {
        self.store.update(|state| {
            let Step::Enroll {
                principal,
                registration,
            } = take_ceremony(state, ceremony, now)?
            else {
                return Err(ControlError::InvalidInput(
                    "this ceremony is a sign-in, not an enrollment".into(),
                ));
            };
            let passkey = rp
                .webauthn
                .finish_passkey_registration(credential, &registration)
                .map_err(verify_error)?;
            let id = credential_key(passkey.cred_id());
            if state.passkeys.contains_key(&id) {
                return Err(ControlError::InvalidInput(
                    "this passkey is already enrolled".into(),
                ));
            }
            state
                .passkeys
                .insert(id, PasskeyRecord { principal, passkey });
            Ok(())
        })
    }

    /// Starts a sign-in with any enrolled passkey.
    pub fn passkey_signin_start(
        &self,
        rp: &RelyingParty,
        now: u64,
    ) -> Result<(String, RequestChallengeResponse), ControlError> {
        self.store.update(|state| {
            require_initialized(state)?;
            if state.passkeys.is_empty() {
                return Err(ControlError::NotFound("no passkey is enrolled yet"));
            }
            let passkeys: Vec<Passkey> = state
                .passkeys
                .values()
                .map(|record| record.passkey.clone())
                .collect();
            let (options, authentication) = rp
                .webauthn
                .start_passkey_authentication(&passkeys)
                .map_err(|error| ControlError::InvalidInput(error.to_string()))?;
            let ceremony = secret::id("cer_");
            insert_ceremony(state, &ceremony, now, Step::SignIn { authentication });
            Ok((ceremony, options))
        })
    }

    /// Finishes a sign-in and returns a browser session for the passkey's principal.
    pub fn passkey_signin_finish(
        &self,
        rp: &RelyingParty,
        ceremony: &str,
        credential: &PublicKeyCredential,
        now: u64,
    ) -> Result<BrowserSession, ControlError> {
        self.store.update(|state| {
            let Step::SignIn { authentication } = take_ceremony(state, ceremony, now)? else {
                return Err(ControlError::InvalidInput(
                    "this ceremony is an enrollment, not a sign-in".into(),
                ));
            };
            let result = rp
                .webauthn
                .finish_passkey_authentication(credential, &authentication)
                .map_err(verify_error)?;
            let record = state
                .passkeys
                .get_mut(&credential_key(result.cred_id()))
                .ok_or(ControlError::Unauthorized("this passkey is not enrolled"))?;
            record.passkey.update_credential(&result);
            let principal = record.principal.clone();
            let token = secret::token(BROWSER_PREFIX);
            state.browser_sessions.insert(
                secret::hash(&token),
                BrowserRecord {
                    principal,
                    expires_at: now + BROWSER_TTL,
                },
            );
            Ok(BrowserSession {
                token,
                expires_in: BROWSER_TTL,
            })
        })
    }
}

/// The principal behind a live browser sign-in token, if there is one.
pub(crate) fn browser_principal(state: &State, token: &str, now: u64) -> Option<String> {
    state
        .browser_sessions
        .get(&secret::hash(token))
        .filter(|session| now < session.expires_at)
        .map(|session| session.principal.clone())
}

/// Who may enroll a passkey: a live browser sign-in, or the admin token.
fn enroller(state: &State, bearer: &str, now: u64) -> Result<String, ControlError> {
    if bearer.starts_with(BROWSER_PREFIX) {
        return browser_principal(state, bearer, now).ok_or(ControlError::Unauthorized(
            "sign in with your passkey first",
        ));
    }
    let caller = authenticate(state, bearer, now)?;
    if caller.admin {
        Ok(caller.principal)
    } else {
        Err(ControlError::Forbidden(
            "only the admin token or a passkey sign-in can enroll a passkey",
        ))
    }
}

/// Spends a ceremony: it must exist and not have expired. Expired ceremonies are dropped.
fn take_ceremony(state: &mut State, ceremony: &str, now: u64) -> Result<Step, ControlError> {
    let record = state
        .ceremonies
        .remove(ceremony)
        .ok_or(ControlError::NotFound(
            "this ceremony is not known or was already used",
        ))?;
    if now >= record.expires_at {
        return Err(ControlError::Expired(
            "this ceremony has expired; start again",
        ));
    }
    Ok(record.step)
}

fn insert_ceremony(state: &mut State, ceremony: &str, now: u64, step: Step) {
    state.ceremonies.retain(|_, record| record.expires_at > now);
    state.ceremonies.insert(
        ceremony.to_string(),
        CeremonyRecord {
            expires_at: now + CEREMONY_TTL,
            step,
        },
    );
}

/// The state key for a credential: its ID, base64url.
fn credential_key(id: &CredentialID) -> String {
    secret::encode(id.as_ref())
}

/// A stable WebAuthn user handle for a principal, so its passkeys share one.
fn user_handle(principal: &str) -> Uuid {
    let digest = Sha256::digest(principal.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}

/// Any verification failure reads the same to the caller: the response did not check out.
fn verify_error(_: WebauthnError) -> ControlError {
    ControlError::Unauthorized("the passkey response did not verify")
}
