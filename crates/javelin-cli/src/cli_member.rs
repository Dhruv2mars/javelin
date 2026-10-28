//! `javelin member list|add|role|remove|invite` and `javelin repo settings`: a repository's
//! members and the policy the control plane pushes with them. REPOSITORY is `OWNER/NAME` or the
//! repository's ID. The caller is this device's sign-in, or the admin token in
//! `JAVELIN_ADMIN_TOKEN`; changes need the maintain role. The control plane pushes each change
//! to the repository's node, which applies it from its next request, with no restart.
//!
//! Without `JAVELIN_CONTROL_URL` the control plane runs in process, and a repository on a
//! Cloudflare node is pushed to with `JAVELIN_NODE_ADMIN_TOKEN`, as `repo create` does.

use std::env;

use javelin_collaboration::Role;
use javelin_control::{
    Control, ControlError, ControlPlane, MemberChange, Members, NodeTarget, RemoteControl,
};
use javelin_node_worker::CloudflareNode;
use serde_json::json;

use crate::{
    cli::Output,
    cli_control::{bearer, control_dir, now},
};

const USAGE: &str = "usage:\n  javelin member list REPOSITORY\n  javelin member add REPOSITORY ACCOUNT [--role read|review|maintain]\n  javelin member role REPOSITORY ACCOUNT read|review|maintain\n  javelin member remove REPOSITORY ACCOUNT\n  javelin member invite REPOSITORY ACCOUNT [--role read|review|maintain]\nREPOSITORY is OWNER/NAME or the repository's ID.";

