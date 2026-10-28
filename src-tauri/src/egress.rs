//! The egress proxy: the only way out of an agent VM. It speaks HTTP `CONNECT` on any async
//! stream (the SSH tunnel in `tunnel.rs` hands it one per guest connection) and tunnels the bytes
//! untouched, deny by default: the host must be on the allowlist, the port must be 443 and the
//! address the name resolves to must be public. The proxy resolves the name itself and connects
//! to that address, so the guest's own DNS plays no part.
//!
//! Hosts a credential injection covers are not tunnelled but handed to `inspect.rs` (TLS
//! inspection and header injection) after the `200`.

#![allow(dead_code)]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;

use crate::inspect::{Inspector, Prefixed};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;

/// Largest request head (`CONNECT host:port HTTP/1.1` and headers) we read.
const MAX_HEAD: usize = 8 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A client that has not sent its request after this long is dropped.
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);
/// Open tunnels per proxy (one proxy per agent run).
const MAX_CONNECTIONS: usize = 64;

/// The hosts an agent may reach: exact names and `*.suffix` wildcards (subdomains only, the bare
/// suffix is not matched). Entries are stored as `profiles::clean_host` leaves them.
#[derive(Debug, Clone, Default)]
pub struct Rules(Vec<String>);

impl Rules {
    /// Builds the rules from several lists (the global one and the profile's), duplicates dropped.
    pub fn new<I, S>(lists: I) -> Rules
    where
        I: IntoIterator<Item = S>,
        S: AsRef<[String]>,
    {
        let mut hosts: Vec<String> = vec![];
        for list in lists {
            for h in list.as_ref() {
                let h = h.trim().trim_end_matches('.').to_ascii_lowercase();
                if !h.is_empty() && !hosts.contains(&h) {
                    hosts.push(h);
                }
            }
        }
        Rules(hosts)
    }

    /// `host` must already be lowercase without a trailing dot.
    pub fn allows(&self, host: &str) -> bool {
        self.0.iter().any(|rule| host_matches(rule, host))
    }

    /// Whether every host `pattern` (a name or a `*.` wildcard, as in an injection's rule) can name is allowed.
    pub fn covers(&self, pattern: &str) -> bool {
        match pattern.strip_prefix("*.") {
            None => self.allows(pattern),
            Some(suffix) => self.0.iter().any(|rule| match rule.strip_prefix("*.") {
                Some(s) => suffix == s || suffix.ends_with(&format!(".{s}")),
                None => false,
            }),
        }
    }
}

/// Whether `host` (lowercase, no trailing dot) is the host `rule` names, or a subdomain of it for
/// a `*.` rule.
pub fn host_matches(rule: &str, host: &str) -> bool {
    match rule.strip_prefix("*.") {
        Some(suffix) => host.len() > suffix.len() + 1 && host.ends_with(suffix) && host.as_bytes()[host.len() - suffix.len() - 1] == b'.',
        None => rule == host,
    }
}

/// Whether an address is a public one. The guard against an allowed name that resolves to the
/// host itself, its LAN or a metadata service.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => is_public_v4(a),
        IpAddr::V6(a) => {
            if let Some(v4) = a.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let first = a.segments()[0];
            !(a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                || first & 0xfe00 == 0xfc00 // unique local fc00::/7
                || first & 0xffc0 == 0xfe80 // link local fe80::/10
                || a == Ipv6Addr::LOCALHOST)
        }
    }
}

fn is_public_v4(a: Ipv4Addr) -> bool {
    let o = a.octets();
    !(a.is_private()
        || a.is_loopback()
        || a.is_link_local()
        || a.is_unspecified()
        || a.is_broadcast()
        || a.is_documentation()
        || a.is_multicast()
        || (o[0] == 100 && o[1] & 0xc0 == 64) // carrier-grade NAT 100.64.0.0/10
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24
        || (o[0] == 198 && o[1] & 0xfe == 18) // benchmarking 198.18.0.0/15
        || o[0] >= 240) // reserved
}

/// One agent run's proxy.
pub struct Proxy {
    pub rules: Rules,
    /// Ports a tunnel may go to.
    pub ports: Vec<u16>,
    /// Refuse private, loopback and link-local addresses (only the tests turn this off).
    pub guard: bool,
    /// Set when the run has injections: the hosts they cover are inspected, not tunnelled.
    inspector: Option<Inspector>,
    slots: Semaphore,
}

impl Proxy {
    pub fn new(rules: Rules, inspector: Option<Inspector>) -> Arc<Proxy> {
        Arc::new(Proxy { rules, ports: vec![443], guard: true, inspector, slots: Semaphore::new(MAX_CONNECTIONS) })
    }

    #[cfg(test)]
    pub(crate) fn for_test(rules: Rules, port: u16, inspector: Option<Inspector>) -> Arc<Proxy> {
        Arc::new(Proxy { rules, ports: vec![port], guard: false, inspector, slots: Semaphore::new(MAX_CONNECTIONS) })
    }

