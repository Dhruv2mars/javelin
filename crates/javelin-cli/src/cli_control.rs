//! `javelin login`, `javelin control` and `javelin repo create`: the first slice of the
//! control plane's commands. With `JAVELIN_CONTROL_URL` set, `login`, `control approve` and
//! `repo create` call the control plane's HTTP API there, which `javelin control serve`
//! provides; otherwise they open its state in process under `JAVELIN_CONTROL_DIR` (default
//! `$JAVELIN_HOME/control`). A control plane that cannot be reached either way is
//! `control_plane_unavailable`. This device's sign-in lives in `$JAVELIN_HOME/credentials`.
//! `JAVELIN_HOME` defaults to `~/.javelin`. See `docs/online-design.md` section 4.7 (D5).

use std::{
    env,
    ffi::OsString,
    path::PathBuf,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use javelin_control::{
    Control, ControlError, ControlPlane, CreateRequest, Credentials, NodeTarget, Poll, Publish,
    RelyingParty, RemoteControl,
};
use javelin_node_worker::CloudflareNode;
use serde_json::json;

use crate::cli::Output;

const USAGE: &str = "usage:\n  javelin control init\n  javelin control serve IP:PORT [--origin https://HOST]   (non-loopback IP only behind TLS)\n  javelin control approve USER-CODE   (needs JAVELIN_ADMIN_TOKEN)\n  javelin login\n  javelin repo create OWNER/NAME --operation ID [--visibility public|private] [--publish direct|requests] [--provider self|cloudflare] [--node-url URL]";

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
        ["control", "serve", addr] => control_serve(addr, None),
        ["control", "serve", addr, "--origin", origin] => control_serve(addr, Some(origin)),
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

/// Serves the approval page and the HTTP API on `addr` until the process is stopped. The
/// passkey origin defaults to `http://localhost:PORT`. The server speaks plain HTTP, so it
/// binds a non-loopback address only behind a TLS-terminating proxy, named by an https
/// `--origin`; otherwise tokens would cross the network in clear.
fn control_serve(addr: &str, origin: Option<&str>) -> Result<Output, ControlError> {
    let parsed: std::net::SocketAddr = addr
        .parse()
        .map_err(|_| ControlError::InvalidInput(format!("expected IP:PORT, got {addr}")))?;
    let behind_tls = origin.is_some_and(|origin| {
        origin
            .get(..8)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
    });
    if !parsed.ip().is_loopback() && !behind_tls {
        return Err(ControlError::InvalidInput(format!(
            "{addr} is not a loopback address, and this server speaks plain HTTP: bind 127.0.0.1, or put it behind a TLS-terminating proxy and pass that proxy's address as --origin https://HOST"
        )));
    }
    let plane = ControlPlane::open(&control_dir()?)?;
    let listener = std::net::TcpListener::bind(addr)
        .map_err(|error| ControlError::InvalidInput(format!("cannot listen on {addr}: {error}")))?;
    let local = listener
        .local_addr()
        .map_err(|error| ControlError::InvalidInput(error.to_string()))?;
    let origin = match origin {
        Some(origin) => origin.to_owned(),
        None => format!("http://localhost:{}", local.port()),
    };
    let rp = RelyingParty::new(&origin)?;
    eprintln!("javelin control plane serving on {local} (passkey origin {origin})");
    javelin_control::serve(&plane, &rp, &listener)
        .map_err(|error| ControlError::InvalidInput(error.to_string()))?;
    Ok((
        json!({"ok": true, "command": "control serve"}),
        String::new(),
        0,
    ))
}

fn control_approve(code: &str) -> Result<Output, ControlError> {
    control()?.device_approve_as(&admin_token()?, code, now()?)?;
    let text = format!("Approved {code}. The waiting `javelin login` will finish.");
    Ok((
        json!({"ok": true, "command": "control approve", "user_code": code}),
        text,
        0,
    ))
}

fn login() -> Result<Output, ControlError> {
    let plane = control()?;
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
    // The new sign-in caches its tokens in its own directory, so the previous sign-in's tokens
    // are never sent for it. Tidying old directories is best effort.
    let _ = crate::cli_auth::remove_stale_sign_ins(&credentials);
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
    let remote = env::var("JAVELIN_CONTROL_URL").is_ok();
    if remote && (cloudflare || node_url.is_some()) {
        return Err(ControlError::InvalidInput(
            "the control plane at JAVELIN_CONTROL_URL chooses the provider; drop --provider and --node-url".into(),
        ));
    }
    let plane: Box<dyn Control> = if cloudflare {
        let url = node_url.ok_or_else(|| {
            ControlError::InvalidInput("--provider cloudflare needs --node-url URL".into())
        })?;
        let token = env::var("JAVELIN_NODE_ADMIN_TOKEN").map_err(|_| {
            ControlError::Unauthorized("set JAVELIN_NODE_ADMIN_TOKEN to the node's admin token")
        })?;
        let node = CloudflareNode::new(&url, &token)?;
        Box::new(ControlPlane::with_node(
            &control_dir()?,
            Box::new(node),
            NodeTarget::cloudflare(&url),
        )?)
    } else {
        if node_url.is_some() {
            return Err(ControlError::InvalidInput(
                "--node-url needs --provider cloudflare".into(),
            ));
        }
        control()?
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
        let mut text = format!(
            "Repository {owner}/{name} ready: {} ({visibility}, publish {}, epoch {}, empty World 0).",
            created.repository,
            created.publish.as_str(),
            created.epoch
        );
        // A self-hosted repository lives in the control plane's directory until it is served.
        if !cloudflare {
            let id = &created.repository;
            let dir = if remote {
                format!("CONTROL_DIR/nodes/{id}")
            } else {
                control_dir()?.join("nodes").join(id).display().to_string()
            };
            text.push_str(&format!(
                "\nServe it on the control plane's machine with\n  javelin-node serve {dir} {id} \
                 127.0.0.1:PORT\nthen attach with `javelin attach http://127.0.0.1:PORT/{id} DIR`."
            ));
        }
        text
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

/// The control plane: over HTTP at `JAVELIN_CONTROL_URL`, else in process at the control
/// directory.
pub(crate) fn control() -> Result<Box<dyn Control>, ControlError> {
    control_until(None)
}

/// [`control`], whose calls over HTTP give up at `deadline`, when there is one.
pub(crate) fn control_until(
    deadline: Option<std::time::Instant>,
) -> Result<Box<dyn Control>, ControlError> {
    match (env::var("JAVELIN_CONTROL_URL"), deadline) {
        (Ok(url), Some(deadline)) => Ok(Box::new(RemoteControl::until(&url, deadline)?)),
        (Ok(url), None) => Ok(Box::new(RemoteControl::new(&url)?)),
        (Err(_), _) => Ok(Box::new(ControlPlane::open(&control_dir()?)?)),
    }
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

pub(crate) fn javelin_home() -> Result<PathBuf, ControlError> {
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

pub(crate) fn credentials_path() -> Result<PathBuf, ControlError> {
    Ok(javelin_home()?.join("credentials"))
}
