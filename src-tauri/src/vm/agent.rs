//! The app's end of an agent VM's channel (see `protocol.rs`): the VM's virtio-serial
//! socket, one JSON message per line. `drive` plays the app's part of the exchange: bootstrap
//! first, the task when the agent says it is ready, then everything the agent sends.
//!
//! The stream must stay open until the run is over: QEMU drops what the guest has not read yet when
//! the host disconnects, and the VM boots long after the app connects.

use crate::vm::protocol::{Bootstrap, Inbound, MAX_MESSAGE, Outbound, Task};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

async fn write_line<W: AsyncWrite + Unpin>(w: &mut W, value: &impl serde::Serialize) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    w.write_all(&bytes).await?;
    w.flush().await
}

/// Runs the exchange on `stream` and returns what the agent sends (`Ready` excluded). The channel
/// closes when the stream ends, the agent sends an oversized line, or the receiver is dropped.
/// Lines that do not parse are skipped: the agent is untrusted and may print anything.
pub fn drive<S>(stream: S, boot: Bootstrap, task: Task) -> mpsc::Receiver<Outbound>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (tx, rx) = mpsc::channel(256);
    tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(stream);
        let mut read = BufReader::new(read);
        // The task goes right behind the bootstrap: the channel buffers it until agentd reads it, so
        // there is no round trip to wait for after the guest boots. agentd still says `ready`.
        if write_line(&mut write, &boot).await.is_err() || write_line(&mut write, &Inbound::Task(task)).await.is_err() {
            return;
        }
        let mut line = String::new();
        loop {
            line.clear();
            match (&mut read).take(MAX_MESSAGE as u64 + 1).read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) if line.len() > MAX_MESSAGE => return,
                Ok(_) => {}
            }
            let Ok(msg) = serde_json::from_str::<Outbound>(&line) else { continue };
            match msg {
                Outbound::Ready => {}
                other => {
                    if tx.send(other).await.is_err() {
                        return;
                    }
                }
            }
        }
    });
    rx
}
