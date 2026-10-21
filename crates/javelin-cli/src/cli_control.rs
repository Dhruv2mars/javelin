//! `javelin login`, `javelin control` and `javelin repo create`: the first slice of the
//! control plane's commands. State lives under `JAVELIN_CONTROL_DIR` (default
//! `$JAVELIN_HOME/control`), and this device's sign-in under `$JAVELIN_HOME/credentials`.
//! `JAVELIN_HOME` defaults to `~/.javelin`. See `docs/online-design.md` section 4.7 (D5).

use std::{
    env,
    ffi::OsString,
    path::PathBuf,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use javelin_control::{
    ControlError, ControlPlane, CreateRequest, Credentials, NodeTarget, Poll, Publish,
};
use javelin_node_worker::CloudflareNode;
use serde_json::json;

use crate::cli::Output;

const USAGE: &str = "usage:\n  javelin control init\n  javelin control approve USER-CODE   (needs JAVELIN_ADMIN_TOKEN)\n  javelin login\n  javelin repo create OWNER/NAME --operation ID [--visibility public|private] [--publish direct|requests] [--provider self|cloudflare] [--node-url URL]";

pub fn run(args: Vec<OsString>) -> Result<Output, ControlError> {
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
        ["login"] => login(),
        ["control", "init"] => control_init(),
        ["control", "approve", code] => control_approve(code),
        ["repo", "create", slug, flags @ ..] => repo_create(slug, flags),
        _ => Err(ControlError::InvalidInput(USAGE.into())),
    }
}

fn control_init() -> Result<Output, ControlError> {
    let dir = control_dir()?;
    let token = ControlPlane::open(&dir)?.bootstrap()?;
    let text = format!(
        "Admin token (shown once; it is not stored, only its hash is):\n  {token}\nControl plane ready at {}",
        dir.display()
    );
    Ok((
        json!({"ok": true, "command": "control init", "admin_token": token, "control_dir": dir}),
        text,
        0,
    ))
}

fn control_approve(code: &str) -> Result<Output, ControlError> {
    let plane = open()?;
    plane.device_approve(&admin_token()?, code, now()?)?;
    let text = format!("Approved {code}. The waiting `javelin login` will finish.");
    Ok((
        json!({"ok": true, "command": "control approve", "user_code": code}),
        text,
        0,
    ))
}

fn login() -> Result<Output, ControlError> {
    let plane = open()?;
    let start = plane.device_start(now()?)?;
    eprintln!(
        "To sign in, approve this code with the admin token:\n  JAVELIN_ADMIN_TOKEN=... javelin control approve {}\nCode expires in 15 minutes. Waiting for approval...",
        start.user_code
    );
    let session = loop {
        match plane.device_poll(&start.device_code, now()?)? {
            Poll::Approved(session) => break session,
            Poll::Pending => thread::sleep(Duration::from_secs(start.interval)),
        }
    };
    let credentials = Credentials {
        principal: session.principal.clone(),
        device: session.device.clone(),
        refresh_token: session.refresh_token,
    };
    let path = credentials_path()?;
    credentials.save(&path)?;
    let text = format!(
        "Signed in as {} on device {}. Credentials saved to {}.",
        session.principal,
        session.device,
        path.display()
    );
    Ok((
        json!({"ok": true, "command": "login", "principal": session.principal, "device": session.device}),
        text,
        0,
    ))
}

