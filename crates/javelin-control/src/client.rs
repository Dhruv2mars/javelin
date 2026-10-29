//! The control plane as a client sees it. [`Control`] is what the CLI needs: device sign-in,
//! token refresh, repository tokens, repository create, and members and invites. [`ControlPlane`] serves it in
//! process; [`RemoteControl`] calls the control plane's HTTP API (`browser.rs`, `/api/`) over
//! HTTPS, or plain HTTP on this machine only.
//! A control plane that does not answer, or answers 5xx without a refusal of its own, is
//! `control_plane_unavailable` (`docs/online-design.md` sections 4.4 and 5).

use std::time::Duration;

use javelin_collaboration::Role;
use serde_json::{Value, json};

use crate::{
    AccessToken, ControlError, ControlPlane, CreateRequest, Created, DeviceStart, Endpoint, Invite,
    Member, MemberChange, Members, PendingInvite, Poll, Publish, Redeemed, Session,
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
    fn members(&self, bearer: &str, repository: &str, now: u64) -> Result<Members, ControlError>;
    fn change_member(
        &self,
        bearer: &str,
        repository: &str,
        principal: &str,
        change: MemberChange,
        now: u64,
    ) -> Result<Members, ControlError>;
    fn set_required_approvals(
        &self,
        bearer: &str,
        repository: &str,
        required: u32,
        now: u64,
    ) -> Result<Members, ControlError>;
    fn invite(
        &self,
        bearer: &str,
        repository: &str,
        principal: &str,
        role: Role,
        now: u64,
    ) -> Result<Invite, ControlError>;
    /// Redeems an invite code. It needs no other credential: the code is the credential.
    fn redeem_invite(&self, code: &str, now: u64) -> Result<Redeemed, ControlError>;
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
    fn members(&self, bearer: &str, repository: &str, now: u64) -> Result<Members, ControlError> {
        ControlPlane::members(self, bearer, repository, now)
    }
    fn change_member(
        &self,
        bearer: &str,
        repository: &str,
        principal: &str,
        change: MemberChange,
        now: u64,
    ) -> Result<Members, ControlError> {
        ControlPlane::change_member(self, bearer, repository, principal, change, now)
    }
    fn set_required_approvals(
        &self,
        bearer: &str,
        repository: &str,
        required: u32,
        now: u64,
    ) -> Result<Members, ControlError> {
        ControlPlane::set_required_approvals(self, bearer, repository, required, now)
    }
    fn invite(
        &self,
        bearer: &str,
        repository: &str,
        principal: &str,
        role: Role,
        now: u64,
    ) -> Result<Invite, ControlError> {
        ControlPlane::invite(self, bearer, repository, principal, role, now)
    }
    fn redeem_invite(&self, code: &str, now: u64) -> Result<Redeemed, ControlError> {
        ControlPlane::redeem_invite(self, code, now)
    }
}

/// A control plane reached over HTTPS, or plain HTTP on this machine (`transport.rs`).
pub struct RemoteControl {
    endpoint: Endpoint,
    agent: ureq::Agent,
    /// No call runs past this instant.
    deadline: Option<std::time::Instant>,
}

impl RemoteControl {
    /// A client for the control plane at `url`, `https://HOST[:PORT]` (plain `http` only for
    /// localhost), with an optional trailing slash. Nothing is sent until the first call.
    pub fn new(url: &str) -> Result<Self, ControlError> {
        let endpoint = Endpoint::parse(url)
            .map_err(|detail| ControlError::InvalidInput(format!("control plane URL: {detail}")))?;
        let agent = endpoint.agent(TIMEOUT);
        Ok(Self {
            endpoint,
            agent,
            deadline: None,
        })
    }

