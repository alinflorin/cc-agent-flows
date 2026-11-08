//! The hub's side: talking to a node, shipping it an image it lacks, and the `Vm` handle of a VM
//! that runs there. `launch` is the remote twin of `vm::launch::launch`: same `LaunchSpec`, same
//! `Launched` (three streams and a handle), so the rest of a run cannot tell where the VM is.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio_rustls::client::TlsStream;

use super::proto::{Ctl, Hello, ImageRef, Info, Op, PROTO, Reply, WireDisk, WireSpec, read_json, write_json};
use super::{Node, tls};
use crate::vm::builder::Stream;
use crate::vm::launch::{LaunchError, LaunchSpec, Launched, Stats, Vm};

type Conn = TlsStream<TcpStream>;

/// Connects, says who the hub is and what it wants, and reads the node's first answer.
async fn open(node: &Node, op: Op, wait: Duration) -> Result<(Conn, Reply), String> {
    let (mut c, _) = tls::dial(&node.addr, Some(&node.fingerprint)).await?;
    write_json(&mut c, &Hello { proto: PROTO, key: node.key.clone(), op }).await?;
    let reply = tokio::time::timeout(wait, read_json::<Reply, _>(&mut c)).await.map_err(|_| format!("{}: no answer", node.name))??;
    Ok((c, reply))
}

fn refused(node: &Node, r: &Reply) -> String {
    format!("{}: {}", node.name, r.error.clone().unwrap_or_else(|| "refused".into()))
}

pub async fn ping(node: &Node) -> Result<Info, String> {
    let (_, r) = open(node, Op::Ping, Duration::from_secs(10)).await?;
    match r.info {
        Some(i) if r.ok => Ok(i),
        _ => Err(r.error.unwrap_or_else(|| "refused".into())),
    }
}

pub async fn drop_disks(node: &Node, prefix: &str) -> Result<(), String> {
    open(node, Op::Drop { prefix: prefix.to_string() }, Duration::from_secs(10)).await.map(|_| ())
}

fn upload_lock(key: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(Default::default).lock().unwrap().entry(key.to_string()).or_default().clone()
}

/// Makes sure the node has this version of the image, sending it if not.
async fn ensure_image(node: &Node, disk: &Path) -> Result<ImageRef, String> {
    let meta = std::fs::metadata(disk).map_err(|e| format!("the image: {e}"))?;
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    let image = ImageRef { key: disk.to_string_lossy().into_owned(), size: meta.len(), stamp: format!("{:x}-{:x}", meta.len(), mtime) };
    // one upload of an image to a node at a time
    let lock = upload_lock(&format!("{}|{}", node.id, image.key));
    let _guard = lock.lock().await;
    let (mut c, r) = open(node, Op::Image { image: image.clone() }, Duration::from_secs(150)).await?;
    if !r.ok {
        return Err(refused(node, &r));
    }
    if !r.need {
        return Ok(image);
    }
    log::info!("sending the image {} ({} MB) to {}", disk.display(), image.size / (1024 * 1024), node.name);
    let file = tokio::fs::File::open(disk).await.map_err(|e| format!("the image: {e}"))?;
    let send = async {
        let sent = tokio::io::copy(&mut file.take(image.size), &mut c).await.map_err(|e| e.to_string())?;
        if sent != image.size {
            return Err("the image changed while it was sent".to_string());
        }
        c.flush().await.map_err(|e| e.to_string())?;
        let done: Reply = tokio::time::timeout(Duration::from_secs(300), read_json(&mut c)).await.map_err(|_| "no answer after the image".to_string())??;
        if done.ok { Ok(()) } else { Err(done.error.unwrap_or_else(|| "refused".into())) }
    };
    tokio::time::timeout(Duration::from_secs(2 * 3600), send).await.map_err(|_| "sending the image took too long".to_string())?.map_err(|e| format!("{}: {e}", node.name))?;
    Ok(image)
}

type Exit = Option<(Option<String>, String)>;

/// A VM on a node: the control connection and what it has reported.
pub struct RemoteVm {
    tx: mpsc::UnboundedSender<Ctl>,
    exit: watch::Receiver<Exit>,
    stats: Arc<Mutex<Option<Stats>>>,
}