    /// Serves one client connection: reads its `CONNECT`, checks it and tunnels. `log` gets one
    /// line per connection (allowed or not, and why). Ends when either side closes.
    pub async fn serve<S>(&self, mut client: S, log: impl Fn(String) + Sync)
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (target, extra) = match tokio::time::timeout(HEAD_TIMEOUT, read_connect(&mut client)).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return deny(&mut client, &log, "-", &e).await,
            Err(_) => return log("egress: no request".into()),
        };
        let (host, port) = &target;
        let shown = format!("{host}:{port}");
        if !self.ports.contains(port) {
            return deny(&mut client, &log, &shown, "port not allowed").await;
        }
        if !self.rules.allows(host) {
            return deny(&mut client, &log, &shown, "host not on the allowlist").await;
        }
        let Ok(_slot) = self.slots.try_acquire() else {
            return deny(&mut client, &log, &shown, "too many connections").await;
        };
        let addrs = match self.resolve(host, *port).await {
            Ok(a) => a,
            Err(e) => return deny(&mut client, &log, &shown, &e).await,
        };
        // A name often has an IPv6 and an IPv4 address and only one of them may be reachable: try each.
        let mut upstream = None;
        let mut failure = String::new();
        for addr in addrs {
            match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
                Ok(Ok(s)) => {
                    upstream = Some(s);
                    break;
                }
                Ok(Err(e)) => failure = format!("cannot connect: {e}"),
                Err(_) => failure = "connect timed out".into(),
            }
        }
        let Some(mut upstream) = upstream else {
            return deny(&mut client, &log, &shown, &failure).await;
        };
        log(format!("egress: {shown} allowed"));
        if client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await.is_err() {
            return;
        }
        if let Some(inspector) = self.inspector.as_ref().filter(|i| i.wants(host)) {
            return inspector.serve(Prefixed::new(extra, client), upstream, host, &log).await;
        }
        // Whatever followed the request head in the same read belongs to the tunnel.
        if !extra.is_empty() && upstream.write_all(&extra).await.is_err() {
            return;
        }
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    }

    /// Resolves `host` and returns the addresses the guard accepts.
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<std::net::SocketAddr>, String> {
        let addrs: Vec<_> = tokio::net::lookup_host((host, port)).await.map_err(|e| format!("cannot resolve: {e}"))?.collect();
        let allowed: Vec<_> = addrs.iter().copied().filter(|a| !self.guard || is_public(a.ip())).collect();
        if !allowed.is_empty() {
            return Ok(allowed);
        }
        Err(if addrs.is_empty() { "cannot resolve".into() } else { "the name resolves to a private address".into() })
    }
}

