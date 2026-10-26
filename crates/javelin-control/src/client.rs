//! The control plane as a client sees it. [`Control`] is what the CLI needs: device sign-in,
//! token refresh, repository tokens and repository create. [`ControlPlane`] serves it in
//! process; [`RemoteControl`] calls the control plane's HTTP API (`browser.rs`, `/api/`) over
//! HTTPS, or plain HTTP on this machine only.
//! A control plane that does not answer, or answers 5xx without a refusal of its own, is
//! `control_plane_unavailable` (`docs/online-design.md` sections 4.4 and 5).

use std::time::Duration;

use serde_json::{Value, json};

use crate::{
    AccessToken, ControlError, ControlPlane, CreateRequest, Created, DeviceStart, Endpoint, Poll,
    Publish, Session,
};

/// How long one call may take to connect, and then to answer.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Largest reply the client reads. Every API reply is a little JSON.
const MAX_REPLY: u64 = 256 * 1024;

/// The control-plane operations a client calls. `now` is the caller's Unix time; a remote
/// control plane uses its own clock.
pub trait Control {
    fn device_start(&self, now: u64) -> Result<DeviceStart, ControlError>;
    fn device_poll(&self, device_code: &str, now: u64) -> Result<Poll, ControlError>;
    /// Approves a device code with the admin token or a browser sign-in.
    fn device_approve_as(
        &self,
        bearer: &str,
        user_code: &str,
        now: u64,
    ) -> Result<(), ControlError>;
    /// Exchanges the device's refresh token for a sign-in access token (`repo` is `*`).
    fn refresh(&self, refresh_token: &str, now: u64) -> Result<AccessToken, ControlError>;
    /// Exchanges a sign-in token for an access token one repository's node accepts.
    fn repository_token(
        &self,
        bearer: &str,
        repository: &str,
        now: u64,
    ) -> Result<AccessToken, ControlError>;
    fn create_repository(
        &self,
        bearer: &str,
        request: &CreateRequest,
        now: u64,
    ) -> Result<Created, ControlError>;
}

impl Control for ControlPlane {
    fn device_start(&self, now: u64) -> Result<DeviceStart, ControlError> {
        ControlPlane::device_start(self, now)
    }
    fn device_poll(&self, device_code: &str, now: u64) -> Result<Poll, ControlError> {
        ControlPlane::device_poll(self, device_code, now)
    }
    fn device_approve_as(
        &self,
        bearer: &str,
        user_code: &str,
        now: u64,
    ) -> Result<(), ControlError> {
        ControlPlane::device_approve_as(self, bearer, user_code, now)
    }
    fn refresh(&self, refresh_token: &str, now: u64) -> Result<AccessToken, ControlError> {
        ControlPlane::refresh(self, refresh_token, now)
    }
    fn repository_token(
        &self,
        bearer: &str,
        repository: &str,
        now: u64,
    ) -> Result<AccessToken, ControlError> {
        ControlPlane::repository_token(self, bearer, repository, now)
    }
    fn create_repository(
        &self,
        bearer: &str,
        request: &CreateRequest,
        now: u64,
    ) -> Result<Created, ControlError> {
        ControlPlane::create_repository(self, bearer, request, now)
    }
}

/// A control plane reached over HTTPS, or plain HTTP on this machine (`transport.rs`).
pub struct RemoteControl {
    endpoint: Endpoint,
    agent: ureq::Agent,
}

impl RemoteControl {
    /// A client for the control plane at `url`, `https://HOST[:PORT]` (plain `http` only for
    /// localhost), with an optional trailing slash. Nothing is sent until the first call.
    pub fn new(url: &str) -> Result<Self, ControlError> {
        let endpoint = Endpoint::parse(url)
            .map_err(|detail| ControlError::InvalidInput(format!("control plane URL: {detail}")))?;
        let agent = endpoint.agent(TIMEOUT);
        Ok(Self { endpoint, agent })
    }