impl RemoteVm {
    pub async fn wait(&mut self) -> Option<String> {
        self.exit.wait_for(|e| e.is_some()).await.ok().and_then(|e| e.clone().and_then(|e| e.0))
    }

    pub fn stats(&self) -> Option<Stats> {
        self.stats.lock().unwrap().clone()
    }

    pub fn err_tail(&self) -> String {
        self.exit.borrow().as_ref().map(|e| e.1.clone()).unwrap_or_default()
    }

    /// Asks the node to stop the VM and waits (a little) for it to say so.
    pub async fn end(mut self) {
        let _ = self.tx.send(Ctl::Kill);
        let _ = tokio::time::timeout(Duration::from_secs(10), self.exit.wait_for(|e| e.is_some())).await;
    }
}

/// Starts the VM of a run on `node`.
pub async fn launch(node: &Node, spec: LaunchSpec, progress: &mut (dyn FnMut(&str) + Send)) -> Result<Launched, LaunchError> {
    use LaunchError::{Failed, Setup};
    let image = ensure_image(node, &spec.disk).await.map_err(Setup)?;
    let wire = WireSpec {
        image,
        cpus: spec.cpus,
        memory_mb: spec.memory_mb,
        work_disk: spec.work_disk.as_ref().map(|d| WireDisk { key: d.key.clone(), size_mb: d.size_mb }),
        shared: spec.shared,
        timeout_secs: spec.timeout_secs,
        grace_secs: spec.grace_secs,
    };
    let (ctl, r) = open(node, Op::Vm { spec: wire }, Duration::from_secs(150)).await.map_err(Setup)?;
    if !r.ok {
        let e = refused(node, &r);
        return Err(if r.kind.as_deref() == Some("failed") { Failed(e) } else { Setup(e) });
    }
    let vm = r.vm.ok_or_else(|| Setup("the node did not name the VM".into()))?;
    progress("starting");
    // a failure from here on drops `ctl`, which makes the node stop the VM
    let mut ports = vec!["agent", "net"];
    if spec.shared {
        ports.push("shared");
    }
    let mut streams: Vec<Box<dyn Stream>> = vec![];
    for p in ports {
        let (s, r) = open(node, Op::Attach { vm: vm.clone(), port: p.to_string() }, Duration::from_secs(30)).await.map_err(|e| Failed(format!("the {p} port: {e}")))?;
        if !r.ok {
            return Err(Failed(format!("the {p} port: {}", refused(node, &r))));
        }
        streams.push(Box::new(s));
    }
    let mut streams = streams.into_iter();
    let (agent, net, shared) = (streams.next().unwrap(), streams.next().unwrap(), streams.next());

    let (mut rd, mut wr) = tokio::io::split(ctl);
    let (tx, mut rx) = mpsc::unbounded_channel::<Ctl>();
    tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if write_json(&mut wr, &m).await.is_err() {
                break;
            }
        }
        let _ = wr.shutdown().await;
    });
    let (exit_tx, exit) = watch::channel::<Exit>(None);
    let stats = Arc::new(Mutex::new(None));
    let seen = stats.clone();
    let name = node.name.clone();
    tokio::spawn(async move {
        let end = loop {
            // the node reports every 2 s: silence means the computer or the network is gone
            match tokio::time::timeout(Duration::from_secs(15), read_json::<Ctl, _>(&mut rd)).await {
                Ok(Ok(Ctl::Stats { cpu, mem_mb, cpus })) => *seen.lock().unwrap() = Some(Stats { cpu, mem_mb, cpus }),
                Ok(Ok(Ctl::Exit { status, tail })) => break (status, tail),
                Ok(Ok(_)) => {}
                Ok(Err(e)) => break (Some(format!("lost the connection to {name}: {e}")), String::new()),
                Err(_) => break (Some(format!("lost the connection to {name}: no sign of life")), String::new()),
            }
        };
        let _ = exit_tx.send(Some(end));
    });
    Ok(Launched { agent, net, shared, vm: Vm::Remote(RemoteVm { tx, exit, stats }) })
}
