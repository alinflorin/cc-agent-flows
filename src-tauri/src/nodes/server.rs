//! The node's side: a TLS listener that starts VMs for a hub. Every connection says what it wants
//! in its first line (`proto.rs`) and proves it knows the node's key; then it is served by
//! `ping`, `image` (a hub's image into the node's cache), `vm` (starts a VM with
//! `vm::launch::launch`, the same code as a local run, and stays as its control channel),
//! `attach` (bridges a port of that VM to the connection) or `drop` (deletes work disks).
//! A VM ends when the hub says so, when the control connection breaks, or when a limit that
//! outlasts the hub's own deadline passes, so a hub that vanishes leaves nothing running.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tauri::AppHandle;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use super::proto::{Ctl, Hello, ImageRef, Info, Op, PROTO, Reply, WireSpec, read_json, write_json};
use super::{NodeConfig, forced, node_config, same, state};
use crate::vm::builder::Stream;
use crate::vm::launch::{LaunchError, LaunchSpec};
use crate::vm::runs::WorkDisk;

/// Largest image taken (a built image is a few GB).
const MAX_IMAGE: u64 = 200 * 1024 * 1024 * 1024;

/// Starts listening (stops the listener that was running), when this computer is a node.
pub fn start(app: &AppHandle) {
    let s = state(app);
    if let Some(t) = s.server.lock().unwrap().take() {
        t.abort();
    }
    *s.server_error.lock().unwrap() = None;
    let cfg = node_config(app);
    if !(cfg.enabled || forced()) {
        return;
    }
    let app2 = app.clone();
    let task = tauri::async_runtime::spawn(async move {
        if let Err(e) = listen(&app2, &cfg).await {
            log::warn!("node server: {e}");
            *state(&app2).server_error.lock().unwrap() = Some(e);
        }
    });
    *s.server.lock().unwrap() = Some(task);
}

async fn listen(app: &AppHandle, cfg: &NodeConfig) -> Result<(), String> {
    let acceptor = super::tls::Identity::load(app)?.acceptor()?;
    let ip: std::net::IpAddr = cfg.bind.parse().map_err(|_| "the address must be an IP address".to_string())?;
    let addr = std::net::SocketAddr::new(ip, cfg.port);
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| format!("cannot listen on {addr}: {e}"))?;
    log::info!("accepting VMs from other computers on {addr}");
    loop {
        let Ok((tcp, _)) = listener.accept().await else { continue };
        let _ = tcp.set_nodelay(true);
        let (app, acceptor) = (app.clone(), acceptor.clone());
        tauri::async_runtime::spawn(async move {
            let Ok(Ok(tls)) = tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await else { return };
            serve(&app, tls).await;
        });
    }
}

type Conn = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;

fn info() -> Info {
    Info { version: env!("CARGO_PKG_VERSION").to_string(), arch: crate::vm::machine::arch().unwrap_or("?").to_string(), os: std::env::consts::OS.to_string() }
}

async fn serve(app: &AppHandle, mut c: Conn) {
    let hello: Hello = match tokio::time::timeout(Duration::from_secs(10), read_json(&mut c)).await {
        Ok(Ok(h)) => h,
        _ => return,
    };
    if hello.proto != PROTO {
        let _ = write_json(&mut c, &Reply::err(format!("this node speaks protocol {PROTO}, the hub {}: update the app on both", hello.proto))).await;
        return;
    }
    if !same(&hello.key, &node_config(app).key) {
        // a guess costs the guesser time
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = write_json(&mut c, &Reply::err("the key is wrong")).await;
        return;
    }
    match hello.op {
        Op::Ping => {
            let _ = write_json(&mut c, &Reply { ok: true, info: Some(info()), ..Default::default() }).await;
        }
        Op::Image { image } => image_op(app, c, image).await,
        Op::Vm { spec } => vm_op(app, c, spec).await,
        Op::Attach { vm, port } => attach(c, vm, port).await,
        Op::Drop { prefix } => {
            let _ = write_json(&mut c, &Reply::ok()).await;
            drop_disks(app, &prefix);
        }
    }
}

