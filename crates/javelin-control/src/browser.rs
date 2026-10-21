//! The browser approval page and the JSON calls it makes (U8b). `answer` handles one HTTP/1.1
//! request on a connection; `serve` loops over a listener. The page is `approval.html`, and
//! its passkey script converts the server's base64url options to the browser's byte arrays.

use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use webauthn_rs::prelude::{PublicKeyCredential, RegisterPublicKeyCredential};

use crate::{ControlError, ControlPlane, RelyingParty, passkey::BROWSER_TTL};

const PAGE: &str = include_str!("approval.html");
/// The cookie that carries a browser sign-in.
const COOKIE: &str = "javelin_web";
/// Largest request body: a credential and a little JSON.
const MAX_BODY: usize = 64 * 1024;
/// Largest whole request, headers included. Reads stop here, so a client cannot run on.
const MAX_REQUEST: u64 = 80 * 1024;
/// How long a client may stall before its connection is dropped.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Serves the approval page and its calls on `listener`, one connection at a time, forever.
pub fn serve(plane: &ControlPlane, rp: &RelyingParty, listener: &TcpListener) -> io::Result<()> {
    for stream in listener.incoming().flatten() {
        let _ = answer(plane, rp, stream);
    }
    Ok(())
}

/// Answers one request on `stream` and closes it. Protocol errors get a 4xx reply.
pub fn answer(plane: &ControlPlane, rp: &RelyingParty, stream: TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    let reply = {
        let mut reader = BufReader::new((&stream).take(MAX_REQUEST));
        match read_request(&mut reader) {
            Ok(request) => route(plane, rp, &request, now()),
            Err(reply) => reply,
        }
    };
    let mut out = &stream;
    write_reply(&mut out, &reply)
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct Reply {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    cookie: Option<String>,
}

impl Reply {
    fn html(text: &str) -> Self {
        Self {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: text.as_bytes().to_vec(),
            cookie: None,
        }
    }

    fn json(status: u16, value: Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: value.to_string().into_bytes(),
            cookie: None,
        }
    }

    fn error(status: u16, code: &str, message: &str) -> Self {
        Self::json(status, json!({"error": code, "message": message}))
    }

    fn from_error(error: ControlError) -> Self {
        Self::error(status_of(&error), error.code(), &error.to_string())
    }
}

fn read_request(reader: &mut impl BufRead) -> Result<Request, Reply> {
    let bad = || Reply::error(400, "bad_request", "the request is not HTTP/1.1");
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|_| bad())?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return Err(bad());
    };
    // The page reads `?code=` itself, so routing only needs the path.
    let path = path.split('?').next().unwrap_or_default();
    let (method, path) = (method.to_string(), path.to_string());
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|_| bad())?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').ok_or_else(bad)?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    let length = match header(&headers, "content-length") {
        Some(text) => text.parse::<usize>().map_err(|_| bad())?,
        None => 0,
    };
    if length > MAX_BODY {
        return Err(Reply::error(
            413,
            "too_large",
            "the request body is larger than 64 KiB",
        ));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).map_err(|_| bad())?;
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

fn route(plane: &ControlPlane, rp: &RelyingParty, request: &Request, now: u64) -> Reply {
    if request.method == "GET" && request.path == "/" {
        return Reply::html(PAGE);
    }
    // A browser sends Origin on every POST. Refusing other origins blocks cross-site posts.
    if request.method == "POST" {
        let expected = rp.origin().origin().ascii_serialization();
        if header(&request.headers, "origin").is_some_and(|origin| origin != expected) {
            return Reply::error(403, "forbidden", "the request came from another origin");
        }
    }
    let outcome = match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/passkey/enroll/start") => enroll_start(plane, rp, request, now),
        ("POST", "/passkey/enroll/finish") => enroll_finish(plane, rp, request, now),
        ("POST", "/passkey/signin/start") => signin_start(plane, rp, now),
        ("POST", "/passkey/signin/finish") => signin_finish(plane, rp, request, now),
        ("POST", "/device/approve") => approve(plane, request, now),
        _ => return Reply::error(404, "not_found", "there is no such page or call"),
    };
    outcome.unwrap_or_else(Reply::from_error)
}

#[derive(Deserialize)]
struct CeremonyBody {
    ceremony: String,
    credential: Value,
}

#[derive(Deserialize)]
struct ApprovalBody {
    user_code: String,
}

