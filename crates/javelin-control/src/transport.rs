//! How a client reaches the control plane or a node: HTTPS to any host, with the platform's
//! web PKI roots, and plain HTTP only to this machine (`localhost`, `127.0.0.0/8`, `::1`), for
//! tests and local development. Bearer tokens therefore never cross a network in clear. The
//! stack is `ureq` with rustls, the same one `javelin-node-worker` uses to reach the Worker.

use std::{
    net::{IpAddr, Ipv6Addr},
    time::Duration,
};

/// A validated `scheme://authority` that a client may send credentials to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Endpoint {
    origin: String,
    loopback: bool,
}

impl Endpoint {
    /// Parses `http(s)://HOST[:PORT]`, with an optional trailing slash and no path. Plain
    /// `http` is refused unless the host is this machine.
    pub fn parse(url: &str) -> Result<Self, String> {
        let url = url.trim_end_matches('/');
        let (scheme, authority) = url
            .split_once("://")
            .ok_or_else(|| format!("{url} is not an http(s)://HOST[:PORT] URL"))?;
        let https = match scheme.to_ascii_lowercase().as_str() {
            "https" => true,
            "http" => false,
            _ => return Err(format!("{url} must use https")),
        };
        if authority.is_empty() || authority.contains(['/', '@', '?', '#', ' ']) {
            return Err(format!(
                "{url} must be scheme://HOST[:PORT], without a path or user"
            ));
        }
        let host = host_of(authority).ok_or_else(|| format!("{url} has no valid host"))?;
        let loopback = is_loopback_host(host);
        if !https && !loopback {
            return Err(format!(
                "{url} would send credentials in clear: use https (plain http only for localhost, 127.0.0.1 or [::1])"
            ));
        }
        Ok(Self {
            origin: format!("{}://{authority}", if https { "https" } else { "http" }),
            loopback,
        })
    }

    /// `scheme://authority`, without a trailing slash.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Whether the host is this machine.
    pub fn loopback(&self) -> bool {
        self.loopback
    }

    /// A client for this endpoint. Statuses come back as answers, not errors; redirects are not
    /// followed, so a credential never goes to another host; a proxy from the environment is
    /// skipped for this machine, where it would see plain HTTP.
    pub fn agent(&self, timeout: Duration) -> ureq::Agent {
        let mut config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(timeout));
        if self.loopback {
            config = config.proxy(None);
        }
        config.build().into()
    }
}

/// The host of `HOST[:PORT]` or `[IPV6][:PORT]`, or `None` when the port is not a number.
fn host_of(authority: &str) -> Option<&str> {
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, rest) = rest.split_once(']')?;
        (host, rest.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    let port_ok = port.is_none_or(|port| !port.is_empty() && port.parse::<u16>().is_ok());
    (!host.is_empty() && port_ok).then_some(host)
}

/// Whether `host` names this machine: `localhost`, a 127.0.0.0/8 address or `::1`.
pub fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => host.parse::<Ipv6Addr>().is_ok_and(|ip| ip.is_loopback()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_anywhere_and_plain_http_only_on_this_machine() {
        let origin = |url: &str| Endpoint::parse(url).map(|e| e.origin().to_string());
        assert_eq!(
            origin("https://node.example.workers.dev/").unwrap(),
            "https://node.example.workers.dev"
        );
        assert!(origin("https://node.example:8443").is_ok());
        for local in [
            "http://127.0.0.1:7878",
            "http://127.1.2.3:7878",
            "http://localhost:80",
            "http://LOCALHOST",
            "http://[::1]:7878",
        ] {
            assert!(Endpoint::parse(local).unwrap().loopback(), "{local}");
        }
        for refused in [
            "http://node.example",
            "http://10.0.0.5:7878",
            "http://localhost.evil.example",
            "http://localhost:80@evil.example",
            "https://user:pass@node.example",
            "https://node.example/path",
            "ftp://127.0.0.1",
            "127.0.0.1:7878",
            "http://127.0.0.1:port",
        ] {
            assert!(Endpoint::parse(refused).is_err(), "{refused}");
        }
    }
}