    /// As [`RemoteControl::new`], with every call giving up at `deadline`.
    pub fn until(url: &str, deadline: std::time::Instant) -> Result<Self, ControlError> {
        Ok(Self {
            deadline: Some(deadline),
            ..Self::new(url)?
        })
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
        if let Some(deadline) = self.deadline {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Err(unavailable(
                    origin,
                    "was not asked: the time budget is spent",
                ));
            }
            request = request.config().timeout_global(Some(left)).build();
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

fn role(value: &Value, key: &str) -> Result<Role, ControlError> {
    Role::parse(&text(value, key)?).ok_or_else(|| malformed(key))
}

/// A change as the API names it: `add`, `role` or `remove`, with the role for the first two.
pub(crate) fn change_json(change: MemberChange) -> Value {
    match change {
        MemberChange::Add(role) => json!({"change": "add", "role": role.name()}),
        MemberChange::Role(role) => json!({"change": "role", "role": role.name()}),
        MemberChange::Remove => json!({"change": "remove"}),
    }
}

fn parse_members(value: &Value) -> Result<Members, ControlError> {
    let list = |key: &str| value[key].as_array().cloned().ok_or_else(|| malformed(key));
    Ok(Members {
        repository: text(value, "repository")?,
        owner: text(value, "owner")?,
        name: text(value, "name")?,
        epoch: number(value, "epoch")?,
        required_approvals: u32::try_from(number(value, "required_approvals")?)
            .map_err(|_| malformed("required_approvals"))?,
        members: list("members")?
            .iter()
            .map(|member| {
                Ok(Member {
                    principal: text(member, "principal")?,
                    role: role(member, "role")?,
                })
            })
            .collect::<Result<_, ControlError>>()?,
        invites: list("invites")?
            .iter()
            .map(|invite| {
                Ok(PendingInvite {
                    principal: text(invite, "principal")?,
                    role: role(invite, "role")?,
                    expires_at: number(invite, "expires_at")?,
                })
            })
            .collect::<Result<_, ControlError>>()?,
        changed: value["changed"]
            .as_bool()
            .ok_or_else(|| malformed("changed"))?,
    })
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
            node_url: value["node_url"].as_str().map(str::to_owned),
        })
    }

    fn members(&self, bearer: &str, repository: &str, _now: u64) -> Result<Members, ControlError> {
        parse_members(&self.call(
            "/api/members/list",
            Some(bearer),
            &json!({"repository": repository}),
        )?)
    }

    fn change_member(
        &self,
        bearer: &str,
        repository: &str,
        principal: &str,
        change: MemberChange,
        _now: u64,
    ) -> Result<Members, ControlError> {
        let mut body = change_json(change);
        body["repository"] = json!(repository);
        body["principal"] = json!(principal);
        parse_members(&self.call("/api/members/change", Some(bearer), &body)?)
    }

    fn set_required_approvals(
        &self,
        bearer: &str,
        repository: &str,
        required: u32,
        _now: u64,
    ) -> Result<Members, ControlError> {
        parse_members(&self.call(
            "/api/repo/settings",
            Some(bearer),
            &json!({"repository": repository, "required_approvals": required}),
        )?)
    }

    fn invite(
        &self,
        bearer: &str,
        repository: &str,
        principal: &str,
        role: Role,
        _now: u64,
    ) -> Result<Invite, ControlError> {
        let value = self.call(
            "/api/members/invite",
            Some(bearer),
            &json!({"repository": repository, "principal": principal, "role": role.name()}),
        )?;
        Ok(Invite {
            code: text(&value, "code")?,
            repository: text(&value, "repository")?,
            principal: text(&value, "principal")?,
            role: self::role(&value, "role")?,
            expires_at: number(&value, "expires_at")?,
        })
    }

    fn redeem_invite(&self, code: &str, _now: u64) -> Result<Redeemed, ControlError> {
        let value = self.call("/api/invite/redeem", None, &json!({"code": code}))?;
        Ok(Redeemed {
            session: Session {
                principal: text(&value, "principal")?,
                device: text(&value, "device")?,
                access_token: text(&value, "access_token")?,
                refresh_token: text(&value, "refresh_token")?,
                expires_in: number(&value, "expires_in")?,
            },
            repository: text(&value, "repository")?,
            role: self::role(&value, "role")?,
            node_error: value["node_error"].as_str().map(str::to_owned),
        })
    }
}