fn repo_create(slug: &str, flags: &[&str]) -> Result<Output, ControlError> {
    let (owner, name) = slug
        .split_once('/')
        .ok_or_else(|| ControlError::InvalidInput(format!("expected OWNER/NAME, got {slug}")))?;
    let mut operation = None;
    let mut public = false;
    let mut publish = Publish::Direct;
    let mut cloudflare = false;
    let mut node_url = None;
    let mut flags = flags.iter();
    while let Some(flag) = flags.next() {
        let mut value = || {
            flags
                .next()
                .copied()
                .ok_or_else(|| ControlError::InvalidInput(format!("{flag} needs a value")))
        };
        match *flag {
            "--operation" => operation = Some(value()?.to_owned()),
            "--visibility" => {
                public = match value()? {
                    "public" => true,
                    "private" => false,
                    other => {
                        return Err(ControlError::InvalidInput(format!(
                            "--visibility is public or private, got {other}"
                        )));
                    }
                }
            }
            "--publish" => {
                publish = Publish::parse(value()?).ok_or_else(|| {
                    ControlError::InvalidInput("--publish is direct or requests".into())
                })?;
            }
            "--provider" => match value()? {
                "self" => cloudflare = false,
                "cloudflare" => cloudflare = true,
                other => {
                    return Err(ControlError::InvalidInput(format!(
                        "--provider is self or cloudflare, got {other}"
                    )));
                }
            },
            "--node-url" => node_url = Some(value()?.to_owned()),
            other => {
                return Err(ControlError::InvalidInput(format!(
                    "unknown flag {other}\n{USAGE}"
                )));
            }
        }
    }
    let operation = operation.ok_or_else(|| {
        ControlError::InvalidInput(
            "repo create needs --operation ID; reuse it when retrying".into(),
        )
    })?;
    let plane = if cloudflare {
        let url = node_url.ok_or_else(|| {
            ControlError::InvalidInput("--provider cloudflare needs --node-url URL".into())
        })?;
        let token = env::var("JAVELIN_NODE_ADMIN_TOKEN").map_err(|_| {
            ControlError::Unauthorized("set JAVELIN_NODE_ADMIN_TOKEN to the node's admin token")
        })?;
        let node = CloudflareNode::new(&url, &token)?;
        ControlPlane::with_node(
            &control_dir()?,
            Box::new(node),
            NodeTarget::cloudflare(&url),
        )?
    } else {
        if node_url.is_some() {
            return Err(ControlError::InvalidInput(
                "--node-url needs --provider cloudflare".into(),
            ));
        }
        open()?
    };
    let now = now()?;
    let bearer = match env::var("JAVELIN_ADMIN_TOKEN") {
        Ok(token) => token,
        Err(_) => {
            let credentials = Credentials::load(&credentials_path()?)?;
            plane.refresh(&credentials.refresh_token, now)?.access_token
        }
    };
    let created = plane.create_repository(
        &bearer,
        &CreateRequest {
            owner: owner.into(),
            name: name.into(),
            public,
            publish,
            operation: operation.clone(),
        },
        now,
    )?;
    let visibility = if created.public { "public" } else { "private" };
    let text = if created.created {
        format!(
            "Repository {owner}/{name} ready: {} ({visibility}, publish {}, epoch {}, empty World 0).",
            created.repository,
            created.publish.as_str(),
            created.epoch
        )
    } else {
        format!(
            "Operation {operation} already created {owner}/{name}: {} (epoch {}).",
            created.repository, created.epoch
        )
    };
    Ok((
        json!({
            "ok": true,
            "command": "repo create",
            "repository": created.repository,
            "owner": owner,
            "name": name,
            "visibility": visibility,
            "publish": created.publish.as_str(),
            "epoch": created.epoch,
            "created": created.created,
        }),
        text,
        0,
    ))
}

fn open() -> Result<ControlPlane, ControlError> {
    ControlPlane::open(&control_dir()?)
}

fn admin_token() -> Result<String, ControlError> {
    env::var("JAVELIN_ADMIN_TOKEN").map_err(|_| {
        ControlError::Unauthorized(
            "set JAVELIN_ADMIN_TOKEN to the admin token from `javelin control init`",
        )
    })
}

fn now() -> Result<u64, ControlError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|_| ControlError::Storage("system clock is before 1970".into()))
}

fn javelin_home() -> Result<PathBuf, ControlError> {
    if let Some(dir) = env::var_os("JAVELIN_HOME") {
        return Ok(dir.into());
    }
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .ok_or_else(|| ControlError::InvalidInput("set JAVELIN_HOME or HOME".into()))?;
    Ok(PathBuf::from(home).join(".javelin"))
}

fn control_dir() -> Result<PathBuf, ControlError> {
    match env::var_os("JAVELIN_CONTROL_DIR") {
        Some(dir) => Ok(dir.into()),
        None => Ok(javelin_home()?.join("control")),
    }
}

fn credentials_path() -> Result<PathBuf, ControlError> {
    Ok(javelin_home()?.join("credentials"))
}