fn enroll_start(
    plane: &ControlPlane,
    rp: &RelyingParty,
    request: &Request,
    now: u64,
) -> Result<Reply, ControlError> {
    let bearer = bearer(request).ok_or(ControlError::Unauthorized(
        "send the admin token, or sign in with your passkey first",
    ))?;
    let (ceremony, options) = plane.passkey_enroll_start(rp, &bearer, now)?;
    Ok(Reply::json(
        200,
        json!({"ceremony": ceremony, "options": options}),
    ))
}

fn enroll_finish(
    plane: &ControlPlane,
    rp: &RelyingParty,
    request: &Request,
    now: u64,
) -> Result<Reply, ControlError> {
    let body: CeremonyBody = parse(request)?;
    let credential: RegisterPublicKeyCredential = from_value(body.credential)?;
    plane.passkey_enroll_finish(rp, &body.ceremony, &credential, now)?;
    Ok(Reply::json(200, json!({"ok": true})))
}

fn signin_start(plane: &ControlPlane, rp: &RelyingParty, now: u64) -> Result<Reply, ControlError> {
    let (ceremony, options) = plane.passkey_signin_start(rp, now)?;
    Ok(Reply::json(
        200,
        json!({"ceremony": ceremony, "options": options}),
    ))
}

fn signin_finish(
    plane: &ControlPlane,
    rp: &RelyingParty,
    request: &Request,
    now: u64,
) -> Result<Reply, ControlError> {
    let body: CeremonyBody = parse(request)?;
    let credential: PublicKeyCredential = from_value(body.credential)?;
    let session = plane.passkey_signin_finish(rp, &body.ceremony, &credential, now)?;
    let secure = if rp.origin().scheme() == "https" {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "{COOKIE}={}; HttpOnly; SameSite=Strict; Path=/; Max-Age={BROWSER_TTL}{secure}",
        session.token
    );
    let mut reply = Reply::json(200, json!({"ok": true, "expires_in": session.expires_in}));
    reply.cookie = Some(cookie);
    Ok(reply)
}

fn approve(plane: &ControlPlane, request: &Request, now: u64) -> Result<Reply, ControlError> {
    let body: ApprovalBody = parse(request)?;
    let bearer = bearer(request).ok_or(ControlError::Unauthorized(
        "sign in with your passkey first",
    ))?;
    plane.device_approve_as(&bearer, &body.user_code, now)?;
    Ok(Reply::json(
        200,
        json!({"ok": true, "user_code": body.user_code}),
    ))
}

/// The caller's credential: an `Authorization: Bearer` token, or the browser sign-in cookie.
fn bearer(request: &Request) -> Option<String> {
    if let Some(token) =
        header(&request.headers, "authorization").and_then(|value| value.strip_prefix("Bearer "))
    {
        return Some(token.trim().to_string());
    }
    header(&request.headers, "cookie")?
        .split(';')
        .find_map(|pair| pair.trim().strip_prefix(COOKIE)?.strip_prefix('='))
        .map(str::to_string)
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn parse<T: DeserializeOwned>(request: &Request) -> Result<T, ControlError> {
    serde_json::from_slice(&request.body).map_err(|error| {
        ControlError::InvalidInput(format!(
            "the request body is not the expected JSON: {error}"
        ))
    })
}

fn from_value<T: DeserializeOwned>(value: Value) -> Result<T, ControlError> {
    serde_json::from_value(value).map_err(|error| {
        ControlError::InvalidInput(format!("the passkey response is malformed: {error}"))
    })
}

/// The HTTP status for a control refusal.
fn status_of(error: &ControlError) -> u16 {
    match error {
        ControlError::NotInitialized
        | ControlError::AlreadyInitialized
        | ControlError::NameTaken
        | ControlError::OperationReused
        | ControlError::Redeemed => 409,
        ControlError::Unauthorized(_) => 401,
        ControlError::Forbidden(_) => 403,
        ControlError::InvalidInput(_) => 400,
        ControlError::NotFound(_) => 404,
        ControlError::Expired(_) => 410,
        ControlError::Node(_) => 503,
        ControlError::Refused(_) => 502,
        ControlError::Storage(_) => 500,
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        410 => "Gone",
        413 => "Payload Too Large",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}

fn write_reply(out: &mut impl Write, reply: &Reply) -> io::Result<()> {
    write!(
        out,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        reply.status,
        reason(reply.status),
        reply.content_type,
        reply.body.len()
    )?;
    if let Some(cookie) = &reply.cookie {
        write!(out, "Set-Cookie: {cookie}\r\n")?;
    }
    out.write_all(b"\r\n")?;
    out.write_all(&reply.body)?;
    out.flush()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}
