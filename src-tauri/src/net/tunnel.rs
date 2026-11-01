//! The app's end of an agent VM's network path. The VM has no network card: its only link is a
//! virtio-serial port (`net`), and `ccaf-relay` in the guest carries each connection of `claude`
//! to its local proxy port as one stream of a yamux session over that port (see `relay/`). This
//! end accepts those streams and hands each to the egress proxy (`egress.rs`), which is the only
//! thing a stream can reach: there is no channel kind that names a destination, and the app never
//! opens a stream towards the guest.
//!
//! One `Tunnel` per agent run, on the run's port connection. There is no authentication: the port
//! belongs to this VM alone, like the agent channel.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::task::JoinHandle;
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};

use crate::net::egress::Proxy;

type Log = Arc<dyn Fn(String) + Send + Sync>;

/// A running session. Dropping it stops it and closes its streams.
pub struct Tunnel {
    task: JoinHandle<()>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Tunnel {
    /// Serves the guest's streams on `io` (the connection to the VM's `net` port) until the VM
    /// closes it.
    pub fn start<S>(io: S, proxy: Arc<Proxy>, log: Log) -> Tunnel
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let task = tokio::spawn(async move {
            let l = log.clone();
            ccaf_relay::serve(io.compat(), move |stream| {
                let (proxy, log) = (proxy.clone(), l.clone());
                tokio::spawn(async move { proxy.serve(stream.compat(), |s| log(s)).await });
            })
            .await;
            log("tunnel: the guest closed the network port".into());
        });
        Tunnel { task }
    }
}

/// Readiness check (About page): runs a session over an in-memory pipe the way a run does, opens a
/// stream like the guest's relay and sends a `CONNECT` that must be denied (no host is allowed).
pub async fn self_check() -> Result<String, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (guest, app) = tokio::io::duplex(64 * 1024);
    let _tunnel = Tunnel::start(app, Proxy::new(crate::net::egress::Rules::default(), None), Arc::new(|_| {}));
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(ccaf_relay::dial(guest.compat(), rx));
    let probe = async {
        let (otx, orx) = tokio::sync::oneshot::channel();
        tx.send(otx).await.map_err(|e| e.to_string())?;
        let mut s = orx.await.map_err(|e| e.to_string())?.map_err(|e| format!("stream: {e}"))?.compat();
        s.write_all(b"CONNECT example.com:443 HTTP/1.1\r\n\r\n").await.map_err(|e| e.to_string())?;
        let mut buf = [0u8; 64];
        let n = s.read(&mut buf).await.map_err(|e| e.to_string())?;
        if !buf[..n].starts_with(b"HTTP/1.1 403") {
            return Err("the proxy did not deny an unlisted host".to_string());
        }
        Ok("virtio-serial + yamux".to_string())
    };
    tokio::time::timeout(Duration::from_secs(5), probe).await.map_err(|_| "timed out".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn the_readiness_check_passes() {
        self_check().await.unwrap();
    }

    #[tokio::test]
    async fn carries_proxy_connections() {
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
        let mut proxy = Proxy::new(crate::net::egress::Rules::new([vec!["localhost".to_string()]]), None);
        let p = Arc::get_mut(&mut proxy).unwrap();
        p.ports = vec![site_port];
        p.guard = false;
        let log_lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let l = log_lines.clone();
        let (guest, app) = tokio::io::duplex(64 * 1024);
        let _tunnel = Tunnel::start(app, proxy, Arc::new(move |s| l.lock().unwrap().push(s)));
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(ccaf_relay::dial(guest.compat(), rx));

        let open = || async {
            let (otx, orx) = tokio::sync::oneshot::channel();
            tx.send(otx).await.unwrap();
            orx.await.unwrap().unwrap().compat()
        };

        // several at once: a CONNECT to an allowed host goes through
        let mut jobs = vec![];
        for _ in 0..4 {
            let mut s = open().await;
            jobs.push(tokio::spawn(async move {
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
        let mut s = open().await;
        s.write_all(b"CONNECT evil.com:443 HTTP/1.1\r\n\r\n").await.unwrap();
        let mut out = vec![0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut out)).await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&out[..n]).starts_with("HTTP/1.1 403"));
        assert!(log_lines.lock().unwrap().iter().any(|l| l.contains("evil.com:443 denied")));
    }
}