async fn deny<S: AsyncWrite + Unpin>(client: &mut S, log: &impl Fn(String), target: &str, reason: &str) {
    log(format!("egress: {target} denied: {reason}"));
    let body = format!("blocked by the agent network policy: {reason}\n");
    let reply = format!("HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = client.write_all(reply.as_bytes()).await;
    let _ = client.shutdown().await;
}

/// Reads the request head. Returns the `(host, port)` of a `CONNECT` and the bytes read past the
/// head. The host is lowercased without a trailing dot; IP literals are refused (the allowlist
/// is about names).
async fn read_connect<S: AsyncRead + Unpin>(client: &mut S) -> Result<((String, u16), Vec<u8>), String> {
    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 1024];
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > MAX_HEAD {
            return Err("request too large".into());
        }
        let n = client.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("connection closed before a request".into());
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = std::str::from_utf8(&buf[..end]).map_err(|_| "request is not text")?;
    let line = head.lines().next().unwrap_or("");
    let mut parts = line.split(' ');
    let (method, authority, version) = (parts.next(), parts.next(), parts.next());
    if method != Some("CONNECT") || !version.is_some_and(|v| v.starts_with("HTTP/1.")) || parts.next().is_some() {
        return Err("only HTTPS (CONNECT) is allowed".into());
    }
    let authority = authority.unwrap_or("");
    let (host, port) = authority.rsplit_once(':').ok_or("the target needs a port")?;
    let port: u16 = port.parse().map_err(|_| "bad port")?;
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host.starts_with('[') || host.parse::<IpAddr>().is_ok() {
        return Err("IP addresses are not allowed, use a host name".into());
    }
    if !host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') {
        return Err("bad host name".into());
    }
    Ok(((host, port), buf[end..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    fn rules(hosts: &[&str]) -> Rules {
        Rules::new([hosts.iter().map(|s| s.to_string()).collect::<Vec<_>>()])
    }

    #[test]
    fn rules_cover_the_hosts_of_an_injection() {
        let r = rules(&["api.github.com", "*.example.com"]);
        assert!(r.covers("api.github.com"));
        assert!(r.covers("a.example.com"));
        assert!(!r.covers("github.com"));
        // a wildcard is covered only by an entry that allows every name it can match
        assert!(r.covers("*.example.com"));
        assert!(r.covers("*.a.example.com"));
        assert!(!r.covers("*.github.com"));
        assert!(!rules(&["a.example.com"]).covers("*.example.com"));
    }

    #[test]
    fn rules_match_exact_and_subdomains_only() {
        let r = rules(&["api.anthropic.com", "*.github.com", "API.Anthropic.com", "x.org."]);
        assert!(r.allows("api.anthropic.com"));
        assert!(r.allows("x.org"));
        assert!(r.allows("api.github.com"));
        assert!(r.allows("a.b.github.com"));
        assert!(!r.allows("github.com"));
        assert!(!r.allows("evilgithub.com"));
        assert!(!r.allows("github.com.evil.com"));
        assert!(!r.allows("anthropic.com"));
        assert!(!r.allows("evil.api.anthropic.com"));
        assert!(!rules(&[]).allows("api.anthropic.com"));
    }

    #[test]
    fn only_public_addresses_pass_the_guard() {
        for ip in ["127.0.0.1", "10.0.2.2", "192.168.1.1", "172.16.0.1", "169.254.169.254", "0.0.0.0", "100.64.0.1", "224.0.0.1", "255.255.255.255", "::1", "fe80::1", "fd00::1", "::ffff:10.0.0.1", "::"] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["1.1.1.1", "8.8.8.8", "93.184.216.34", "2606:4700::1111", "::ffff:8.8.8.8"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    /// Runs the proxy against `request` over an in-memory stream; returns what the client got back
    /// until the proxy closed, and the log.
    async fn exchange(proxy: Arc<Proxy>, request: &[u8]) -> (String, Vec<String>) {
        let (mut client, server) = tokio::io::duplex(4096);
        let lines = Arc::new(std::sync::Mutex::new(vec![]));
        let l = lines.clone();
        let task = tokio::spawn(async move { proxy.serve(server, move |s| l.lock().unwrap().push(s)).await });
        client.write_all(request).await.unwrap();
        let mut out = vec![];
        let _ = tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut out)).await;
        let _ = task.await;
        let log = lines.lock().unwrap().clone();
        (String::from_utf8_lossy(&out).into_owned(), log)
    }

    #[tokio::test]
    async fn denies_what_is_not_allowed() {
        let proxy = Proxy::new(rules(&["api.anthropic.com", "localhost"]), None);
        for (req, why) in [
            ("CONNECT evil.com:443 HTTP/1.1\r\n\r\n", "not on the allowlist"),
            ("CONNECT api.anthropic.com:80 HTTP/1.1\r\n\r\n", "port not allowed"),
            ("GET http://api.anthropic.com/ HTTP/1.1\r\n\r\n", "only HTTPS"),
            ("CONNECT 1.1.1.1:443 HTTP/1.1\r\n\r\n", "IP addresses"),
            ("CONNECT [::1]:443 HTTP/1.1\r\n\r\n", "IP addresses"),
            ("CONNECT api.anthropic.com HTTP/1.1\r\n\r\n", "needs a port"),
            ("CONNECT bad host:443 HTTP/1.1\r\n\r\n", "only HTTPS"),
            // allowed by name, but it resolves to the host's own loopback
            ("CONNECT localhost:443 HTTP/1.1\r\n\r\n", "private address"),
        ] {
            let (reply, log) = exchange(proxy.clone(), req.as_bytes()).await;
            assert!(reply.starts_with("HTTP/1.1 403"), "{req:?}: {reply}");
            assert!(reply.contains(why), "{req:?}: {reply}");
            assert!(log.iter().any(|l| l.contains("denied")), "{log:?}");
        }
    }

    #[tokio::test]
    async fn tunnels_an_allowed_host() {
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = echo.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = echo.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut buf = [0u8; 256];
                    while let Ok(n) = s.read(&mut buf).await {
                        if n == 0 || s.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        let proxy = Proxy::for_test(rules(&["localhost"]), port, None);
        let (mut client, server) = tokio::io::duplex(4096);
        let p = proxy.clone();
        tokio::spawn(async move { p.serve(server, |_| {}).await });
        // the first payload bytes ride along with the request head
        client.write_all(format!("CONNECT LocalHost.:{port} HTTP/1.1\r\nHost: x\r\n\r\nhello").as_bytes()).await.unwrap();
        let mut got = [0u8; 64];
        let mut n = 0;
        while !String::from_utf8_lossy(&got[..n]).contains("hello") {
            assert!(n < got.len(), "no tunnel: {}", String::from_utf8_lossy(&got[..n]));
            n += tokio::time::timeout(Duration::from_secs(5), client.read(&mut got[n..])).await.unwrap().unwrap();
        }
        let text = String::from_utf8_lossy(&got[..n]).into_owned();
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.ends_with("hello"), "{text}");
        client.write_all(b" again").await.unwrap();
        let mut more = vec![0u8; 16];
        let k = tokio::time::timeout(Duration::from_secs(5), client.read(&mut more)).await.unwrap().unwrap();
        assert_eq!(&more[..k], b" again");
    }
}
