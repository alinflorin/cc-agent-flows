//! The agent's channel to the app: one virtio-serial port, one JSON message per line. The app
//! speaks first (the bootstrap), `agentd` answers `ready`, the app sends the task.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use protocol::{Bootstrap, Event, Final, Heartbeat, Inbound, MAX_MESSAGE, Outbound, Task};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};

type Reader = BufReader<Box<dyn AsyncRead + Send + Unpin>>;

enum Out {
    Line(Vec<u8>),
    /// Answered once everything before it has been written.
    Flush(oneshot::Sender<()>),
}

pub struct Link {
    tx: mpsc::UnboundedSender<Out>,
    reader: Mutex<Reader>,
}

impl Link {
    /// Opens the channel: a virtio-serial device, or `unix:<path>` for a socket (used by tests).
    /// The device may not exist for a moment after boot, so keep trying.
    pub async fn open(path: &str) -> Result<Arc<Link>, String> {
        let (reader, mut writer): (Box<dyn AsyncRead + Send + Unpin>, Box<dyn AsyncWrite + Send + Unpin>) =
            if let Some(sock) = path.strip_prefix("unix:") {
                let (r, w) = tokio::io::split(tokio::net::UnixStream::connect(sock).await.map_err(|e| format!("{sock}: {e}"))?);
                (Box::new(r), Box::new(w))
            } else {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
                let file = loop {
                    match std::fs::OpenOptions::new().read(true).write(true).open(Path::new(path)) {
                        Ok(f) => break f,
                        Err(e) if tokio::time::Instant::now() >= deadline => return Err(format!("{path}: {e}")),
                        Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
                    }
                };
                // A virtio port can be opened only once, so both halves share the descriptor.
                let w = file.try_clone().map_err(|e| e.to_string())?;
                (Box::new(tokio::fs::File::from_std(file)), Box::new(tokio::fs::File::from_std(w)))
            };
        let (tx, mut rx) = mpsc::unbounded_channel::<Out>();
        tokio::spawn(async move {
            while let Some(item) = rx.recv().await {
                match item {
                    Out::Line(bytes) => {
                        if writer.write_all(&bytes).await.is_err() || writer.flush().await.is_err() {
                            break;
                        }
                    }
                    Out::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        Ok(Arc::new(Link { tx, reader: Mutex::new(BufReader::new(reader)) }))
    }

    /// Reads one message. Until the app connects its end, a read returns end of file, so an empty
    /// read is retried until `wait` has passed.
    async fn read<T: DeserializeOwned>(&self, wait: Duration) -> Result<T, String> {
        let deadline = tokio::time::Instant::now() + wait;
        let mut reader = self.reader.lock().await;
        let mut line = String::new();
        loop {
            let n = (&mut *reader).take(MAX_MESSAGE as u64 + 1).read_line(&mut line).await.map_err(|e| e.to_string())?;
            if line.len() > MAX_MESSAGE {
                return Err("message too large".into());
            }
            if line.ends_with('\n') {
                return serde_json::from_str(&line).map_err(|e| format!("bad message: {e}"));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("no message from the app".into());
            }
            if n == 0 {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }

    pub async fn bootstrap(&self, wait: Duration) -> Result<Bootstrap, String> {
        self.read(wait).await
    }

    /// Tells the app the bootstrap arrived and waits for the task.
    pub async fn next_task(&self, wait: Duration) -> Result<Task, String> {
        self.send(&Outbound::Ready)?;
        match self.read::<Inbound>(wait).await? {
            Inbound::Task(t) => Ok(t),
        }
    }

    pub fn send(&self, msg: &Outbound) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(msg).map_err(|e| e.to_string())?;
        if bytes.len() >= MAX_MESSAGE {
            return Err("message too large".into());
        }
        bytes.push(b'\n');
        self.tx.send(Out::Line(bytes)).map_err(|_| "channel closed".to_string())
    }

    /// Publishes one line of `claude` output. Too large for a message, it is cut to a stub.
    pub fn event(&self, seq: u64, event: Value) -> Result<(), String> {
        let msg = Outbound::Event(Event { seq, event });
        let size = serde_json::to_vec(&msg).map(|v| v.len()).unwrap_or(usize::MAX);
        if size < MAX_MESSAGE / 2 {
            return self.send(&msg);
        }
        let Outbound::Event(e) = msg else { unreachable!() };
        let head: String = e.event.to_string().chars().take(4096).collect();
        self.send(&Outbound::Event(Event { seq, event: json!({ "truncated": true, "bytes": size, "head": head }) }))
    }

    /// One heartbeat every 10 seconds, so the app can tell a hung VM from a quiet one.
    pub fn start_heartbeat(self: &Arc<Self>) {
        let me = self.clone();
        let seq = AtomicU64::new(0);
        tokio::spawn(async move {
            loop {
                if me.send(&Outbound::Heartbeat(Heartbeat { seq: seq.fetch_add(1, Ordering::Relaxed) })).is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        });
    }

    /// Sends the final message and waits until it has been written to the channel.
    pub async fn finish(&self, fin: Final) -> Result<(), String> {
        self.send(&Outbound::Final(fin))?;
        let (done, wait) = oneshot::channel();
        self.tx.send(Out::Flush(done)).map_err(|_| "channel closed".to_string())?;
        wait.await.map_err(|_| "channel closed".to_string())
    }
}
