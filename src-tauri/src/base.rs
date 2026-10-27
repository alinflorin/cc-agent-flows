//! The base image: the plain Alpine image plus agentd, Claude Code and ripgrep (see
//! `resources/agentd/setup.sh`). Not editable. The app builds it in a builder VM into
//! `<app data>/images/base.qcow2` when it is missing or out of date (a new plain image or setup
//! files come with an app update), and on request from the settings.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};
use tokio::process::Command;

use crate::builder::{Console, Serial, within};

const LOG_LINES: usize = 1000;

#[derive(Default)]
struct Inner {
    building: bool,
    log: Vec<String>,
    error: Option<String>,
}

/// Managed state: what the build is doing right now.
#[derive(Default)]
pub struct Base(Mutex<Inner>);

#[derive(Serialize, Deserialize)]
struct Meta {
    stamp: String,
    built_at: u64,
}

#[derive(Serialize)]
pub struct Status {
    /// `missing`, `building`, `ready` or `stale` (built from an older plain image or setup).
    state: &'static str,
    built_at: Option<u64>,
    size: Option<u64>,
    error: Option<String>,
    log: Vec<String>,
}

fn arch() -> Result<&'static str, String> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("x86_64"),
        "aarch64" => Ok("aarch64"),
        other => Err(format!("unsupported architecture {other}")),
    }
}

fn plain_image(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().resource_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("images").join(format!("alpine-{}.qcow2", arch()?)))
}

fn setup_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().resource_dir().map_err(|e| e.to_string())?.join("agentd"))
}

fn out_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("images"))
}

const SETUP_FILES: [&str; 3] = ["setup.sh", "agentd.sh", "agentd.initd"];

/// Identifies what a base image is made of: the setup files and the plain image's size.
fn stamp(app: &AppHandle) -> Result<String, String> {
    let dir = setup_dir(app)?;
    let mut h = Sha256::new();
    for f in SETUP_FILES {
        h.update(std::fs::read(dir.join(f)).map_err(|e| format!("{f}: {e}"))?);
    }
    let plain = std::fs::metadata(plain_image(app)?).map_err(|e| format!("plain image: {e}"))?;
    h.update(plain.len().to_le_bytes());
    Ok(h.finalize()[..12].iter().map(|b| format!("{b:02x}")).collect())
}

/// The built base image, if there is one.
#[allow(dead_code)] // used by the run engine
pub fn path(app: &AppHandle) -> Option<PathBuf> {
    let p = out_dir(app).ok()?.join("base.qcow2");
    p.is_file().then_some(p)
}

fn status(app: &AppHandle) -> Status {
    let inner = app.state::<Base>();
    let g = inner.0.lock().unwrap();
    let (mut state, mut built_at, mut size) = ("missing", None, None);
    if let Ok(dir) = out_dir(app)
        && let Ok(meta) = std::fs::read_to_string(dir.join("base.json")).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str::<Meta>(&t).map_err(|e| e.to_string()))
        && dir.join("base.qcow2").is_file()
    {
        built_at = Some(meta.built_at);
        size = std::fs::metadata(dir.join("base.qcow2")).ok().map(|m| m.len());
        state = if stamp(app).is_ok_and(|s| s == meta.stamp) { "ready" } else { "stale" };
    }
    if g.building {
        state = "building";
    }
    Status { state, built_at, size, error: g.error.clone(), log: g.log.clone() }
}

#[tauri::command]
pub fn base_status(app: AppHandle) -> Status {
    status(&app)
}

/// Starts a build in the background; progress is in `base_status`.
#[tauri::command]
pub fn base_build(app: AppHandle) -> Result<(), String> {
    start(&app)
}

/// Builds the base image if it is missing or stale (run at startup).
pub fn ensure(app: &AppHandle) {
    if matches!(status(app).state, "missing" | "stale") {
        let _ = start(app);
    }
}

fn start(app: &AppHandle) -> Result<(), String> {
    {
        let base = app.state::<Base>();
        let mut g = base.0.lock().unwrap();
        if g.building {
            return Err("a build is already running".into());
        }
        g.building = true;
        g.log.clear();
        g.error = None;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = build(&app).await;
        let base = app.state::<Base>();
        let mut g = base.0.lock().unwrap();
        g.building = false;
        if let Err(e) = result {
            g.log.push(format!("build failed: {e}"));
            g.error = Some(e);
        }
    });
    Ok(())
}

