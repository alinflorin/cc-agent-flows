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
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

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

type Body = BoxBody<Bytes, hyper::Error>;

pub struct Inspector {
    active: Vec<Active>,
    ca_pem: String,
    issuer: Issuer<'static, KeyPair>,
    leaves: Mutex<HashMap<String, Arc<ServerConfig>>>,
    upstream: Arc<ClientConfig>,
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

impl Inspector {
    pub fn new(active: Vec<Active>) -> Result<Inspector, String> {
        let roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        Inspector::with_roots(active, roots)
    }

    /// With the roots the real servers are checked against (the tests use their own).
    pub fn with_roots(active: Vec<Active>, roots: RootCertStore) -> Result<Inspector, String> {
        let e = |e: &dyn std::fmt::Display| e.to_string();
        let mut params = CertificateParams::new(Vec::<String>::new()).map_err(|x| e(&x))?;
        params.distinguished_name = DistinguishedName::new();
        params.distinguished_name.push(DnType::CommonName, "CC Agent Flows run CA");
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let key = KeyPair::generate().map_err(|x| e(&x))?;
        let ca_pem = params.self_signed(&key).map_err(|x| e(&x))?.pem();
        let mut client = ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(|x| e(&x))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        client.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Inspector { active, ca_pem, issuer: Issuer::new(params, key), leaves: Mutex::new(HashMap::new()), upstream: Arc::new(client) })
    }

    /// The CA certificate (PEM) the guest must trust.
    pub fn ca_pem(&self) -> &str {
        &self.ca_pem
    }

    /// Whether the traffic to `host` is inspected: some injection of the run covers it.
    pub fn wants(&self, host: &str) -> bool {
        self.active.iter().any(|a| a.covers_host(host))
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

    /// Serves one guest connection to `host` whose `CONNECT` was accepted: `client` is the guest's
    /// stream (what it sent after the `CONNECT` first), `upstream` the TCP connection to the real
    /// server. Ends when either side closes.
    pub async fn serve<S>(&self, client: S, upstream: TcpStream, host: &str, log: &(dyn Fn(String) + Sync))
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
        let Ok(name) = ServerName::try_from(host.to_string()) else { return };
        let up = match TlsConnector::from(self.upstream.clone()).connect(name, upstream).await {
            Ok(t) => t,
            Err(e) => return log(format!("egress: {host}: TLS handshake with the server failed: {e}")),
        };
        let (sender, conn) = match hyper::client::conn::http1::handshake::<_, Incoming>(TokioIo::new(up)).await {
            Ok(v) => v,
            Err(e) => return log(format!("egress: {host}: {e}")),
        };
        let driver = tokio::spawn(async move {
            let _ = conn.await;
        });
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
        sender: &tokio::sync::Mutex<hyper::client::conn::http1::SendRequest<Incoming>>,
        host: &str,
        mut req: Request<Incoming>,
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
}
