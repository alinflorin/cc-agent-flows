//! The egress proxy: the only way out of an agent VM. It speaks HTTP `CONNECT` on any async
//! stream (the SSH tunnel in `tunnel.rs` hands it one per guest connection) and tunnels the bytes
//! untouched, deny by default: the host must be on the allowlist, the port must be 443 and the
//! address the name resolves to must be public. The proxy resolves the name itself and connects
//! to that address, so the guest's own DNS plays no part.
//!
//! Hosts a credential injection covers are not tunnelled but handed to `inspect.rs` (TLS
//! inspection and header injection) after the `200`.


use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use crate::net::inspect::{Inspector, Prefixed, Upstream, STOCK_PER_HOST};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;

/// Largest request head (`CONNECT host:port HTTP/1.1` and headers) we read.
const MAX_HEAD: usize = 8 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Buffer of each direction of a tunnelled connection (tokio's default is 8 KiB): bulk transfers
/// (git, npm) take fewer reads and writes through the SSH channel.
const COPY_BUF: usize = 64 * 1024;
/// A client that has not sent its request after this long is dropped.
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);
/// Open tunnels per proxy (one proxy per agent run) unless the settings say otherwise.
pub const MAX_CONNECTIONS: usize = 64;

/// What one run's traffic may amount to. No budget (`None`) is the default.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Tunnels open at the same time.
    pub max_connections: usize,
    /// Bytes the guest may send out over the whole run.
    pub max_upload: Option<u64>,
    /// Bytes the guest may take in over the whole run.
    pub max_download: Option<u64>,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_connections: MAX_CONNECTIONS, max_upload: None, max_download: None }
    }
}

/// A snapshot of what a run's traffic has been so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Totals {
    /// Bytes from the guest to the internet.
    pub up: u64,
    /// Bytes from the internet to the guest.
    pub down: u64,
    /// Connections open now.
    pub open: u32,
    /// Connections accepted so far.
    pub connections: u64,
}

/// The counters of one run: every byte that crosses the proxy, in both directions, and the budget
/// that ends the connections when it is used up.
pub struct Meter {
    up: AtomicU64,
    down: AtomicU64,
    open: AtomicU32,
    connections: AtomicU64,
    limits: Limits,
    /// bit 0: upload budget reached, bit 1: download budget reached
    tripped: AtomicU8,
    reported: AtomicU8,
}

impl Meter {
    fn new(limits: Limits) -> Meter {
        Meter { up: AtomicU64::new(0), down: AtomicU64::new(0), open: AtomicU32::new(0), connections: AtomicU64::new(0), limits, tripped: AtomicU8::new(0), reported: AtomicU8::new(0) }
    }

    pub fn totals(&self) -> Totals {
        Totals { up: self.up.load(Ordering::Relaxed), down: self.down.load(Ordering::Relaxed), open: self.open.load(Ordering::Relaxed), connections: self.connections.load(Ordering::Relaxed) }
    }

    fn over_up(&self) -> bool {
        let over = self.limits.max_upload.is_some_and(|m| self.up.load(Ordering::Relaxed) >= m);
        if over {
            self.tripped.fetch_or(1, Ordering::Relaxed);
        }
        over
    }

    fn over_down(&self) -> bool {
        let over = self.limits.max_download.is_some_and(|m| self.down.load(Ordering::Relaxed) >= m);
        if over {
            self.tripped.fetch_or(2, Ordering::Relaxed);
        }
        over
    }

    /// Whether either budget is used up: new connections are refused then, and the run is stopped (Claude's own
    /// traffic is counted too, so an agent over its budget cannot reach its API any more).
    pub fn exhausted(&self) -> bool {
        self.over_up() | self.over_down()
    }

    /// Logs a budget that ran out, once.
    fn report(&self, log: &impl Fn(String)) {
        let now = self.tripped.load(Ordering::Relaxed);
        let fresh = now & !self.reported.fetch_or(now, Ordering::Relaxed);
        if fresh & 1 != 0 {
            log("egress: upload budget reached, the connections were closed".into());
        }
        if fresh & 2 != 0 {
            log("egress: download budget reached, the connections were closed".into());
        }
    }
}

