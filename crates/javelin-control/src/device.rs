//! The device-code flow (D5). `javelin login` starts a device code and prints its user code.
//! The admin token approves that code. The waiting login then polls and receives a session.
//! The browser approval page and passkeys come in U8b.

use crate::{
    ControlError, ControlPlane,
    control::{authenticate, issue, require_initialized},
    secret,
    state::{DeviceRecord, SessionRecord},
};

/// How long a device code can wait for approval, in seconds.
const DEVICE_CODE_TTL: u64 = 900;
/// Seconds a device should wait between polls.
const POLL_INTERVAL: u64 = 5;
/// Refresh tokens last 30 days (D6).
const REFRESH_TTL: u64 = 30 * 24 * 3600;
/// Access tokens last one hour (D6).
pub(crate) const ACCESS_TTL: u64 = javelin_identity::MAX_LIFETIME;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceStart {
    /// Secret. Only the device holds it; the control plane keeps its hash.
    pub device_code: String,
    /// What the approver types, shown as `XXXX-XXXX`.
    pub user_code: String,
    pub expires_at: u64,
    pub interval: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Poll {
    Pending,
    Approved(Session),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub principal: String,
    pub device: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessToken {
    pub access_token: String,
    pub expires_in: u64,
}

impl ControlPlane {
    /// Starts a device code. Expired, unredeemed codes are dropped on the way.
    pub fn device_start(&self, now: u64) -> Result<DeviceStart, ControlError> {
        let device_code = secret::token("");
        let user_code = secret::user_code();
        let expires_at = now + DEVICE_CODE_TTL;
        let canonical = user_code.replace('-', "");
        self.store.update(|state| {
            require_initialized(state)?;
            state
                .device_codes
                .retain(|_, record| record.expires_at > now);
            state.device_codes.insert(
                secret::hash(&device_code),
                DeviceRecord {
                    user_hash: secret::hash(&canonical),
                    expires_at,
                    approved_by: None,
                    device: None,
                    redeemed: false,
                },
            );
            Ok(())
        })?;
        Ok(DeviceStart {
            device_code,
            user_code,
            expires_at,
            interval: POLL_INTERVAL,
        })
    }

    /// Approves the device code that `user_code` names. Only the admin token may approve.
    /// Approving twice is harmless.
    pub fn device_approve(
        &self,
        admin_token: &str,
        user_code: &str,
        now: u64,
    ) -> Result<(), ControlError> {
        let canonical = secret::canonical_user_code(user_code).ok_or_else(|| {
            ControlError::InvalidInput(
                "a user code is eight letters and digits, as XXXX-XXXX".into(),
            )
        })?;
        let user_hash = secret::hash(&canonical);
        self.store.update(|state| {
            let caller = authenticate(state, admin_token, now)?;
            if !caller.admin {
                return Err(ControlError::Forbidden(
                    "only the admin token can approve a device",
                ));
            }
            let record = state
                .device_codes
                .values_mut()
                .find(|record| record.user_hash == user_hash)
                .ok_or(ControlError::NotFound(
                    "no pending device code has this user code",
                ))?;
            if now >= record.expires_at && record.approved_by.is_none() {
                return Err(ControlError::Expired("this device code has expired"));
            }
            if record.approved_by.is_none() {
                record.approved_by = Some(caller.principal);
                record.device = Some(secret::id("dev_"));
            }
            Ok(())
        })
    }

    /// Polls a device code. Once approved, the first poll returns the session; later polls
    /// report that the code was already redeemed.
    pub fn device_poll(&self, device_code: &str, now: u64) -> Result<Poll, ControlError> {
        let hash = secret::hash(device_code);
        self.store.update(|state| {
            let record = state
                .device_codes
                .get_mut(&hash)
                .ok_or(ControlError::NotFound("this device code is not known"))?;
            if record.redeemed {
                return Err(ControlError::Redeemed);
            }
            let (Some(principal), Some(device)) =
                (record.approved_by.clone(), record.device.clone())
            else {
                if now >= record.expires_at {
                    return Err(ControlError::Expired("this device code has expired"));
                }
                return Ok(Poll::Pending);
            };
            record.redeemed = true;
            let refresh_token = secret::token("jav_refresh_");
            state.sessions.insert(
                secret::hash(&refresh_token),
                SessionRecord {
                    principal: principal.clone(),
                    device: device.clone(),
                    expires_at: now + REFRESH_TTL,
                },
            );
            let access_token = issue(state, &principal, &device, "*", now)?;
            Ok(Poll::Approved(Session {
                principal,
                device,
                access_token,
                refresh_token,
                expires_in: ACCESS_TTL,
            }))
        })
    }

    /// Exchanges a refresh token for a new access token. The refresh token itself stays valid.
    pub fn refresh(&self, refresh_token: &str, now: u64) -> Result<AccessToken, ControlError> {
        let hash = secret::hash(refresh_token);
        self.store.read(|state| {
            let session = state
                .sessions
                .get(&hash)
                .ok_or(ControlError::Unauthorized("refresh token is not valid"))?;
            if now >= session.expires_at {
                return Err(ControlError::Unauthorized("refresh token has expired"));
            }
            Ok(AccessToken {
                access_token: issue(state, &session.principal, &session.device, "*", now)?,
                expires_in: ACCESS_TTL,
            })
        })
    }
}
