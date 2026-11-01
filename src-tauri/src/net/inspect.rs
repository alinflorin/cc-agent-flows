//! TLS inspection for the hosts of a run's credential injections (`injections.rs`). For those
//! hosts the egress proxy does not tunnel the bytes: it answers the guest's `CONNECT`, terminates
//! TLS with a certificate for that host signed by a CA made for this run, reads the HTTP requests,
//! sets the injected headers on the ones that match a rule and sends them on over a new TLS
//! connection to the real server (checked against the public web roots).
//!
//! The CA's private key never leaves this struct; its certificate goes to the VM in the bootstrap
//! (`agentd` installs it). One CA per run, so a stolen or reused certificate is worth nothing the
//! next run. Only HTTP/1.1 is spoken (the ALPN offers nothing else) and protocol upgrades are
//! refused. The request's `Host` and the TLS SNI must be the host the guest asked for in its
//! `CONNECT`, so an allowed tunnel cannot be used to reach another site with the token.


use std::collections::HashMap;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::header::{HOST, HeaderName, HeaderValue, UPGRADE};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::config::injections::Active;
use crate::net::leaks::Leaks;

/// Largest request body read to look for a leaked secret; a bigger one to a guarded host is refused.
const MAX_SCAN: usize = 4 * 1024 * 1024;

type Body = BoxBody<Bytes, hyper::Error>;

pub struct Inspector {
    active: Vec<Active>,
    leaks: Option<Leaks>,
    ca_pem: String,
    issuer: Issuer<'static, KeyPair>,
    leaves: Mutex<HashMap<String, Arc<ServerConfig>>>,
    upstream: Arc<ClientConfig>,
    /// Connections to the real servers made ahead of time (see `stock`).
    stocked: Mutex<HashMap<String, Vec<Ready>>>,
    /// Woken when a stocked connection was taken (and when this is dropped), so the proxy tops the stock up.
    refill: Arc<tokio::sync::Notify>,
}

/// A finished TLS and HTTP handshake with the real server, waiting for a guest connection.
pub struct Ready {
    sender: hyper::client::conn::http1::SendRequest<Body>,
    driver: tokio::task::JoinHandle<()>,
    made: Instant,
}

/// Where a guest connection's server side comes from.
pub enum Upstream {
    /// A TCP connection to the server; the TLS handshake still has to be done.
    Fresh(TcpStream),
    /// One made ahead of time.
    Ready(Ready),
}

/// A stocked connection older than this is not used: servers close idle connections.
const STOCK_MAX_AGE: Duration = Duration::from_secs(25);
/// Connections kept ready per host. Claude Code opens a few at its start.
pub const STOCK_PER_HOST: usize = 2;

/// The client side of the connections to the real servers is the same for every run, so the TLS
/// session tickets a server hands out are kept and later runs resume them (no certificate chain to
/// verify again). A ticket is session state, not a credential: requests carry their own headers.
fn shared_upstream() -> Result<Arc<ClientConfig>, String> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    if let Some(c) = CONFIG.get() {
        return Ok(c.clone());
    }
    let roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    Ok(CONFIG.get_or_init(|| Arc::new(upstream_config(roots).expect("TLS client configuration"))).clone())
}