/// Counts the bytes of the guest's connection and fails it once a budget is used up.
struct Counted<S> {
    inner: S,
    meter: Arc<Meter>,
}

fn budget_error() -> std::io::Error {
    std::io::Error::other("the data budget of this agent is used up")
}

impl<S: AsyncRead + Unpin> AsyncRead for Counted<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut tokio::io::ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        if self.meter.over_up() {
            return Poll::Ready(Err(budget_error()));
        }
        let before = buf.filled().len();
        let r = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &r {
            self.meter.up.fetch_add((buf.filled().len() - before) as u64, Ordering::Relaxed);
        }
        r
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Counted<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        if self.meter.over_down() {
            return Poll::Ready(Err(budget_error()));
        }
        let r = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &r {
            self.meter.down.fetch_add(*n as u64, Ordering::Relaxed);
        }
        r
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Keeps the count of open connections right whatever way a connection ends.
struct OpenGuard(Arc<Meter>);

impl Drop for OpenGuard {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The hosts an agent may reach: exact names and `*.suffix` wildcards (subdomains only, the bare
/// suffix is not matched). Entries are stored as `profiles::clean_host` leaves them. An entry
/// that starts with `http://` also allows plain HTTP (port 80) to the host.
#[derive(Debug, Clone, Default)]
pub struct Rules {
    https: Vec<String>,
    http: Vec<String>,
}

impl Rules {
    /// Builds the rules from several lists (the global one and the profile's), duplicates dropped.
    pub fn new<I, S>(lists: I) -> Rules
    where
        I: IntoIterator<Item = S>,
        S: AsRef<[String]>,
    {
        let mut rules = Rules::default();
        for list in lists {
            for h in list.as_ref() {
                let h = h.trim().trim_end_matches('.').to_ascii_lowercase();
                let (h, plain) = match h.strip_prefix(crate::config::profiles::HTTP_PREFIX) {
                    Some(rest) => (rest.to_string(), true),
                    None => (h, false),
                };
                if h.is_empty() {
                    continue;
                }
                if plain && !rules.http.contains(&h) {
                    rules.http.push(h.clone());
                }
                if !rules.https.contains(&h) {
                    rules.https.push(h);
                }
            }
        }
        rules
    }

    /// `host` must already be lowercase without a trailing dot.
    pub fn allows(&self, host: &str) -> bool {
        self.https.iter().any(|rule| host_matches(rule, host))
    }

    /// Whether plain HTTP to `host` is allowed too (an entry marked `http://`).
    pub fn allows_http(&self, host: &str) -> bool {
        self.http.iter().any(|rule| host_matches(rule, host))
    }

    /// Whether every host `pattern` (a name or a `*.` wildcard, as in an injection's rule) can name is allowed.
    pub fn covers(&self, pattern: &str) -> bool {
        match pattern.strip_prefix("*.") {
            None => self.allows(pattern),
            Some(suffix) => self.https.iter().any(|rule| match rule.strip_prefix("*.") {
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
    meter: Arc<Meter>,
    /// Port plain HTTP requests are sent to (80; only the tests change it).
    http_port: u16,
}

impl Proxy {
    pub fn new(rules: Rules, inspector: Option<Inspector>) -> Arc<Proxy> {
        Proxy::with_limits(rules, inspector, Limits::default())
    }

    pub fn with_limits(rules: Rules, inspector: Option<Inspector>, limits: Limits) -> Arc<Proxy> {
        Arc::new(Proxy { rules, ports: vec![443], guard: true, inspector, slots: Semaphore::new(limits.max_connections.max(1)), meter: Arc::new(Meter::new(limits)), http_port: 80 })
    }

    /// The run's traffic counters.
    pub fn meter(&self) -> Arc<Meter> {
        self.meter.clone()
    }

    /// How many connections to `host` the inspector has ready.
    #[cfg(test)]
    pub(crate) fn stocked(&self, host: &str) -> usize {
        self.inspector.as_ref().map_or(0, |i| i.stocked(host))
    }

    #[cfg(test)]
    pub(crate) fn for_test(rules: Rules, port: u16, inspector: Option<Inspector>) -> Arc<Proxy> {
        Proxy::for_test_with(rules, port, inspector, Limits::default())
    }

    #[cfg(test)]
    pub(crate) fn for_test_with(rules: Rules, port: u16, inspector: Option<Inspector>, limits: Limits) -> Arc<Proxy> {
        Arc::new(Proxy { rules, ports: vec![port], guard: false, inspector, slots: Semaphore::new(limits.max_connections.max(1)), meter: Arc::new(Meter::new(limits)), http_port: port })
    }

    /// Serves one client connection: reads its `CONNECT`, checks it and tunnels. `log` gets one
    /// line per connection (allowed or not, and why). Ends when either side closes.
    pub async fn serve<S>(&self, client: S, log: impl Fn(String) + Sync)
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        self.serve_counted(Counted { inner: client, meter: self.meter.clone() }, &log).await;
        self.meter.report(&log);
    }

    async fn serve_counted<S>(&self, mut client: Counted<S>, log: &(impl Fn(String) + Sync))
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let request = match tokio::time::timeout(HEAD_TIMEOUT, read_request(&mut client)).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return deny(&mut client, log, "-", &e).await,
            Err(_) => return log("egress: no request".into()),
        };
        let (target, extra) = match request {
            Request::Connect { target, extra } => (target, extra),
            Request::Plain(plain) => return self.serve_plain(client, log, plain).await,
        };
        let (host, port) = &target;
        let shown = format!("{host}:{port}");
        if !self.ports.contains(port) {
            return deny(&mut client, log, &shown, "port not allowed").await;
        }
        if !self.rules.allows(host) {
            return deny(&mut client, log, &shown, "host not on the allowlist").await;
        }
        if self.meter.exhausted() {
            return deny(&mut client, log, &shown, "the data budget of this agent is used up").await;
        }
        let Ok(_slot) = self.slots.try_acquire() else {
            return deny(&mut client, log, &shown, "too many connections").await;
        };
        self.meter.open.fetch_add(1, Ordering::Relaxed);
        self.meter.connections.fetch_add(1, Ordering::Relaxed);
        let _open = OpenGuard(self.meter.clone());
        // A connection to an inspected host that was made ahead of time saves the TCP and TLS
        // handshakes with the real server (see `warm`); the checks above have all been done.
        let inspector = self.inspector.as_ref().filter(|i| i.wants(host));
        if let Some(ready) = inspector.and_then(|i| i.take(host)) {
            log(format!("egress: {shown} allowed"));
            if client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await.is_err() {
                return;
            }
            return inspector.unwrap().serve(Prefixed::new(extra, client), Upstream::Ready(ready), host, log).await;
        }
        let mut upstream = match self.dial(host, *port).await {
            Ok(s) => s,
            Err(e) => return deny(&mut client, log, &shown, &e).await,
        };
        log(format!("egress: {shown} allowed"));
        if client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await.is_err() {
            return;
        }
        if let Some(inspector) = inspector {
            return inspector.serve(Prefixed::new(extra, client), Upstream::Fresh(upstream), host, log).await;
        }
        // Whatever followed the request head in the same read belongs to the tunnel.
        if !extra.is_empty() && upstream.write_all(&extra).await.is_err() {
            return;
        }
        let _ = tokio::io::copy_bidirectional_with_sizes(&mut client, &mut upstream, COPY_BUF, COPY_BUF).await;
    }

    /// A plain HTTP request (`GET http://host/path`) to a host marked `http://`: one request per
    /// connection (`Connection: close`), sent to port 80 in origin form with `Host` set from the
    /// URL. When the run protects secrets and the profile does not name the host, the whole
    /// request is read first and refused if it carries one (a body must have a `Content-Length`).
    async fn serve_plain<S>(&self, mut client: Counted<S>, log: &(impl Fn(String) + Sync), req: Plain)
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let shown = format!("http://{}{}", req.host, req.path);
        if !self.rules.allows_http(&req.host) {
            return deny(&mut client, log, &shown, "only HTTPS is allowed for this host (plain HTTP is not enabled for it)").await;
        }
        if self.meter.exhausted() {
            return deny(&mut client, log, &shown, "the data budget of this agent is used up").await;
        }
        let guarded = self.inspector.as_ref().and_then(|i| i.leaks()).filter(|l| l.guards(&req.host));
        let mut body = req.extra;
        if let Some(leaks) = guarded {
            if req.chunked {
                return deny(&mut client, log, &shown, "a chunked body to a host the profile does not name cannot be checked for secrets").await;
            }
            let len = req.length.unwrap_or(0);
            if len > MAX_SCANNED_BODY {
                return deny(&mut client, log, &shown, "a body over 4 MiB to a host the profile does not name cannot be checked for secrets").await;
            }
            let mut chunk = [0u8; 8192];
            body.truncate(len);
            while body.len() < len {
                match tokio::time::timeout(HEAD_TIMEOUT, client.read(&mut chunk)).await {
                    Ok(Ok(n)) if n > 0 => body.extend_from_slice(&chunk[..n.min(len - body.len())]),
                    _ => return deny(&mut client, log, &shown, "the request body ended early").await,
                }
            }
            let found = leaks
                .find(req.path.as_bytes())
                .or_else(|| req.headers.iter().find_map(|(_, v)| leaks.find_in_value(v.as_bytes())))
                .or_else(|| leaks.find(&body));
            if let Some(name) = found {
                return deny(&mut client, log, &shown, &format!("the request carries the protected secret {name} and the profile does not list this host")).await;
            }
        }
        let Ok(_slot) = self.slots.try_acquire() else {
            return deny(&mut client, log, &shown, "too many connections").await;
        };
        self.meter.open.fetch_add(1, Ordering::Relaxed);
        self.meter.connections.fetch_add(1, Ordering::Relaxed);
        let _open = OpenGuard(self.meter.clone());
        let mut upstream = match self.dial(&req.host, self.http_port).await {
            Ok(s) => s,
            Err(e) => return deny(&mut client, log, &shown, &e).await,
        };
        log(format!("egress: {} {shown} allowed (plain HTTP)", req.method));
        let mut head = format!("{} {} HTTP/1.1\r\nHost: {}\r\n", req.method, req.path, req.host);
        for (name, value) in &req.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("Connection: close\r\n\r\n");
        if upstream.write_all(head.as_bytes()).await.is_err() || (!body.is_empty() && upstream.write_all(&body).await.is_err()) {
            return;
        }
        // The exchange ends with the response (`Connection: close`). When everything the client may
        // send has been checked, whatever follows the request is not forwarded.
        let (mut from_client, mut to_client) = tokio::io::split(client);
        let (mut from_server, mut to_server) = upstream.into_split();
        {
            let down = tokio::io::copy(&mut from_server, &mut to_client);
            tokio::pin!(down);
            if guarded.is_some() {
                let _ = down.await;
            } else {
                let up = tokio::io::copy(&mut from_client, &mut to_server);
                tokio::pin!(up);
                tokio::select! {
                    _ = &mut down => {}
                    _ = &mut up => { let _ = down.await; }
                }
            }
        }
        let _ = to_client.shutdown().await;
    }

    /// Resolves `host` and connects to the first address the guard accepts. A name often has an
    /// IPv6 and an IPv4 address and only one of them may be reachable: each is tried.
    async fn dial(&self, host: &str, port: u16) -> Result<TcpStream, String> {
        let mut failure = String::new();
        for addr in self.resolve(host, port).await? {
            match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
                Ok(Ok(s)) => {
                    // small writes (TLS flights, request heads) must not wait for Nagle's timer
                    let _ = s.set_nodelay(true);
                    return Ok(s);
                }
                Ok(Err(e)) => failure = format!("cannot connect: {e}"),
                Err(_) => failure = "connect timed out".into(),
            }
        }
        Err(failure)
    }

    /// Keeps a few connections to the inspected hosts ready for the guest: the TCP and TLS
    /// handshakes with the real server are a round trip or more each, and Claude Code opens its
    /// connections to the API as the first thing it does. Only the exact hosts of the injections
    /// are connected, through the same name checks and address guard as any connection; nothing is
    /// sent on them until the guest asks, and a stocked connection older than 25 s is dropped.
    /// Ends when the proxy is gone.
    pub fn warm(self: &Arc<Self>) {
        let Some(inspector) = self.inspector.as_ref() else { return };
        let signal = inspector.refill_signal();
        let proxy = Arc::downgrade(self);
        // certificates for the guest's TLS handshakes, ready before its first connection
        let ahead = self.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(i) = ahead.inspector.as_ref() {
                i.pregenerate();
            }
        });
        tokio::spawn(async move {
            loop {
                let Some(p) = proxy.upgrade() else { return };
                p.top_up().await;
                drop(p);
                signal.notified().await;
            }
        });
    }

    async fn top_up(&self) {
        let Some(inspector) = self.inspector.as_ref() else { return };
        let port = self.ports[0];
        for host in inspector.stockable() {
            if !self.rules.allows(&host) || self.meter.exhausted() {
                continue;
            }
            let missing = STOCK_PER_HOST.saturating_sub(inspector.stocked(&host));
            let one = || async {
                let tcp = self.dial(&host, port).await?;
                inspector.stock(&host, tcp).await
            };
            let (a, b) = tokio::join!(async { if missing > 0 { Some(one().await) } else { None } }, async { if missing > 1 { Some(one().await) } else { None } });
            for e in [a, b].into_iter().flatten().filter_map(Result::err) {
                log::debug!("egress: could not make a connection to {host} ahead of time: {e}");
            }
        }
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

/// What a client asked for.
enum Request {
    /// A `CONNECT`: the `(host, port)` and the bytes read past the head.
    Connect { target: (String, u16), extra: Vec<u8> },
    Plain(Plain),
}

/// A plain HTTP request in proxy form, `GET http://host/path HTTP/1.1`, already taken apart.
struct Plain {
    method: String,
    host: String,
    /// The path and query, in origin form.
    path: String,
    /// The headers to pass on: no `Host`, `Connection` or other hop-by-hop ones.
    headers: Vec<(String, String)>,
    length: Option<usize>,
    chunked: bool,
    /// The bytes read past the head.
    extra: Vec<u8>,
}

/// Largest body that is read whole to look for protected secrets.
const MAX_SCANNED_BODY: usize = 4 * 1024 * 1024;

/// The host name of a request target: lowercased without a trailing dot; IP literals are refused (the
/// allowlist is about names).
fn clean_name(host: &str) -> Result<String, String> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host.starts_with('[') || host.parse::<IpAddr>().is_ok() {
        return Err("IP addresses are not allowed, use a host name".into());
    }
    if !host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') {
        return Err("bad host name".into());
    }
    Ok(host)
}