pub fn run(args: Vec<std::ffi::OsString>) -> Result<Output, ControlError> {
    let args = args
        .iter()
        .map(|arg| {
            arg.to_str()
                .map(str::to_owned)
                .ok_or_else(|| ControlError::InvalidInput("arguments must be UTF-8".into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["list", repository] => list(repository),
        ["add", repository, account, flags @ ..] => {
            change(repository, account, MemberChange::Add(role_flag(flags)?))
        }
        ["role", repository, account, role] => {
            change(repository, account, MemberChange::Role(parse_role(role)?))
        }
        ["remove", repository, account] => change(repository, account, MemberChange::Remove),
        ["invite", repository, account, flags @ ..] => {
            invite(repository, account, role_flag(flags)?)
        }
        _ => Err(ControlError::InvalidInput(USAGE.into())),
    }
}

fn parse_role(name: &str) -> Result<Role, ControlError> {
    Role::parse(name).ok_or_else(|| {
        ControlError::InvalidInput(format!("a role is read, review or maintain, got {name}"))
    })
}

/// `[--role ROLE]`, read by default.
fn role_flag(flags: &[&str]) -> Result<Role, ControlError> {
    match flags {
        [] => Ok(Role::Read),
        ["--role", role] => parse_role(role),
        _ => Err(ControlError::InvalidInput(USAGE.into())),
    }
}

/// The control plane that can push to `repository`'s node: over HTTP at `JAVELIN_CONTROL_URL`,
/// else in process, reaching a Cloudflare node with `JAVELIN_NODE_ADMIN_TOKEN`.
fn plane_for(repository: &str) -> Result<Box<dyn Control>, ControlError> {
    if let Ok(url) = env::var("JAVELIN_CONTROL_URL") {
        return Ok(Box::new(RemoteControl::new(&url)?));
    }
    let dir = control_dir()?;
    let plane = ControlPlane::open(&dir)?;
    match plane.node_target(repository) {
        Ok(target) if target.provider == "cloudflare" => {
            let url = target.url.clone().ok_or_else(|| {
                ControlError::Storage("a cloudflare repository has no node URL".into())
            })?;
            let token = env::var("JAVELIN_NODE_ADMIN_TOKEN").map_err(|_| {
                ControlError::Unauthorized(
                    "this repository is on a cloudflare node: set JAVELIN_NODE_ADMIN_TOKEN to its admin token",
                )
            })?;
            let node = CloudflareNode::new(&url, &token)?;
            Ok(Box::new(ControlPlane::with_node(
                &dir,
                Box::new(node),
                NodeTarget::cloudflare(&url),
            )?))
        }
        // An unknown repository is refused by the call itself, with the caller checked first.
        _ => Ok(Box::new(plane)),
    }
}

fn list(repository: &str) -> Result<Output, ControlError> {
    let plane = plane_for(repository)?;
    let now = now()?;
    let members = plane.members(&bearer(plane.as_ref(), now)?, repository, now)?;
    let mut text = format!(
        "{}/{} ({}), epoch {}. Publish needs {} approval(s).\n",
        members.owner, members.name, members.repository, members.epoch, members.required_approvals
    );
    for member in &members.members {
        text.push_str(&format!("  {} {}\n", member.principal, member.role.name()));
    }
    if !members.invites.is_empty() {
        text.push_str("Pending invites:\n");
        for invite in &members.invites {
            text.push_str(&format!(
                "  {} {} (expires at {})\n",
                invite.principal,
                invite.role.name(),
                invite.expires_at
            ));
        }
    }
    Ok(output("member list", &members, text.trim_end().to_string()))
}

fn change(repository: &str, account: &str, change: MemberChange) -> Result<Output, ControlError> {
    let plane = plane_for(repository)?;
    let now = now()?;
    let members = plane.change_member(
        &bearer(plane.as_ref(), now)?,
        repository,
        account,
        change,
        now,
    )?;
    let slug = format!("{}/{}", members.owner, members.name);
    let held = members
        .members
        .iter()
        .find(|member| member.principal == account)
        .map(|member| member.role.name());
    let what = match held {
        Some(role) => format!("{account} holds {role} in {slug}"),
        None => format!("{account} holds no role in {slug}"),
    };
    let text = if members.changed {
        format!(
            "{what} (epoch {}). The node applies it from its next request.",
            members.epoch
        )
    } else {
        format!("{what} already; nothing changed (epoch {}).", members.epoch)
    };
    let command = match change {
        MemberChange::Add(_) => "member add",
        MemberChange::Role(_) => "member role",
        MemberChange::Remove => "member remove",
    };
    Ok(output(command, &members, text))
}

fn invite(repository: &str, account: &str, role: Role) -> Result<Output, ControlError> {
    let plane = plane_for(repository)?;
    let now = now()?;
    let invite = plane.invite(
        &bearer(plane.as_ref(), now)?,
        repository,
        account,
        role,
        now,
    )?;
    let text = format!(
        "Invited {account} to {repository} as {}. Give them this code; it is shown once and works once, for 7 days:\n  {}\nThey sign in with `javelin login --invite CODE`.",
        role.name(),
        invite.code
    );
    Ok((
        json!({
            "ok": true,
            "command": "member invite",
            "repository": invite.repository,
            "principal": invite.principal,
            "role": invite.role.name(),
            "code": invite.code,
            "expires_at": invite.expires_at,
        }),
        text,
        0,
    ))
}

/// `javelin repo settings REPOSITORY --required-approvals N`.
pub fn settings(repository: &str, count: &str) -> Result<Output, ControlError> {
    let required: u32 = count.parse().map_err(|_| {
        ControlError::InvalidInput(format!(
            "--required-approvals takes a whole number, got {count}"
        ))
    })?;
    let plane = plane_for(repository)?;
    let now = now()?;
    let members =
        plane.set_required_approvals(&bearer(plane.as_ref(), now)?, repository, required, now)?;
    let text = format!(
        "{}/{}: Publish needs {} approval(s) (epoch {}).",
        members.owner, members.name, members.required_approvals, members.epoch
    );
    Ok(output("repo settings", &members, text))
}

fn output(command: &str, members: &Members, text: String) -> Output {
    let mut value = members.to_json();
    value["ok"] = json!(true);
    value["command"] = json!(command);
    (value, text, 0)
}
