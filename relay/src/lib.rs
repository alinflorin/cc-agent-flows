//! The two ends of the agent VM's network path: one virtio-serial port carries a yamux session, and
//! each TCP connection of the guest is one stream in it. The guest end (`ccaf-relay`, `main.rs`) opens
//! a stream for every connection to its local proxy port; the app end accepts them and hands each to
//! the egress proxy. The app never opens streams towards the guest, and the guest has no other way
//! out: its NIC does not exist.
//!
//! Shared by the app (as a dependency) and the guest binary, so both speak the same yamux settings.

use std::future::poll_fn;
use std::task::Poll;

use futures::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};

pub use yamux::{Config, Connection, ConnectionError, Mode, Stream};

/// The local port the guest's `HTTPS_PROXY` points at.
pub const PROXY_PORT: u16 = 3128;

/// Connections at once through one session (the proxy has its own, lower, limit).
const MAX_STREAMS: usize = 256;

pub fn config() -> Config {
    let mut c = Config::default();
    c.set_max_num_streams(MAX_STREAMS);
    c
}

/// The app's end: runs the session on `io` and calls `on_stream` for every stream the guest opens,
/// until the session ends (the guest closed it, the VM is gone, or the peer broke the protocol).
pub async fn serve<T>(io: T, mut on_stream: impl FnMut(Stream))
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut conn = Connection::new(io, config(), Mode::Server);
    poll_fn(|cx| loop {
        match conn.poll_next_inbound(cx) {
            Poll::Ready(Some(Ok(s))) => on_stream(s),
            Poll::Ready(_) => return Poll::Ready(()),
            Poll::Pending => return Poll::Pending,
        }
    })
    .await
}

/// A request for a new stream: the answer comes back on the channel.
pub type Open = oneshot::Sender<Result<Stream, ConnectionError>>;

/// The guest's end: runs the session on `io`, opening a stream for each request. Streams the peer
/// opens are dropped (the app has no reason to). Ends when the session does; requests still queued
/// then see their sender dropped.
pub async fn dial<T>(io: T, mut requests: mpsc::Receiver<Open>)
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut conn = Connection::new(io, config(), Mode::Client);
    let mut waiting: Option<Open> = None;
    poll_fn(|cx| {
        loop {
            // the connection must be polled for its I/O, whatever else is going on
            match conn.poll_next_inbound(cx) {
                Poll::Ready(Some(Ok(_unwanted))) => continue,
                Poll::Ready(_) => return Poll::Ready(()),
                Poll::Pending => {}
            }
            if waiting.is_none() {
                match requests.poll_recv(cx) {
                    Poll::Ready(Some(tx)) => waiting = Some(tx),
                    Poll::Ready(None) => return Poll::Ready(()),
                    Poll::Pending => return Poll::Pending,
                }
            }
            match conn.poll_new_outbound(cx) {
                Poll::Ready(r) => {
                    if let Some(tx) = waiting.take() {
                        let _ = tx.send(r);
                    }
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};

    /// Echoes one line back, upper-cased, per stream.
    async fn app_end(io: tokio::io::DuplexStream) {
        serve(io.compat(), |s| {
            tokio::spawn(async move {
                let mut s = s.compat();
                let mut buf = vec![0u8; 5];
                s.read_exact(&mut buf).await.unwrap();
                s.write_all(&buf.to_ascii_uppercase()).await.unwrap();
                s.shutdown().await.unwrap();
            });
        })
        .await
    }

    #[tokio::test]
    async fn many_streams_share_one_pipe() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        tokio::spawn(app_end(b));
        let (tx, rx) = mpsc::channel::<Open>(8);
        tokio::spawn(dial(a.compat(), rx));

        let mut jobs = vec![];
        for i in 0..20 {
            let tx = tx.clone();
            jobs.push(tokio::spawn(async move {
                let (otx, orx) = oneshot::channel();
                tx.send(otx).await.unwrap();
                let mut s = orx.await.unwrap().unwrap().compat();
                s.write_all(format!("hi{i:03}").as_bytes()[..5].as_ref()).await.unwrap();
                let mut out = vec![];
                s.read_to_end(&mut out).await.unwrap();
                String::from_utf8(out).unwrap()
            }));
        }
        for (i, j) in jobs.into_iter().enumerate() {
            assert_eq!(j.await.unwrap(), format!("HI{:03}", i)[..5].to_string());
        }
    }

    #[tokio::test]
    async fn a_big_transfer_with_flow_control() {
        let (a, b) = tokio::io::duplex(16 * 1024);
        tokio::spawn(serve(b.compat(), |s| {
            tokio::spawn(async move {
                let mut s = s.compat();
                let mut n = 0usize;
                let mut buf = vec![0u8; 8192];
                loop {
                    let r = s.read(&mut buf).await.unwrap();
                    if r == 0 {
                        break;
                    }
                    n += r;
                }
                s.write_all(n.to_string().as_bytes()).await.unwrap();
                s.shutdown().await.unwrap();
            });
        }));
        let (tx, rx) = mpsc::channel::<Open>(1);
        tokio::spawn(dial(a.compat(), rx));
        let (otx, orx) = oneshot::channel();
        tx.send(otx).await.unwrap();
        let mut s = orx.await.unwrap().unwrap().compat();
        let data = vec![7u8; 5 * 1024 * 1024];
        s.write_all(&data).await.unwrap();
        s.shutdown().await.unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        assert_eq!(out, data.len().to_string());
    }
}