/// Reads the request head: a `CONNECT`, or a plain HTTP request for a host marked `http://`.
async fn read_request<S: AsyncRead + Unpin>(client: &mut S) -> Result<Request, String> {
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
    let mut lines = head.split("\r\n");
    let line = lines.next().unwrap_or("");
    let mut parts = line.split(' ');
    let (method, target, version) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next());
    let https_only = || "only HTTPS (CONNECT) is allowed".to_string();
    if !version.is_some_and(|v| v.starts_with("HTTP/1.")) || parts.next().is_some() {
        return Err(https_only());
    }
    if method != "CONNECT" {
        let rest = target.strip_prefix("http://").ok_or_else(https_only)?;
        if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(https_only());
        }
        let (authority, path) = match rest.find(['/', '?']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let path = if path.starts_with('?') { format!("/{path}") } else { path.to_string() };
        let (host, port) = authority.rsplit_once(':').unwrap_or((authority, "80"));
        if port != "80" || authority.contains('@') {
            return Err("plain HTTP is only allowed on port 80, without credentials in the URL".into());
        }
        let host = clean_name(host)?;
        let mut headers = vec![];
        let (mut length, mut chunked) = (None, false);
        for l in lines.take_while(|l| !l.is_empty()) {
            let (name, value) = l.split_once(':').filter(|(n, _)| !n.is_empty() && !n.starts_with([' ', '\t']) && !n.ends_with([' ', '\t'])).ok_or("bad header")?;
            let (lower, value) = (name.to_ascii_lowercase(), value.trim());
            match lower.as_str() {
                "host" | "connection" | "proxy-connection" | "proxy-authorization" | "keep-alive" | "upgrade" | "te" | "trailer" | "expect" => continue,
                "content-length" => {
                    if length.is_some() {
                        return Err("two Content-Length headers".into());
                    }
                    length = Some(value.parse::<usize>().map_err(|_| "bad Content-Length")?);
                }
                "transfer-encoding" => chunked = true,
                _ => {}
            }
            headers.push((name.to_string(), value.to_string()));
        }
        if chunked && length.is_some() {
            return Err("both Content-Length and Transfer-Encoding".into());
        }
        return Ok(Request::Plain(Plain { method: method.into(), host, path, headers, length, chunked, extra: buf[end..].to_vec() }));
    }
    let (host, port) = target.rsplit_once(':').ok_or("the target needs a port")?;
    let port: u16 = port.parse().map_err(|_| "bad port")?;
    Ok(Request::Connect { target: (clean_name(host)?, port), extra: buf[end..].to_vec() })
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

    /// A server that answers every connection with `ok` and reports the request it got.
    async fn http_server() -> (u16, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = l.accept().await.unwrap();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mut got = vec![];
                    let mut buf = [0u8; 1024];
                    while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = s.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        got.extend_from_slice(&buf[..n]);
                    }
                    let _ = tx.send(String::from_utf8_lossy(&got).into_owned());
                    let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await;
                });
            }
        });
        (port, rx)
    }

    #[test]
    fn an_http_entry_allows_plain_http_and_https() {
        let r = rules(&["http://a.example.com", "b.example.com", "http://*.c.org"]);
        assert!(r.allows("a.example.com") && r.allows_http("a.example.com"));
        assert!(r.allows("b.example.com") && !r.allows_http("b.example.com"));
        assert!(r.allows("x.c.org") && r.allows_http("x.c.org") && !r.allows_http("c.org"));
    }

    #[tokio::test]
    async fn forwards_plain_http_only_to_marked_hosts() {
        let (port, mut seen) = http_server().await;
        let proxy = Proxy::for_test(rules(&["http://localhost", "other.com"]), port, None);
        let (reply, log) = exchange(proxy.clone(), b"GET http://LocalHost./a/b?x=1 HTTP/1.1\r\nHost: evil.com\r\nProxy-Connection: keep-alive\r\nX-Test: 1\r\n\r\n").await;
        assert!(reply.starts_with("HTTP/1.1 200") && reply.ends_with("ok"), "{reply}");
        assert!(log.iter().any(|l| l.contains("allowed (plain HTTP)")), "{log:?}");
        let sent = seen.recv().await.unwrap();
        assert!(sent.starts_with("GET /a/b?x=1 HTTP/1.1\r\nHost: localhost\r\n"), "{sent}");
        assert!(sent.contains("X-Test: 1") && sent.contains("Connection: close") && !sent.contains("evil.com") && !sent.contains("Proxy-Connection"), "{sent}");
        // listed for HTTPS only, or not listed: refused
        for req in ["GET http://other.com/ HTTP/1.1\r\n\r\n", "GET http://nope.com/ HTTP/1.1\r\n\r\n", "GET http://localhost:8080/ HTTP/1.1\r\n\r\n", "GET http://u:p@localhost/ HTTP/1.1\r\n\r\n", "GET http://127.0.0.1/ HTTP/1.1\r\n\r\n"] {
            let (reply, _) = exchange(proxy.clone(), req.as_bytes()).await;
            assert!(reply.starts_with("HTTP/1.1 403"), "{req}: {reply}");
        }
    }

    #[tokio::test]
    async fn plain_http_to_a_host_the_profile_does_not_name_is_checked_for_secrets() {
        use crate::net::leaks::{Leaks, Watched};
        let (port, mut seen) = http_server().await;
        let leaks = Leaks::new(vec![Watched { name: "TOKEN".into(), value: "s3cret-token-value".into() }], rules(&["github.com"]));
        let inspector = Inspector::with_roots(vec![], rustls::RootCertStore::empty()).unwrap().watching(leaks);
        let proxy = Proxy::for_test(rules(&["http://localhost"]), port, Some(inspector));
        for req in [
            "GET http://localhost/?k=s3cret-token-value HTTP/1.1\r\n\r\n",
            "GET http://localhost/ HTTP/1.1\r\nAuthorization: Bearer s3cret-token-value\r\n\r\n",
            "POST http://localhost/ HTTP/1.1\r\nContent-Length: 24\r\n\r\nxx s3cret-token-value xx",
            "POST http://localhost/ HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
        ] {
            let (reply, _) = exchange(proxy.clone(), req.as_bytes()).await;
            assert!(reply.starts_with("HTTP/1.1 403"), "{req}: {reply}");
        }
        assert!(seen.try_recv().is_err(), "nothing may reach the server");
        let (reply, _) = exchange(proxy, b"POST http://localhost/ HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello").await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(seen.recv().await.unwrap().starts_with("POST / HTTP/1.1"));
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

    async fn echo() -> u16 {
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
        port
    }

    async fn open(client: &mut tokio::io::DuplexStream, port: u16) {
        client.write_all(format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n").as_bytes()).await.unwrap();
        let mut head = vec![];
        while !head.ends_with(b"\r\n\r\n") {
            head.push(tokio::time::timeout(Duration::from_secs(5), client.read_u8()).await.unwrap().unwrap());
        }
        assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"));
    }

    #[tokio::test]
    async fn counts_the_bytes_and_closes_at_the_upload_budget() {
        let port = echo().await;
        let limits = Limits { max_upload: Some(300), ..Limits::default() };
        let proxy = Proxy::for_test_with(rules(&["localhost"]), port, None, limits);
        let meter = proxy.meter();
        let log = Arc::new(std::sync::Mutex::new(vec![]));
        let (mut client, server) = tokio::io::duplex(1 << 16);
        let l = log.clone();
        let p = proxy.clone();
        let task = tokio::spawn(async move { p.serve(server, move |s| l.lock().unwrap().push(s)).await });
        open(&mut client, port).await;
        client.write_all(&[7u8; 100]).await.unwrap();
        let mut got = [0u8; 100];
        client.read_exact(&mut got).await.unwrap();
        let t = meter.totals();
        assert!(t.up >= 100 && t.down >= 100 && t.open == 1 && t.connections == 1, "{t:?}");
        // past the budget the connection is closed
        let _ = client.write_all(&[7u8; 400]).await;
        tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
        assert_eq!(meter.totals().open, 0);
        assert!(log.lock().unwrap().iter().any(|l| l.contains("upload budget reached")), "{:?}", log.lock().unwrap());
        // and the next connection is refused at once
        let (reply, _) = exchange(proxy, format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n").as_bytes()).await;
        assert!(reply.contains("data budget"), "{reply}");
    }

    #[tokio::test]
    async fn refuses_connections_over_the_limit() {
        let port = echo().await;
        let proxy = Proxy::for_test_with(rules(&["localhost"]), port, None, Limits { max_connections: 1, ..Limits::default() });
        let (mut first, server) = tokio::io::duplex(4096);
        let p = proxy.clone();
        tokio::spawn(async move { p.serve(server, |_| {}).await });
        open(&mut first, port).await;
        let (reply, log) = exchange(proxy.clone(), format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n").as_bytes()).await;
        assert!(reply.starts_with("HTTP/1.1 403") && reply.contains("too many connections"), "{reply} {log:?}");
        // a slot frees up when the first one closes
        drop(first);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let (mut second, server) = tokio::io::duplex(4096);
        tokio::spawn(async move { proxy.serve(server, |_| {}).await });
        open(&mut second, port).await;
    }
}