    /// One call: a JSON body, an optional bearer, and the JSON reply.
    fn call(&self, path: &str, bearer: Option<&str>, body: &Value) -> Result<Value, ControlError> {
        let origin = self.endpoint.origin();
        let mut request = self
            .agent
            .post(format!("{origin}{path}"))
            .header("content-type", "application/json");
        if let Some(bearer) = bearer {
            request = request.header("authorization", &format!("Bearer {bearer}"));
        }
        let mut reply = request
            .send(body.to_string())
            .map_err(|error| unavailable(origin, &format!("did not answer: {error}")))?;
        let status = reply.status().as_u16();
        let bytes = reply
            .body_mut()
            .with_config()
            .limit(MAX_REPLY)
            .read_to_vec()
            .map_err(|error| unavailable(origin, &format!("reply unreadable: {error}")))?;
        let value: Option<Value> = serde_json::from_slice(&bytes).ok();
        match (status, value) {
            (200, Some(value)) => Ok(value),
            // A refusal of the control plane's own carries its code.
            (_, Some(value)) if value["error"].is_string() => Err(ControlError::Remote {
                code: value["error"].as_str().unwrap_or_default().into(),
                detail: value["message"].as_str().unwrap_or_default().into(),
            }),
            (500.., _) => Err(unavailable(
                origin,
                &format!("the control plane answered HTTP {status}"),
            )),
            _ => Err(ControlError::Remote {
                code: "unexpected_reply".into(),
                detail: format!("the control plane answered HTTP {status} without a refusal"),
            }),
        }
    }
}

fn unavailable(authority: &str, detail: &str) -> ControlError {
    ControlError::Unavailable(format!("{authority}: {detail}"))
}

fn text(value: &Value, key: &str) -> Result<String, ControlError> {
    value[key]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| malformed(key))
}

fn number(value: &Value, key: &str) -> Result<u64, ControlError> {
    value[key].as_u64().ok_or_else(|| malformed(key))
}

fn malformed(key: &str) -> ControlError {
    ControlError::Remote {
        code: "unexpected_reply".into(),
        detail: format!("the control plane's reply has no valid {key}"),
    }
}

fn access_token(value: &Value) -> Result<AccessToken, ControlError> {
    Ok(AccessToken {
        access_token: text(value, "access_token")?,
        expires_in: number(value, "expires_in")?,
    })
}

impl Control for RemoteControl {
    fn device_start(&self, _now: u64) -> Result<DeviceStart, ControlError> {
        let value = self.call("/api/device/start", None, &json!({}))?;
        Ok(DeviceStart {
            device_code: text(&value, "device_code")?,
            user_code: text(&value, "user_code")?,
            expires_at: number(&value, "expires_at")?,
            interval: number(&value, "interval")?,
        })
    }

    fn device_poll(&self, device_code: &str, _now: u64) -> Result<Poll, ControlError> {
        let value = self.call(
            "/api/device/poll",
            None,
            &json!({"device_code": device_code}),
        )?;
        match value["status"].as_str() {
            Some("pending") => Ok(Poll::Pending),
            Some("approved") => Ok(Poll::Approved(Session {
                principal: text(&value, "principal")?,
                device: text(&value, "device")?,
                access_token: text(&value, "access_token")?,
                refresh_token: text(&value, "refresh_token")?,
                expires_in: number(&value, "expires_in")?,
            })),
            _ => Err(malformed("status")),
        }
    }

    fn device_approve_as(
        &self,
        bearer: &str,
        user_code: &str,
        _now: u64,
    ) -> Result<(), ControlError> {
        self.call(
            "/device/approve",
            Some(bearer),
            &json!({"user_code": user_code}),
        )
        .map(|_| ())
    }

    fn refresh(&self, refresh_token: &str, _now: u64) -> Result<AccessToken, ControlError> {
        access_token(&self.call(
            "/api/token/refresh",
            None,
            &json!({"refresh_token": refresh_token}),
        )?)
    }

    fn repository_token(
        &self,
        bearer: &str,
        repository: &str,
        _now: u64,
    ) -> Result<AccessToken, ControlError> {
        access_token(&self.call(
            "/api/token/repository",
            Some(bearer),
            &json!({"repository": repository}),
        )?)
    }

    fn create_repository(
        &self,
        bearer: &str,
        request: &CreateRequest,
        _now: u64,
    ) -> Result<Created, ControlError> {
        let value = self.call(
            "/api/repo/create",
            Some(bearer),
            &json!({
                "owner": request.owner,
                "name": request.name,
                "public": request.public,
                "publish": request.publish.as_str(),
                "operation": request.operation,
            }),
        )?;
        Ok(Created {
            repository: text(&value, "repository")?,
            owner: text(&value, "owner")?,
            name: text(&value, "name")?,
            public: value["public"]
                .as_bool()
                .ok_or_else(|| malformed("public"))?,
            publish: Publish::parse(&text(&value, "publish")?)
                .ok_or_else(|| malformed("publish"))?,
            epoch: number(&value, "epoch")?,
            created: value["created"]
                .as_bool()
                .ok_or_else(|| malformed("created"))?,
        })
    }
}