fn upstream_config(roots: RootCertStore) -> Result<ClientConfig, String> {
    let mut client = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|x| x.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    client.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(client)
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

impl Inspector {
    pub fn new(active: Vec<Active>) -> Result<Inspector, String> {
        Inspector::build(active, shared_upstream()?)
    }

    /// With the roots the real servers are checked against (the tests use their own).
    #[cfg(test)]
    pub fn with_roots(active: Vec<Active>, roots: RootCertStore) -> Result<Inspector, String> {
        Inspector::build(active, Arc::new(upstream_config(roots)?))
    }

    fn build(active: Vec<Active>, upstream: Arc<ClientConfig>) -> Result<Inspector, String> {
        let e = |e: &dyn std::fmt::Display| e.to_string();
        let mut params = CertificateParams::new(Vec::<String>::new()).map_err(|x| e(&x))?;
        params.distinguished_name = DistinguishedName::new();
        params.distinguished_name.push(DnType::CommonName, "CC Agent Flows run CA");
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let key = KeyPair::generate().map_err(|x| e(&x))?;
        let ca_pem = params.self_signed(&key).map_err(|x| e(&x))?.pem();
        Ok(Inspector {
            active,
            leaks: None,
            ca_pem,
            issuer: Issuer::new(params, key),
            leaves: Mutex::new(HashMap::new()),
            upstream,
            stocked: Mutex::new(HashMap::new()),
            refill: Arc::new(tokio::sync::Notify::new()),
        })
    }

    /// Also inspects the hosts the run's protected secrets must not reach, to refuse requests that carry them.
    pub fn watching(mut self, leaks: Option<Leaks>) -> Inspector {
        self.leaks = leaks;
        self
    }

    /// The protected secrets of the run, if any.
    pub fn leaks(&self) -> Option<&Leaks> {
        self.leaks.as_ref()
    }

    /// The CA certificate (PEM) the guest must trust.
    pub fn ca_pem(&self) -> &str {
        &self.ca_pem
    }

    /// Whether the traffic to `host` is inspected: some injection of the run covers it, or a
    /// protected secret must not go there.
    pub fn wants(&self, host: &str) -> bool {
        self.active.iter().any(|a| a.covers_host(host)) || self.leaks.as_ref().is_some_and(|l| l.guards(host))
    }

    fn server_config(&self, host: &str) -> Result<Arc<ServerConfig>, String> {
        if let Some(c) = self.leaves.lock().unwrap().get(host) {
            return Ok(c.clone());
        }
        let e = |e: &dyn std::fmt::Display| e.to_string();
        let mut params = CertificateParams::new(vec![host.to_string()]).map_err(|x| e(&x))?;
        params.distinguished_name = DistinguishedName::new();
        params.distinguished_name.push(DnType::CommonName, host);
        params.is_ca = IsCa::NoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let key = KeyPair::generate().map_err(|x| e(&x))?;
        let cert = params.signed_by(&key, &self.issuer).map_err(|x| e(&x))?;
        let mut config = ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(|x| e(&x))?
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(cert.der().to_vec())], PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())))
            .map_err(|x| e(&x))?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let config = Arc::new(config);
        self.leaves.lock().unwrap().insert(host.to_string(), config.clone());
        Ok(config)
    }

    /// Makes the certificates of the stockable hosts now (while the VM boots), so the guest's first
    /// TLS handshake to them does not wait for a key pair and a signature.
    pub fn pregenerate(&self) {
        for host in self.stockable() {
            let _ = self.server_config(&host);
        }
    }

    /// The exact hosts of the injections' rules (not the `*.host` ones): where a connection can be
    /// made ahead of time.
    pub fn stockable(&self) -> Vec<String> {
        let mut hosts: Vec<String> = self.active.iter().flat_map(|a| a.rules.iter()).map(|r| r.host.to_ascii_lowercase()).filter(|h| !h.starts_with('*')).collect();
        hosts.sort();
        hosts.dedup();
        hosts
    }

    /// How many of the connections ready for `host` are still usable.
    pub fn stocked(&self, host: &str) -> usize {
        self.stocked.lock().unwrap().get(host).map_or(0, |v| v.iter().filter(|r| r.usable()).count())
    }

    /// Signal that fires when a stocked connection was taken or the inspector is gone.
    pub fn refill_signal(&self) -> Arc<tokio::sync::Notify> {
        self.refill.clone()
    }

    /// Does the TLS and HTTP handshake with the real server on `tcp` and keeps the connection for
    /// the next guest connection to `host`. Nothing is sent that a guest did not ask for.
    pub async fn stock(&self, host: &str, tcp: TcpStream) -> Result<(), String> {
        let ready = self.handshake(host, tcp).await?;
        let mut all = self.stocked.lock().unwrap();
        let list = all.entry(host.to_string()).or_default();
        list.retain(|r| r.usable());
        if list.len() < STOCK_PER_HOST {
            list.push(ready);
        } else {
            ready.driver.abort();
        }
        Ok(())
    }

    /// A ready connection to `host`, if one is left and still alive.
    pub fn take(&self, host: &str) -> Option<Ready> {
        let taken = {
            let mut all = self.stocked.lock().unwrap();
            let list = all.get_mut(host)?;
            let mut found = None;
            while let Some(r) = list.pop() {
                if r.usable() {
                    found = Some(r);
                    break;
                }
                r.driver.abort();
            }
            found
        };
        self.refill.notify_one();
        taken
    }

    async fn handshake(&self, host: &str, tcp: TcpStream) -> Result<Ready, String> {
        let name = ServerName::try_from(host.to_string()).map_err(|e| e.to_string())?;
        let up = TlsConnector::from(self.upstream.clone()).connect(name, tcp).await.map_err(|e| format!("TLS handshake with the server failed: {e}"))?;
        let (sender, conn) = hyper::client::conn::http1::handshake::<_, Body>(TokioIo::new(up)).await.map_err(|e| e.to_string())?;
        let driver = tokio::spawn(async move {
            let _ = conn.await;
        });
        Ok(Ready { sender, driver, made: Instant::now() })
    }

    /// Serves one guest connection to `host` whose `CONNECT` was accepted: `client` is the guest's
    /// stream (what it sent after the `CONNECT` first), `upstream` the connection to the real
    /// server. Ends when either side closes.
    pub async fn serve<S>(&self, client: S, upstream: Upstream, host: &str, log: &(dyn Fn(String) + Sync))
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let config = match self.server_config(host) {
            Ok(c) => c,
            Err(e) => return log(format!("egress: {host}: cannot make a certificate: {e}")),
        };
        let tls = match TlsAcceptor::from(config).accept(client).await {
            Ok(t) => t,
            Err(e) => return log(format!("egress: {host}: TLS handshake with the guest failed: {e}")),
        };
        if !tls.get_ref().1.server_name().is_some_and(|n| n.eq_ignore_ascii_case(host)) {
            return log(format!("egress: {host}: refused, the TLS server name is not the CONNECT host"));
        }
        let Ready { sender, driver, .. } = match upstream {
            Upstream::Ready(r) => r,
            Upstream::Fresh(tcp) => match self.handshake(host, tcp).await {
                Ok(r) => r,
                Err(e) => return log(format!("egress: {host}: {e}")),
            },
        };
        let sender = tokio::sync::Mutex::new(sender);
        let service = service_fn(|req: Request<Incoming>| {
            let sender = &sender;
            async move { Ok::<_, Infallible>(self.handle(sender, host, req, log).await) }
        });
        let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(tls), service).await;
        driver.abort();
    }

    async fn handle(
        &self,
        sender: &tokio::sync::Mutex<hyper::client::conn::http1::SendRequest<Body>>,
        host: &str,
        req: Request<Incoming>,
        log: &(dyn Fn(String) + Sync),
    ) -> Response<Body> {
        let asked = req.headers().get(HOST).and_then(|h| h.to_str().ok()).map(|h| h.trim_end_matches(":443").trim_end_matches('.').to_ascii_lowercase());
        let uri_host = req.uri().host().map(|h| h.trim_end_matches('.').to_ascii_lowercase());
        if asked.as_deref() != Some(host) || uri_host.as_ref().is_some_and(|h| h != host) {
            log(format!("egress: {host}: refused a request for another host"));
            return reply(StatusCode::MISDIRECTED_REQUEST, "the Host header is not the host this tunnel was opened for\n");
        }
        if req.headers().contains_key(UPGRADE) || req.method() == hyper::Method::CONNECT {
            return reply(StatusCode::NOT_IMPLEMENTED, "protocol upgrades are not supported by the agent network policy\n");
        }
        // a protected secret must not leave for a host the profile does not name
        let mut req: Request<Body> = match self.leaks.as_ref().filter(|l| l.guards(host)) {
            None => req.map(|b| b.boxed()),
            Some(l) => {
                let target = req.uri().path_and_query().map_or(String::new(), |p| p.as_str().to_string());
                let refuse = |name: &str| {
                    log(format!("egress: {host}: refused a request that carries the secret {name}"));
                    reply(StatusCode::FORBIDDEN, &format!("blocked by the agent network policy: the request carries the value of the secret {name}, which this host may not receive\n"))
                };
                if let Some(name) = l.find_in_head(&target, req.headers()) {
                    return refuse(name);
                }
                let (parts, body) = req.into_parts();
                let bytes = match http_body_util::Limited::new(body, MAX_SCAN).collect().await {
                    Ok(c) => c.to_bytes(),
                    Err(_) => {
                        log(format!("egress: {host}: refused a request whose body cannot be checked for leaked secrets (over {} MiB)", MAX_SCAN / (1024 * 1024)));
                        return reply(StatusCode::PAYLOAD_TOO_LARGE, "blocked by the agent network policy: the body is too large to check for leaked secrets\n");
                    }
                };
                if let Some(name) = l.find(&bytes) {
                    return refuse(name);
                }
                Request::from_parts(parts, Full::new(bytes).map_err(|e: Infallible| match e {}).boxed())
            }
        };
        let (method, path) = (req.method().as_str().to_string(), req.uri().path().to_string());
        for a in self.active.iter().filter(|a| a.applies(host, &method, &path)) {
            let (Ok(name), Ok(mut value)) = (HeaderName::from_bytes(a.header.as_bytes()), HeaderValue::from_str(&a.value)) else {
                return reply(StatusCode::BAD_GATEWAY, "the injection is not usable\n");
            };
            value.set_sensitive(true);
            req.headers_mut().insert(name, value);
            log(format!("egress: {method} {host}{} gets {}", shown(&path), a.name));
        }
        req.headers_mut().remove("proxy-authorization");
        req.headers_mut().remove("proxy-connection");

        let mut s = sender.lock().await;
        if let Err(e) = s.ready().await {
            return reply(StatusCode::BAD_GATEWAY, &format!("the connection to the server is closed: {e}\n"));
        }
        match s.send_request(req).await {
            Ok(resp) => resp.map(|b| b.boxed()),
            Err(e) => reply(StatusCode::BAD_GATEWAY, &format!("the server did not answer: {e}\n")),
        }
    }
}