fn dir(app: &AppHandle, name: &str) -> Result<PathBuf, String> {
    let d = crate::util::data_dir(app)?.join("node").join(name);
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    Ok(d)
}

/// A name safe in a file name; keeps prefixes (a prefix of the key is a prefix of the name).
fn safe(key: &str) -> String {
    key.bytes().map(|b| if b.is_ascii_alphanumeric() || b == b'-' { (b as char).to_string() } else { format!("_{b:02x}") }).collect()
}

fn image_prefix(key: &str) -> String {
    Sha256::digest(key.as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn image_path(app: &AppHandle, image: &ImageRef) -> Result<PathBuf, String> {
    if image.stamp.is_empty() || !image.stamp.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err("bad image version".into());
    }
    Ok(dir(app, "images")?.join(format!("{}-{}.qcow2", image_prefix(&image.key), image.stamp)))
}

fn lock_for(key: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(Default::default).lock().unwrap().entry(key.to_string()).or_default().clone()
}

async fn image_op(app: &AppHandle, mut c: Conn, image: ImageRef) {
    let fail = |e: String| Reply::err(e);
    let path = match image_path(app, &image) {
        Ok(p) if image.size > 0 && image.size <= MAX_IMAGE => p,
        Ok(_) => return drop(write_json(&mut c, &fail("bad image size".into())).await),
        Err(e) => return drop(write_json(&mut c, &fail(e)).await),
    };
    // two runs that need the same image at once: one receives it, the other finds it
    let lock = lock_for(&image.key);
    let _guard = lock.lock().await;
    if path.exists() {
        let _ = write_json(&mut c, &Reply::ok()).await;
        return;
    }
    if write_json(&mut c, &Reply { ok: true, need: true, ..Default::default() }).await.is_err() {
        return;
    }
    let tmp = path.with_extension(format!("tmp-{}", crate::vm::runs::new_run_id()));
    let res: Result<(), String> = async {
        let mut file = tokio::fs::File::create(&tmp).await.map_err(|e| e.to_string())?;
        let got = tokio::io::copy(&mut (&mut c).take(image.size), &mut file).await.map_err(|e| e.to_string())?;
        file.flush().await.map_err(|e| e.to_string())?;
        if got != image.size {
            return Err("the image was cut short".into());
        }
        // older versions of this image are of no use any more
        let prefix = format!("{}-", image_prefix(&image.key));
        for old in std::fs::read_dir(path.parent().unwrap()).into_iter().flatten().flatten() {
            let n = old.file_name().to_string_lossy().into_owned();
            if n.starts_with(&prefix) && n.ends_with(".qcow2") {
                let _ = std::fs::remove_file(old.path());
            }
        }
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }
    .await;
    let _ = match res {
        Ok(()) => write_json(&mut c, &Reply::ok()).await,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            write_json(&mut c, &Reply::err(e)).await
        }
    };
}

/// Streams of started VMs that the hub has not connected to yet: (vm, port).
type Pending = Mutex<HashMap<(String, String), Box<dyn Stream>>>;

fn pending() -> &'static Pending {
    static P: OnceLock<Pending> = OnceLock::new();
    P.get_or_init(Default::default)
}

async fn attach(mut c: Conn, vm: String, port: String) {
    let stream = pending().lock().unwrap().remove(&(vm, port));
    let Some(mut stream) = stream else {
        let _ = write_json(&mut c, &Reply::err("no such port")).await;
        return;
    };
    if write_json(&mut c, &Reply::ok()).await.is_err() {
        return;
    }
    let _ = tokio::io::copy_bidirectional(&mut c, &mut stream).await;
}

fn drop_disks(app: &AppHandle, prefix: &str) {
    let Ok(d) = dir(app, "work") else { return };
    let prefix = safe(prefix);
    for f in std::fs::read_dir(d).into_iter().flatten().flatten() {
        if f.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(f.path());
        }
    }
}