fn log(app: &AppHandle, line: &str) {
    let base = app.state::<Base>();
    let mut g = base.0.lock().unwrap();
    g.log.push(line.to_string());
    if g.log.len() > LOG_LINES {
        g.log.remove(0);
    }
}

async fn build(app: &AppHandle) -> Result<(), String> {
    let stamp = stamp(app)?;
    let plain = plain_image(app)?;
    let out = out_dir(app)?;
    let work = out.join("build");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = build_in(app, &plain, &out, &work, &stamp).await;
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Boots a builder VM from an overlay of `plain`, runs the setup in it and flattens the disk to
/// `out/base.qcow2`.
async fn build_in(app: &AppHandle, plain: &Path, out: &Path, work: &Path, stamp: &str) -> Result<(), String> {
    let overlay = work.join("disk.qcow2");
    log(app, "creating the builder disk");
    qemu_img(app, &["create", "-q", "-f", "qcow2", "-b", plain.to_str().unwrap(), "-F", "qcow2", overlay.to_str().unwrap()]).await?;

    log(app, "starting the builder VM");
    let serial = Serial::new(work);
    let mut vm = launch(app, &overlay, &serial)?;
    let mut con = Console::new(serial.connect().await?);
    within(240, "waiting for the VM to boot", con.login()).await?;

    let dir = setup_dir(app)?;
    con.run("mkdir -p /tmp/setup", |_| {}).await?;
    for f in SETUP_FILES {
        let bytes = std::fs::read(dir.join(f)).map_err(|e| format!("{f}: {e}"))?;
        con.put(&format!("/tmp/setup/{f}"), &bytes).await?;
    }
    log(app, "installing (this downloads Claude Code)");
    let code = within(1500, "the setup script", con.run("sh /tmp/setup/setup.sh 2>&1", |l| log(app, l))).await?;
    if code != 0 {
        return Err(format!("the setup script failed (exit {code}), see the log"));
    }

    log(app, "shutting the builder VM down");
    con.send("poweroff\n").await?;
    within(120, "waiting for the VM to stop", async { vm.wait().await.map(|_| ()).map_err(|e| e.to_string()) }).await?;

    log(app, "compressing the image");
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let tmp = out.join("base.qcow2.tmp");
    qemu_img(app, &["convert", "-c", "-O", "qcow2", overlay.to_str().unwrap(), tmp.to_str().unwrap()]).await?;
    std::fs::rename(&tmp, out.join("base.qcow2")).map_err(|e| e.to_string())?;
    let built_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let meta = serde_json::to_string(&Meta { stamp: stamp.to_string(), built_at }).unwrap();
    std::fs::write(out.join("base.json"), meta).map_err(|e| e.to_string())?;
    log(app, "done");
    Ok(())
}

fn launch(app: &AppHandle, disk: &Path, serial: &Serial) -> Result<tokio::process::Child, String> {
    let arch = arch()?;
    let mut cmd = crate::qemu::command(app, &format!("qemu-system-{arch}"));
    let accel = if cfg!(target_os = "linux") {
        "kvm"
    } else if cfg!(target_os = "macos") {
        "hvf"
    } else {
        "whpx"
    };
    cmd.args(["-accel", accel, "-accel", "tcg", "-cpu", "max", "-m", "1024", "-smp", "2"]);
    if arch == "aarch64" {
        let fw = app.path().resource_dir().map_err(|e| e.to_string())?.join("qemu/share/qemu/edk2-aarch64-code.fd");
        cmd.args(["-M", "virt", "-bios"]).arg(fw);
    }
    cmd.args(["-display", "none", "-monitor", "none", "-no-reboot", "-nic", "user"])
        .arg("-drive")
        .arg(format!("file={},if=virtio,discard=unmap", disk.display()))
        .arg("-serial")
        .arg(&serial.qemu_arg);
    let mut cmd = Command::from(cmd);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    cmd.spawn().map_err(|e| format!("cannot start QEMU: {e}"))
}

async fn qemu_img(app: &AppHandle, args: &[&str]) -> Result<(), String> {
    let mut cmd = Command::from(crate::qemu::command(app, "qemu-img"));
    let out = tokio::time::timeout(Duration::from_secs(600), cmd.args(args).output())
        .await
        .map_err(|_| "qemu-img timed out".to_string())?
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("qemu-img: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}