impl Ready {
    fn usable(&self) -> bool {
        !self.sender.is_closed() && self.made.elapsed() < STOCK_MAX_AGE
    }
}

impl Drop for Inspector {
    fn drop(&mut self) {
        for r in self.stocked.lock().unwrap().drain().flat_map(|(_, v)| v) {
            r.driver.abort();
        }
        self.refill.notify_one();
    }
}

/// A path for the log: no more than 100 characters.
fn shown(path: &str) -> String {
    path.chars().take(100).collect()
}

fn reply(status: StatusCode, text: &str) -> Response<Body> {
    let mut r = Response::new(Full::new(Bytes::from(text.to_string())).map_err(|e: Infallible| match e {}).boxed());
    *r.status_mut() = status;
    r.headers_mut().insert("content-type", HeaderValue::from_static("text/plain"));
    r
}

/// A stream that yields `prefix` before what `inner` sends: the bytes of the guest's first flight
/// that arrived in the same read as its `CONNECT`.
pub struct Prefixed<S> {
    prefix: Vec<u8>,
    at: usize,
    inner: S,
}

impl<S> Prefixed<S> {
    pub fn new(prefix: Vec<u8>, inner: S) -> Prefixed<S> {
        Prefixed { prefix, at: 0, inner }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Prefixed<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        if self.at < self.prefix.len() {
            let n = (self.prefix.len() - self.at).min(buf.remaining());
            let at = self.at;
            buf.put_slice(&self.prefix[at..at + n]);
            self.at += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Prefixed<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::egress::{Proxy, Rules};
    use crate::config::injections::Rule;
    use base64::Engine;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use std::time::Duration;

    /// A server on localhost with a certificate of its own CA; answers `<Authorization>|<X-Api-Key>|<path>`.
    async fn upstream() -> (u16, RootCertStore) {
        let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca_key = KeyPair::generate().unwrap();
        let ca_cert = ca.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca, ca_key);
        let key = KeyPair::generate().unwrap();
        let cert = CertificateParams::new(vec!["localhost".to_string()]).unwrap().signed_by(&key, &issuer).unwrap();
        let mut config = ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(cert.der().to_vec())], PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())))
            .unwrap();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((s, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(s).await else { return };
                    let svc = service_fn(|req: Request<Incoming>| async move {
                        let h = |n: &str| req.headers().get(n).and_then(|v| v.to_str().ok()).unwrap_or("-").to_string();
                        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!("{}|{}|{}", h("authorization"), h("x-api-key"), req.uri().path())))))
                    });
                    let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(tls), svc).await;
                });
            }
        });
        let mut roots = RootCertStore::empty();
        roots.add(CertificateDer::from(ca_cert.der().to_vec())).unwrap();
        (port, roots)
    }

    fn active(name: &str, header: &str, value: &str, path: &str, methods: &[&str]) -> Active {
        Active {
            name: name.into(),
            rules: vec![Rule { host: "localhost".into(), path: path.into(), methods: methods.iter().map(|m| m.to_string()).collect() }],
            header: header.into(),
            value: value.into(),
        }
    }

    #[tokio::test]
    async fn injects_into_matching_requests_only() {
        let (port, roots) = upstream().await;
        let inspector = Inspector::with_roots(
            vec![active("A", "Authorization", "Bearer real", "/user*", &["GET"]), active("B", "X-Api-Key", "k", "/key", &[])],
            roots,
        )
        .unwrap();
        let ca = inspector.ca_pem().to_string();
        assert!(inspector.wants("localhost") && !inspector.wants("other.test"));
        let proxy = Proxy::for_test(Rules::new([vec!["localhost".to_string()]]), port, Some(inspector));
        let log = Arc::new(Mutex::new(vec![]));
        let (mut guest, server) = tokio::io::duplex(1 << 16);
        let l = log.clone();
        tokio::spawn(async move { proxy.serve(server, move |s| l.lock().unwrap().push(s)).await });
        guest.write_all(format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n").as_bytes()).await.unwrap();
        let mut head = vec![];
        while !head.ends_with(b"\r\n\r\n") {
            head.push(guest.read_u8().await.unwrap());
        }
        assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"));

        // the guest trusts the run's CA, nothing else
        let der = base64::engine::general_purpose::STANDARD.decode(ca.lines().filter(|l| !l.starts_with("-----")).collect::<String>()).unwrap();
        let mut trust = RootCertStore::empty();
        trust.add(CertificateDer::from(der)).unwrap();
        let mut cfg = ClientConfig::builder_with_provider(provider()).with_safe_default_protocol_versions().unwrap().with_root_certificates(trust).with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        let tls = TlsConnector::from(Arc::new(cfg)).connect(ServerName::try_from("localhost").unwrap(), guest).await.unwrap();
        let (mut send, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(tls)).await.unwrap();
        tokio::spawn(conn);

        let mut ask = async |method: &str, path: &str, host: &str, extra: Option<(&str, &str)>| {
            let mut b = Request::builder().method(method).uri(path).header("host", host);
            if let Some((k, v)) = extra {
                b = b.header(k, v);
            }
            let resp = send.send_request(b.body(Full::new(Bytes::new())).unwrap()).await.unwrap();
            let status = resp.status();
            (status, String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap())
        };
        let own = Some(("authorization", "Bearer placeholder"));
        // matches A: the client's own header is replaced
        assert_eq!(ask("GET", "/user", "localhost", own).await.1, "Bearer real|-|/user");
        assert_eq!(ask("GET", "/user/emails?x=1", "localhost", None).await.1, "Bearer real|-|/user/emails");
        // another method, another path, a dot segment: not A, the client's header stays
        assert_eq!(ask("POST", "/user", "localhost", own).await.1, "Bearer placeholder|-|/user");
        assert_eq!(ask("GET", "/repos", "localhost", own).await.1, "Bearer placeholder|-|/repos");
        assert_eq!(ask("GET", "/user/../key", "localhost", None).await.1, "-|-|/user/../key");
        // B has no method restriction
        assert_eq!(ask("PUT", "/key", "localhost", None).await.1, "-|k|/key");
        // a request for another host through this tunnel
        let (status, _) = ask("GET", "/user", "evil.example", None).await;
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST);
        // the secret is never logged
        let log = log.lock().unwrap().join("\n");
        assert!(log.contains("gets A") && !log.contains("real"), "{log}");
    }

    /// A relay to `port` that makes every connection after the first `quick` ones wait 400 ms, and
    /// counts the connections.
    async fn slow_relay(port: u16, quick: usize) -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay = l.local_addr().unwrap().port();
        let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = n.clone();
        tokio::spawn(async move {
            while let Ok((mut a, _)) = l.accept().await {
                let seen = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    if seen >= quick {
                        tokio::time::sleep(Duration::from_millis(400)).await;
                    }
                    let Ok(mut b) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await else { return };
                    let _ = tokio::io::copy_bidirectional(&mut a, &mut b).await;
                });
            }
        });
        (relay, n)
    }

    #[tokio::test]
    async fn uses_a_connection_made_ahead_of_time_and_makes_another() {
        let (up, roots) = upstream().await;
        // only the first two connections to the server are quick: the stock; a third one is slow
        let (port, count) = slow_relay(up, 2).await;
        let inspector = Inspector::with_roots(vec![active("A", "Authorization", "Bearer real", "/user*", &["GET"])], roots).unwrap();
        assert_eq!(inspector.stockable(), vec!["localhost".to_string()]);
        let ca = inspector.ca_pem().to_string();
        let proxy = Proxy::for_test(Rules::new([vec!["localhost".to_string()]]), port, Some(inspector));
        proxy.warm();
        for _ in 0..100 {
            if proxy.stocked("localhost") == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(proxy.stocked("localhost"), 2, "two connections ready before any guest asked");
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 2);

        let (mut guest, server) = tokio::io::duplex(1 << 16);
        let p = proxy.clone();
        tokio::spawn(async move { p.serve(server, |_| {}).await });
        let started = std::time::Instant::now();
        guest.write_all(format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n").as_bytes()).await.unwrap();
        let mut head = vec![];
        while !head.ends_with(b"\r\n\r\n") {
            head.push(guest.read_u8().await.unwrap());
        }
        let der = base64::engine::general_purpose::STANDARD.decode(ca.lines().filter(|l| !l.starts_with("-----")).collect::<String>()).unwrap();
        let mut trust = RootCertStore::empty();
        trust.add(CertificateDer::from(der)).unwrap();
        let mut cfg = ClientConfig::builder_with_provider(provider()).with_safe_default_protocol_versions().unwrap().with_root_certificates(trust).with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        let tls = TlsConnector::from(Arc::new(cfg)).connect(ServerName::try_from("localhost").unwrap(), guest).await.unwrap();
        let (mut send, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(tls)).await.unwrap();
        tokio::spawn(conn);
        let resp = send.send_request(Request::builder().uri("/user").header("host", "localhost").body(Full::new(Bytes::new())).unwrap()).await.unwrap();
        let body = String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        assert_eq!(body, "Bearer real|-|/user", "the token is set on a request over a stocked connection too");
        // a fresh connection would have waited 400 ms in the relay
        assert!(started.elapsed() < Duration::from_millis(300), "took {:?}: the stock was not used", started.elapsed());

        // the stock is topped up again (through the slow relay)
        for _ in 0..200 {
            if proxy.stocked("localhost") == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(proxy.stocked("localhost"), 2);
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn does_not_connect_ahead_of_time_to_a_host_that_is_not_allowed() {
        let (up, roots) = upstream().await;
        let (port, count) = slow_relay(up, 10).await;
        let inspector = Inspector::with_roots(vec![active("A", "Authorization", "Bearer real", "", &[])], roots).unwrap();
        let proxy = Proxy::for_test(Rules::new([vec!["other.test".to_string()]]), port, Some(inspector));
        proxy.warm();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(proxy.stocked("localhost"), 0);
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn refuses_a_different_server_name() {
        let (port, roots) = upstream().await;
        let inspector = Inspector::with_roots(vec![active("A", "Authorization", "Bearer real", "", &[])], roots).unwrap();
        let ca = inspector.ca_pem().to_string();
        let proxy = Proxy::for_test(Rules::new([vec!["localhost".to_string()]]), port, Some(inspector));
        let (mut guest, server) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move { proxy.serve(server, |_| {}).await });
        guest.write_all(format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n").as_bytes()).await.unwrap();
        let mut head = vec![];
        while !head.ends_with(b"\r\n\r\n") {
            head.push(guest.read_u8().await.unwrap());
        }
        let der = base64::engine::general_purpose::STANDARD.decode(ca.lines().filter(|l| !l.starts_with("-----")).collect::<String>()).unwrap();
        let mut trust = RootCertStore::empty();
        trust.add(CertificateDer::from(der)).unwrap();
        let cfg = ClientConfig::builder_with_provider(provider()).with_safe_default_protocol_versions().unwrap().with_root_certificates(trust).with_no_client_auth();
        // the certificate is for localhost: asking for another name fails the guest's own check
        assert!(TlsConnector::from(Arc::new(cfg)).connect(ServerName::try_from("other.example").unwrap(), guest).await.is_err());
    }

    #[tokio::test]
    async fn refuses_a_request_that_carries_a_protected_secret_to_another_host() {
        use crate::net::leaks::{Leaks, Watched};
        let (port, roots) = upstream().await;
        // the secret may go to github.com only; `localhost` (this test's server) is not one of its hosts
        let leaks = Leaks::new(vec![Watched { name: "TOKEN".into(), value: "s3cret-token-value".into() }], Rules::new([vec!["github.com".to_string()]]));
        let inspector = Inspector::with_roots(vec![], roots).unwrap().watching(leaks);
        let ca = inspector.ca_pem().to_string();
        // no injection covers localhost: it is inspected because the secret must not go there
        assert!(inspector.wants("localhost") && !inspector.wants("github.com"));
        let proxy = Proxy::for_test(Rules::new([vec!["localhost".to_string()]]), port, Some(inspector));
        let log = Arc::new(Mutex::new(vec![]));
        let (mut guest, server) = tokio::io::duplex(1 << 16);
        let l = log.clone();
        tokio::spawn(async move { proxy.serve(server, move |s| l.lock().unwrap().push(s)).await });
        guest.write_all(format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n").as_bytes()).await.unwrap();
        let mut head = vec![];
        while !head.ends_with(b"\r\n\r\n") {
            head.push(guest.read_u8().await.unwrap());
        }
        let der = base64::engine::general_purpose::STANDARD.decode(ca.lines().filter(|l| !l.starts_with("-----")).collect::<String>()).unwrap();
        let mut trust = RootCertStore::empty();
        trust.add(CertificateDer::from(der)).unwrap();
        let mut cfg = ClientConfig::builder_with_provider(provider()).with_safe_default_protocol_versions().unwrap().with_root_certificates(trust).with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        let tls = TlsConnector::from(Arc::new(cfg)).connect(ServerName::try_from("localhost").unwrap(), guest).await.unwrap();
        let (mut send, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(tls)).await.unwrap();
        tokio::spawn(conn);

        let mut ask = async |method: &str, path: &str, header: Option<(&str, &str)>, body: &str| {
            let mut b = Request::builder().method(method).uri(path).header("host", "localhost");
            if let Some((k, v)) = header {
                b = b.header(k, v);
            }
            let resp = send.send_request(b.body(Full::new(Bytes::from(body.to_string()))).unwrap()).await.unwrap();
            let status = resp.status();
            (status, String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap())
        };
        // an ordinary request goes through
        assert_eq!(ask("GET", "/fine", None, "").await.0, StatusCode::OK);
        assert_eq!(ask("POST", "/fine", None, "just some text").await.0, StatusCode::OK);
        // the value in the address, a header, a Basic header and the body is refused
        assert_eq!(ask("GET", "/x?t=s3cret-token-value", None, "").await.0, StatusCode::FORBIDDEN);
        assert_eq!(ask("GET", "/fine", Some(("x-note", "s3cret-token-value")), "").await.0, StatusCode::FORBIDDEN);
        let basic = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("u:s3cret-token-value"));
        assert_eq!(ask("GET", "/fine", Some(("authorization", &basic)), "").await.0, StatusCode::FORBIDDEN);
        assert_eq!(ask("POST", "/fine", None, "data=s3cret-token-value").await.0, StatusCode::FORBIDDEN);
        let log = log.lock().unwrap().join("\n");
        assert!(log.contains("carries the secret TOKEN") && !log.contains("s3cret-token-value"), "{log}");
    }
}
