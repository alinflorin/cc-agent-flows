//! The SSH server in the app that carries an agent VM's traffic to the egress proxy. QEMU's
//! `guestfwd=tcp:` gives the guest exactly one connection to the host, so the guest runs
//! `ssh -N -L 3128:egress:3128 ...` through it and SSH multiplexes the guest's connections as
//! channels. This server accepts one thing: a `direct-tcpip` channel to `egress:3128`, which is
//! handed to the proxy (`egress.rs`). No shell, exec, subsystem, agent, X11 or remote forwarding:
//! those channel kinds and requests are all refused (the defaults of `russh` drop them).
//!
//! One `Tunnel` per agent run. The client authenticates with the run's own key pair, generated
//! here and sent to the VM in the bootstrap, so the connection is the run's and nothing else's.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use russh::keys::{Algorithm, PrivateKey, PublicKey, ssh_key::LineEnding};
use russh::server::{Auth, Config, Handler, Msg, Session, VerifiedPublicKey, run_stream};
use russh::{Channel, ChannelOpenFailure};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::egress::Proxy;

/// What the guest asks for with `-L`: the name is a label for the proxy, nothing resolves it.
pub const PROXY_HOST: &str = "egress";
pub const PROXY_PORT: u32 = 3128;
/// The SSH user name the guest logs in as.
pub const USER: &str = "agent";

type Log = Arc<dyn Fn(String) + Send + Sync>;

/// A running tunnel server. Dropping it stops it and closes its connections.
pub struct Tunnel {
    /// The loopback port QEMU forwards the guest's connection to.
    pub port: u16,
    /// The client's private key, OpenSSH format, for the guest.
    pub client_key: String,
    /// The server's public key (`ssh-ed25519 AAAA...`), for the guest's `known_hosts`.
    pub host_key: String,
    task: JoinHandle<()>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Gate {
    client: PublicKey,
    proxy: Arc<Proxy>,
    log: Log,
}

impl Handler for Gate {
    type Error = russh::Error;

    async fn auth_publickey_offered(&mut self, _user: &str, offer: russh::server::PublicKeyOffer<'_>) -> Result<Auth, Self::Error> {
        Ok(if *offer.public_key == self.client { Auth::Accept } else { Auth::reject() })
    }