async fn vm_op(app: &AppHandle, mut c: Conn, spec: WireSpec) {
    let id = format!("v-{}", crate::vm::runs::new_run_id());
    let timeout = spec.timeout_secs.saturating_add(spec.grace_secs);
    let launched = match prepare(app, &id, spec) {
        Ok(ls) => crate::vm::launch::launch(app, ls, &mut |_| {}).await,
        Err(e) => Err(LaunchError::Setup(e)),
    };
    let l = match launched {
        Ok(l) => l,
        Err(e) => {
            let (kind, msg) = match e {
                LaunchError::Setup(m) => ("setup", m),
                LaunchError::Failed(m) => ("failed", m),
            };
            let _ = write_json(&mut c, &Reply { ok: false, error: Some(msg), kind: Some(kind.into()), ..Default::default() }).await;
            return;
        }
    };
    let mut vm = l.vm;
    {
        let mut p = pending().lock().unwrap();
        p.insert((id.clone(), "agent".into()), l.agent);
        p.insert((id.clone(), "net".into()), l.net);
        if let Some(s) = l.shared {
            p.insert((id.clone(), "shared".into()), s);
        }
    }
    if write_json(&mut c, &Reply { ok: true, vm: Some(id.clone()), ..Default::default() }).await.is_err() {
        pending().lock().unwrap().retain(|k, _| k.0 != id);
        vm.end().await;
        return;
    }
    // what the hub says (read in its own task: a half-read line must not be lost to a select)
    let (mut rd, mut wr) = tokio::io::split(c);
    let (tx, mut from_hub) = mpsc::channel::<Ctl>(8);
    let reader = tokio::spawn(async move {
        while let Ok(m) = read_json::<Ctl, _>(&mut rd).await {
            if tx.send(m).await.is_err() {
                break;
            }
        }
    });
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // the hub's own deadline comes first; this one is for a hub that is gone
    let limit = tokio::time::sleep(Duration::from_secs(timeout.saturating_add(180)));
    tokio::pin!(limit);
    loop {
        tokio::select! {
            m = from_hub.recv() => match m {
                Some(Ctl::Kill) | None => break,
                Some(_) => {}
            },
            _ = tick.tick() => {
                let msg = match vm.stats() {
                    Some(s) => Ctl::Stats { cpu: s.cpu, mem_mb: s.mem_mb, cpus: s.cpus },
                    None => Ctl::Beat,
                };
                if write_json(&mut wr, &msg).await.is_err() {
                    break;
                }
            }
            status = vm.wait() => {
                let _ = write_json(&mut wr, &Ctl::Exit { status, tail: vm.err_tail() }).await;
                break;
            }
            _ = &mut limit => break,
        }
    }
    reader.abort();
    pending().lock().unwrap().retain(|k, _| k.0 != id);
    vm.end().await;
    let _ = wr.shutdown().await;
}

/// The wire spec as a local launch: the image from the cache, the work disk under the node's own folder.
fn prepare(app: &AppHandle, id: &str, spec: WireSpec) -> Result<LaunchSpec, String> {
    let disk = image_path(app, &spec.image)?;
    if !disk.exists() {
        return Err("the image is not on this node".into());
    }
    if spec.memory_mb == 0 || spec.memory_mb > 1 << 20 || spec.cpus.is_some_and(|c| c == 0 || c > 256) {
        return Err("bad VM size".into());
    }
    let work_disk = match spec.work_disk {
        Some(d) => {
            if d.key.is_empty() || d.key.len() > 400 || d.size_mb == 0 || d.size_mb > 4_000_000 {
                return Err("bad work disk".into());
            }
            Some(WorkDisk { path: dir(app, "work")?.join(format!("{}.qcow2", safe(&d.key))), size_mb: d.size_mb, key: d.key })
        }
        None => None,
    };
    Ok(LaunchSpec { name: format!("node-{id}"), disk, cpus: spec.cpus, memory_mb: spec.memory_mb, work_disk, shared: spec.shared, timeout_secs: spec.timeout_secs, grace_secs: spec.grace_secs })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_names_keep_prefixes() {
        let (full, prefix) = (safe("demo/s-1/dev"), safe("demo/s-1/"));
        assert!(full.starts_with(&prefix));
        assert!(!full.contains('/'));
        assert_ne!(safe("a/b"), safe("a_b"));
    }
}
