//! A gate for wire tests: the node trusts the public test key, and `owner` holds Maintain on a
//! public repository. Requests carry the owner's token as `Authorization: Bearer`.

#![allow(dead_code)] // each test binary uses a different subset

use std::{
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_identity::{Scope, test_keys};
use javelin_node::{FileWorldLog, auth::Gate, serve, serve_live};

pub const OWNER: &str = "owner";
const ALL: [Scope; 5] = [
    Scope::Read,
    Scope::LayerWrite,
    Scope::Publish,
    Scope::ReviewWrite,
    Scope::ReviewApprove,
];

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

pub fn permissions(repository: &str, visibility: Visibility) -> AppliedPermissions {
    AppliedPermissions {
        repository: RepositoryId::parse(repository).unwrap(),
        epoch: 1,
        visibility,
        roles: [(PrincipalId::parse(OWNER).unwrap(), Role::Maintain)]
            .into_iter()
            .collect(),
        required_approvals: 0,
    }
}

/// The gate of public repository `repository`.
pub fn gate(repository: &str) -> Gate {
    let mut gate = Gate::new(RepositoryId::parse(repository).unwrap(), test_keys::keys());
    gate.apply(permissions(repository, Visibility::Public))
        .unwrap();
    gate
}

/// A ten-minute token for `sub` on `repository`, with every repository scope.
pub fn token_for(sub: &str, repository: &str) -> String {
    let now = now();
    let claims = test_keys::claims(sub, repository, &ALL, now, now + 600, &format!("{sub}-jti"));
    test_keys::issuer().issue(&claims).unwrap()
}

/// The owner's token for `repository`.
pub fn token(repository: &str) -> String {
    token_for(OWNER, repository)
}

/// `request` with the owner's `Authorization` line after its request line.
pub fn authorized(repository: &str, request: &[u8]) -> Vec<u8> {
    let Some(end) = request.windows(2).position(|w| w == b"\r\n") else {
        return request.to_vec();
    };
    let mut out = request[..end + 2].to_vec();
    out.extend_from_slice(format!("Authorization: Bearer {}\r\n", token(repository)).as_bytes());
    out.extend_from_slice(&request[end + 2..]);
    out
}

/// A ten-minute token like [`token_for`], issued to device `dev`, and limited to `layer`
/// when one is given (D26).
pub fn device_token(sub: &str, repository: &str, dev: &str, layer: Option<&str>) -> String {
    let now = now();
    let mut claims = test_keys::claims(
        sub,
        repository,
        &ALL,
        now,
        now + 600,
        &format!("{sub}-{dev}-jti"),
    );
    claims.dev = dev.to_string();
    claims.layer = layer.map(str::to_string);
    test_keys::issuer().issue(&claims).unwrap()
}

/// A node serving `name` from `log` on `listener` until `join`. It checks for the stop
/// between connections instead of counting them: other local processes probe any new
/// loopback listener, and a probe counted as one of the test's connections would close the
/// listener under the test's last request.
pub struct Served {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: thread::JoinHandle<FileWorldLog>,
}

pub fn serve_until_joined(
    listener: TcpListener,
    name: &str,
    log: FileWorldLog,
    gate: Gate,
) -> Served {
    let name = name.to_string();
    serve_each(listener, log, move |listener, log| {
        serve(listener, &name, log, &gate, Some(1)).unwrap()
    })
}

/// [`serve_until_joined`], serving as `javelin-node serve` does: the permissions and keys
/// stored in `dir` decide each request, so a push between requests applies to the next one.
pub fn serve_live_until_joined(
    listener: TcpListener,
    name: &str,
    log: FileWorldLog,
    dir: PathBuf,
) -> Served {
    let name = name.to_string();
    serve_each(listener, log, move |listener, log| {
        serve_live(listener, &name, log, &dir, Some(1)).unwrap()
    })
}

/// Serves one connection at a time with `serve_one` until the stop is set.
fn serve_each(
    listener: TcpListener,
    log: FileWorldLog,
    serve_one: impl Fn(&TcpListener, &mut FileWorldLog) + Send + 'static,
) -> Served {
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = thread::spawn(move || {
        let mut log = log;
        while !flag.load(Ordering::SeqCst) {
            serve_one(&listener, &mut log);
        }
        log
    });
    Served { addr, stop, thread }
}

impl Served {
    /// Stops the node and returns its log. A bare connection wakes the accept loop.
    pub fn join(self) -> thread::Result<FileWorldLog> {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        self.thread.join()
    }
}
