//! The guest's side of the session: the virtio-serial port as an async stream, and the accept loop.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Sleep;
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};

use ccaf_relay::Open;

/// A virtio-serial port. It cannot be opened twice, so one descriptor serves reads and writes.
/// While the app has not connected to its end, reads return end of file: that is not the end of the
/// session here, just "not yet", so the read is tried again shortly.
struct Port {
    fd: AsyncFd<std::os::fd::OwnedFd>,
    retry: Pin<Box<Sleep>>,
    waiting: bool,
}

impl Port {
    fn open(path: &str) -> io::Result<Port> {
        use std::os::fd::{FromRawFd, OwnedFd};
        let c = std::ffi::CString::new(path).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        // SAFETY: plain open(2) on a NUL-terminated path; the descriptor is owned right away.
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::new(io::Error::last_os_error().kind(), format!("cannot open {path}: {}", io::Error::last_os_error())));
        }
        // SAFETY: `fd` is valid and nothing else owns it.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: the descriptor is open and owned by the `AsyncFd` from here on.
        let fd = unsafe { AsyncFd::register_with_interest(fd, tokio::io::Interest::READABLE | tokio::io::Interest::WRITABLE) }.map_err(io::Error::from)?;
        Ok(Port { fd, retry: Box::pin(tokio::time::sleep(Duration::ZERO)), waiting: false })
    }
}

impl AsyncRead for Port {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        use std::os::fd::AsRawFd;
        loop {
            if self.waiting {
                ready!(self.retry.as_mut().poll(cx));
                self.waiting = false;
            }
            let mut guard = ready!(self.fd.poll_read_ready(cx))?;
            let unfilled = buf.initialize_unfilled();
            // SAFETY: reads into the initialised, unfilled part of `buf`.
            let n = unsafe { libc::read(self.fd.as_raw_fd(), unfilled.as_mut_ptr().cast(), unfilled.len()) };
            if n < 0 {
                let e = io::Error::last_os_error();
                match e.kind() {
                    io::ErrorKind::WouldBlock => {
                        guard.clear_ready();
                        continue;
                    }
                    io::ErrorKind::Interrupted => continue,
                    // ENODEV/EIO are what the port says while the app's end is not there
                    _ => {
                        self.retry.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(50));
                        self.waiting = true;
                        continue;
                    }
                }
            }
            if n == 0 {
                self.retry.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(50));
                self.waiting = true;
                continue;
            }
            buf.advance(n as usize);
            return Poll::Ready(Ok(()));
        }
    }
}

impl AsyncWrite for Port {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        use std::os::fd::AsRawFd;
        loop {
            let mut guard = ready!(self.fd.poll_write_ready(cx))?;
            // SAFETY: writes from a valid slice.
            let n = unsafe { libc::write(self.fd.as_raw_fd(), data.as_ptr().cast(), data.len()) };
            if n < 0 {
                let e = io::Error::last_os_error();
                match e.kind() {
                    io::ErrorKind::WouldBlock => {
                        guard.clear_ready();
                        continue;
                    }
                    io::ErrorKind::Interrupted => continue,
                    _ => return Poll::Ready(Err(e)),
                }
            }
            return Poll::Ready(Ok(n as usize));
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

pub async fn run(path: &str, listen: &str, ready: impl FnOnce()) -> Result<(), String> {
    let port = Port::open(path).map_err(|e| e.to_string())?;
    let listener = TcpListener::bind(listen).await.map_err(|e| format!("cannot listen on {listen}: {e}"))?;
    ready();

    let (tx, rx) = mpsc::channel::<Open>(64);
    tokio::spawn(ccaf_relay::dial(port.compat(), rx));
    loop {
        let (tcp, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let _ = tcp.set_nodelay(true);
        let tx = tx.clone();
        tokio::spawn(async move {
            let (otx, orx) = oneshot::channel();
            if tx.send(otx).await.is_err() {
                return;
            }
            let Ok(Ok(stream)) = orx.await else { return };
            let mut stream = stream.compat();
            let mut tcp = tcp;
            let _ = tokio::io::copy_bidirectional_with_sizes(&mut tcp, &mut stream, 64 * 1024, 64 * 1024).await;
        });
    }
}