    async fn auth_publickey(&mut self, _user: &str, key: VerifiedPublicKey<'_>) -> Result<Auth, Self::Error> {
        Ok(if *key.public_key == self.client { Auth::Accept } else { Auth::reject() })
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if host_to_connect != PROXY_HOST || port_to_connect != PROXY_PORT {
            (self.log)(format!("tunnel: refused a channel to {host_to_connect}:{port_to_connect}"));
            reply.reject(ChannelOpenFailure::AdministrativelyProhibited).await;
            return Ok(());
        }
        reply.accept().await;
        let (proxy, log) = (self.proxy.clone(), self.log.clone());
        tokio::spawn(async move { proxy.serve(channel.into_stream(), |s| log(s)).await });
        Ok(())
    }
}

impl Tunnel {
    /// Starts the server on a free loopback port. QEMU connects to it when it starts, so it must
    /// be listening before the VM boots.
    pub async fn start(proxy: Arc<Proxy>, log: Log) -> Result<Tunnel, String> {
        // the generator is not Send: keep it out of the awaits below
        let (host, client) = {
            let mut rng = rand::rng();
            let host = PrivateKey::random(&mut rng, Algorithm::Ed25519).map_err(|e| e.to_string())?;
            let client = PrivateKey::random(&mut rng, Algorithm::Ed25519).map_err(|e| e.to_string())?;
            (host, client)
        };
        let host_key = host.public_key().to_openssh().map_err(|e| e.to_string())?;
        let client_key = client.to_openssh(LineEnding::LF).map_err(|e| e.to_string())?.to_string();
        let client_pub = client.public_key().clone();

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(|e| format!("cannot listen: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let config = Arc::new(Config {
            keys: vec![host],
            // The agent may be idle for long stretches; the run's own deadline ends the tunnel.
            inactivity_timeout: None,
            auth_rejection_time: Duration::from_millis(200),
            auth_rejection_time_initial: Some(Duration::ZERO),
            max_auth_attempts: 3,
            ..Config::default()
        });
        let task = tokio::spawn(async move {
            // QEMU may connect again (its forward is re-made when the guest restarts the tunnel).
            while let Ok((stream, _)) = listener.accept().await {
                let gate = Gate { client: client_pub.clone(), proxy: proxy.clone(), log: log.clone() };
                let config = config.clone();
                let log = log.clone();
                tokio::spawn(async move {
                    match run_stream(config, stream, gate).await {
                        Ok(session) => {
                            if let Err(e) = session.await {
                                log(format!("tunnel: session ended: {e}"));
                            }
                        }
                        Err(e) => log(format!("tunnel: handshake failed: {e}")),
                    }
                });
            }
        });
        Ok(Tunnel { port, client_key, host_key, task })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::client;
    use russh::keys::PrivateKeyWithHashAlg;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct Client;
    impl client::Handler for Client {
        type Error = russh::Error;
        async fn check_server_key(&mut self, _: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> {
            Ok(true)
        }
    }

    async fn connect(t: &Tunnel, key: &str) -> Option<client::Handle<Client>> {
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", t.port)).await.unwrap();
        let mut h = client::connect_stream(Arc::new(client::Config::default()), stream, Client).await.unwrap();
        let key = PrivateKey::from_openssh(key).unwrap();
        let ok = h.authenticate_publickey(USER, PrivateKeyWithHashAlg::new(Arc::new(key), None)).await.unwrap().success();
        ok.then_some(h)
    }

    #[tokio::test]
    async fn carries_only_proxy_channels_for_its_own_key() {
        // a local "internet" that answers with a greeting
        let site = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let site_port = site.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = site.accept().await {
                tokio::spawn(async move {
                    let _ = s.write_all(b"hi from the site").await;
                });
            }
        });
        let mut proxy = Proxy::new(crate::egress::Rules::new([vec!["localhost".to_string()]]), None);
        let p = Arc::get_mut(&mut proxy).unwrap();
        p.ports = vec![site_port];
        p.guard = false;
        let log_lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let l = log_lines.clone();
        let tunnel = Tunnel::start(proxy, Arc::new(move |s| l.lock().unwrap().push(s))).await.unwrap();
        assert!(tunnel.host_key.starts_with("ssh-ed25519 "));

        // another key is refused
        let other = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap().to_openssh(LineEnding::LF).unwrap();
        assert!(connect(&tunnel, &other).await.is_none());

        let h = connect(&tunnel, &tunnel.client_key).await.expect("the run's own key logs in");

        // not the proxy: refused
        assert!(h.channel_open_direct_tcpip("example.com", 443, "127.0.0.1", 1).await.is_err());
        assert!(h.channel_open_direct_tcpip(PROXY_HOST, 22, "127.0.0.1", 1).await.is_err());
        // not a forwarding channel: refused
        assert!(h.channel_open_session().await.is_err());

        // the proxy: a CONNECT to an allowed host goes through; several at once
        let mut jobs = vec![];
        for _ in 0..4 {
            let ch = h.channel_open_direct_tcpip(PROXY_HOST, PROXY_PORT, "127.0.0.1", 1).await.unwrap();
            jobs.push(tokio::spawn(async move {
                let mut s = ch.into_stream();
                s.write_all(format!("CONNECT localhost:{site_port} HTTP/1.1\r\n\r\n").as_bytes()).await.unwrap();
                let mut got = vec![];
                let mut chunk = [0u8; 256];
                while !String::from_utf8_lossy(&got).contains("hi from the site") {
                    let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut chunk)).await.unwrap().unwrap();
                    assert!(n > 0, "closed early: {}", String::from_utf8_lossy(&got));
                    got.extend_from_slice(&chunk[..n]);
                }
                String::from_utf8_lossy(&got).into_owned()
            }));
        }
        for j in jobs {
            assert!(j.await.unwrap().starts_with("HTTP/1.1 200"));
        }

        // a denied host comes back as a 403 through the tunnel
        let ch = h.channel_open_direct_tcpip(PROXY_HOST, PROXY_PORT, "127.0.0.1", 1).await.unwrap();
        let mut s = ch.into_stream();
        s.write_all(b"CONNECT evil.com:443 HTTP/1.1\r\n\r\n").await.unwrap();
        let mut out = vec![0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut out)).await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&out[..n]).starts_with("HTTP/1.1 403"));
        assert!(log_lines.lock().unwrap().iter().any(|l| l.contains("evil.com:443 denied")));
    }
}
