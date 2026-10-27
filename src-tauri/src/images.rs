//! Images agents run on. The **base** image (plain Alpine + agentd, Claude Code and ripgrep, built
//! by `resources/agentd/setup.sh`) is built in and not editable. **Custom** images are made by the
//! user in Settings: a name and a shell script that runs as root in a builder VM on top of the
//! base image. Agent profiles pick an image (`profiles.rs`).
//!
//! Layout in `<app data>/images/`: `specs/<id>.json` (custom image definitions) and
//! `built/<id>.qcow2` + `built/<id>.json` (a standalone, flattened disk and its stamp). A stamp
//! covers what an image is made of (for custom ones: the script and the base's stamp), so an image
//! is "stale" when its source changed, e.g. after an app update that brings a new base.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};
use tokio::process::Command;

use crate::builder::{Console, Serial, script_cmd, within};

const LOG_LINES: usize = 1000;
pub const BASE: &str = "base";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Spec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Shell script (busybox ash) run as root in the builder VM, on top of the base image.
    #[serde(default)]
    pub script: String,
}

impl Spec {
    fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.name.len() > 80 {
            return Err("the name must be 1 to 80 characters".into());
        }
        if self.script.len() > 256 * 1024 {
            return Err("the script is too large".into());
        }
        Ok(())
    }
}

#[derive(Default)]
struct Inner {
    /// The image being built (one at a time).
    building: Option<String>,
    logs: HashMap<String, Vec<String>>,
    errors: HashMap<String, String>,
}

/// Managed state: what the builds are doing.
#[derive(Default)]
pub struct Images(Mutex<Inner>);

#[derive(Serialize, Deserialize)]
struct Meta {
    stamp: String,
    built_at: u64,
}

#[derive(Serialize)]
pub struct Info {
    id: String,
    /// The built-in image: not editable or deletable.
    builtin: bool,
    name: String,
    description: String,
    script: String,
    /// `missing`, `building`, `ready` or `stale`.
    state: &'static str,
    /// Whether it can be built now (a custom image needs the base to exist).
    can_build: bool,
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

fn resources(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().resource_dir().map_err(|e| e.to_string())
}

fn plain_image(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(resources(app)?.join("images").join(format!("alpine-{}.qcow2", arch()?)))
}

fn root(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("images"))
}

fn disk(app: &AppHandle, id: &str) -> Result<PathBuf, String> {
    Ok(root(app)?.join("built").join(format!("{id}.qcow2")))
}

fn meta_path(app: &AppHandle, id: &str) -> Result<PathBuf, String> {
    Ok(root(app)?.join("built").join(format!("{id}.json")))
}

fn spec_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(root(app)?.join("specs"))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id != BASE && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

const SETUP_FILES: [&str; 3] = ["setup.sh", "agentd.sh", "agentd.initd"];

/// What the base image is made of: the setup files and the plain image's size.
fn base_stamp(app: &AppHandle) -> Result<String, String> {
    let dir = resources(app)?.join("agentd");
    let mut h = Sha256::new();
    for f in SETUP_FILES {
        h.update(std::fs::read(dir.join(f)).map_err(|e| format!("{f}: {e}"))?);
    }
    let plain = std::fs::metadata(plain_image(app)?).map_err(|e| format!("plain image: {e}"))?;
    h.update(plain.len().to_le_bytes());
    Ok(hex(&h.finalize()))
}

/// The stamp an image should have given its sources now. For a custom image: its script on top
/// of the base's stamp as it was built (so a rebuilt base makes it stale).
fn wanted_stamp(app: &AppHandle, spec: Option<&Spec>) -> Result<String, String> {
    match spec {
        None => base_stamp(app),
        Some(spec) => {
            let base: Meta = read_meta(app, BASE).ok_or("the base image is not built")?;
            let mut h = Sha256::new();
            h.update(&base.stamp);
            h.update(spec.script.as_bytes());
            Ok(hex(&h.finalize()))
        }
    }
}

fn hex(d: &[u8]) -> String {
    d[..12].iter().map(|b| format!("{b:02x}")).collect()
}

fn read_meta(app: &AppHandle, id: &str) -> Option<Meta> {
    let text = std::fs::read_to_string(meta_path(app, id).ok()?).ok()?;
    serde_json::from_str(&text).ok()
}

fn slug(name: &str) -> String {
    crate::profiles::slug(name)
}

fn read_specs(app: &AppHandle) -> Vec<(String, Spec)> {
    let Ok(dir) = spec_dir(app) else { return vec![] };
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut v: Vec<_> = rd
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let id = p.file_stem()?.to_str()?.to_string();
            let spec: Spec = serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
            (p.extension()? == "json" && valid_id(&id) && spec.validate().is_ok()).then_some((id, spec))
        })
        .collect();
    v.sort_by_key(|(_, s)| s.name.to_lowercase());
    v
}

fn info(app: &AppHandle, id: &str, spec: Option<&Spec>) -> Info {
    let inner = app.state::<Images>();
    let g = inner.0.lock().unwrap();
    let (mut state, mut built_at, mut size) = ("missing", None, None);
    if let Some(meta) = read_meta(app, id)
        && let Ok(d) = disk(app, id)
        && d.is_file()
    {
        built_at = Some(meta.built_at);
        size = std::fs::metadata(&d).ok().map(|m| m.len());
        state = if wanted_stamp(app, spec).is_ok_and(|s| s == meta.stamp) { "ready" } else { "stale" };
    }
    if g.building.as_deref() == Some(id) {
        state = "building";
    }
    let (name, description, script) = match spec {
        Some(s) => (s.name.clone(), s.description.clone(), s.script.clone()),
        None => ("Base".into(), "Alpine with agentd, Claude Code and ripgrep".into(), String::new()),
    };
    Info {
        can_build: spec.is_none() || path(app, BASE).is_some(),
        id: id.to_string(),
        builtin: spec.is_none(),
        name,
        description,
        script,
        state,
        built_at,
        size,
        error: g.errors.get(id).cloned(),
        log: g.logs.get(id).cloned().unwrap_or_default(),
    }
}

#[tauri::command]
pub fn images_list(app: AppHandle) -> Vec<Info> {
    let mut v = vec![info(&app, BASE, None)];
    for (id, spec) in read_specs(&app) {
        v.push(info(&app, &id, Some(&spec)));
    }
    v
}

/// Creates (`id` null) or updates a custom image; returns its id. Editing the script makes the
/// built disk stale, it is not rebuilt until asked.
#[tauri::command]
pub fn images_save(app: AppHandle, id: Option<String>, spec: Spec) -> Result<String, String> {
    spec.validate()?;
    let dir = spec_dir(&app)?;
    let id = match id {
        Some(id) if valid_id(&id) => id,
        Some(_) => return Err("bad image id".into()),
        None => {
            let base = slug(&spec.name);
            if base.is_empty() || base == BASE {
                return Err("pick another name".into());
            }
            let (mut id, mut n) = (base.clone(), 2);
            while dir.join(format!("{id}.json")).exists() {
                id = format!("{base}-{n}");
                n += 1;
            }
            id
        }
    };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(&spec).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(format!("{id}.json")), text).map_err(|e| e.to_string())?;
    Ok(id)
}

#[tauri::command]
pub fn images_delete(app: AppHandle, id: String) -> Result<(), String> {
    if !valid_id(&id) {
        return Err("the base image cannot be deleted".into());
    }
    if crate::profiles::used_images(&app).contains(&id) {
        return Err("an agent profile uses this image".into());
    }
    if app.state::<Images>().0.lock().unwrap().building.as_deref() == Some(id.as_str()) {
        return Err("it is being built".into());
    }
    std::fs::remove_file(spec_dir(&app)?.join(format!("{id}.json"))).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(disk(&app, &id)?);
    let _ = std::fs::remove_file(meta_path(&app, &id)?);
    Ok(())
}

/// Starts a build in the background; progress is in `images_list`.
#[tauri::command]
pub fn images_build(app: AppHandle, id: String) -> Result<(), String> {
    if id != BASE && path(&app, BASE).is_none() {
        return Err("build the base image first".into());
    }
    start(&app, &id)
}

/// The built disk of an image, if there is one.
#[allow(dead_code)] // used by the run engine
pub fn path(app: &AppHandle, id: &str) -> Option<PathBuf> {
    disk(app, id).ok().filter(|p| p.is_file())
}

/// Builds the base image if it is missing or stale (run at startup).
pub fn ensure_base(app: &AppHandle) {
    if matches!(info(app, BASE, None).state, "missing" | "stale") {
        let _ = start(app, BASE);
    }
}

fn start(app: &AppHandle, id: &str) -> Result<(), String> {
    let spec = if id == BASE {
        None
    } else {
        let specs = read_specs(app);
        Some(specs.into_iter().find(|(i, _)| i == id).ok_or("no such image")?.1)
    };
    {
        let images = app.state::<Images>();
        let mut g = images.0.lock().unwrap();
        if let Some(b) = &g.building {
            return Err(format!("{b} is being built, wait for it"));
        }
        g.building = Some(id.to_string());
        g.logs.insert(id.to_string(), vec![]);
        g.errors.remove(id);
    }
    let (app, id) = (app.clone(), id.to_string());
    tauri::async_runtime::spawn(async move {
        let result = build(&app, &id, spec.as_ref()).await;
        let images = app.state::<Images>();
        let mut g = images.0.lock().unwrap();
        g.building = None;
        if let Err(e) = result {
            g.logs.entry(id.clone()).or_default().push(format!("build failed: {e}"));
            g.errors.insert(id, e);
        }
    });
    Ok(())
}

fn log(app: &AppHandle, id: &str, line: &str) {
    let images = app.state::<Images>();
    let mut g = images.0.lock().unwrap();
    let log = g.logs.entry(id.to_string()).or_default();
    log.push(line.to_string());
    if log.len() > LOG_LINES {
        log.remove(0);
    }
}

async fn build(app: &AppHandle, id: &str, spec: Option<&Spec>) -> Result<(), String> {
    let stamp = wanted_stamp(app, spec)?;
    // The base is built from the plain image, a custom image from the finished base.
    let source = match spec {
        None => plain_image(app)?,
        Some(_) => path(app, BASE).ok_or("build the base image first")?,
    };
    let out = root(app)?.join("built");
    let work = root(app)?.join("work");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = build_in(app, id, spec, &source, &out, &work, &stamp).await;
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Boots a builder VM from an overlay of `source`, runs the setup in it and flattens the disk to
/// `out/<id>.qcow2`.
async fn build_in(app: &AppHandle, id: &str, spec: Option<&Spec>, source: &Path, out: &Path, work: &Path, stamp: &str) -> Result<(), String> {
    let say = |l: &str| log(app, id, l);
    let overlay = work.join("disk.qcow2");
    say("creating the builder disk");
    qemu_img(app, &["create", "-q", "-f", "qcow2", "-b", source.to_str().unwrap(), "-F", "qcow2", overlay.to_str().unwrap()]).await?;

    say("starting the builder VM");
    let serial = Serial::new(work);
    let mut vm = launch(app, &overlay, &serial)?;
    let mut con = Console::new(serial.connect().await?);
    within(240, "waiting for the VM to boot", con.login()).await?;

    con.run("mkdir -p /tmp/setup", |_| {}).await?;
    let script = match spec {
        None => {
            let dir = resources(app)?.join("agentd");
            for f in SETUP_FILES {
                let bytes = std::fs::read(dir.join(f)).map_err(|e| format!("{f}: {e}"))?;
                con.put(&format!("/tmp/setup/{f}"), &bytes).await?;
            }
            say("installing (this downloads Claude Code)");
            "/tmp/setup/setup.sh"
        }
        Some(spec) => {
            con.put("/tmp/setup/image.sh", spec.script.as_bytes()).await?;
            say("running the image script");
            "/tmp/setup/image.sh"
        }
    };
    let code = within(1500, "the setup script", con.run(&script_cmd(script), |l| say(l))).await?;
    if code != 0 {
        return Err(format!("the script failed (exit {code}), see the log"));
    }
    // Same tidy-up for every image: leave nothing behind and give back the free space.
    con.run("rm -rf /tmp/setup; apk cache clean >/dev/null 2>&1; fstrim / ; sync", |_| {}).await?;

    say("shutting the builder VM down");
    con.send("poweroff\n").await?;
    within(120, "waiting for the VM to stop", async { vm.wait().await.map(|_| ()).map_err(|e| e.to_string()) }).await?;

    say("compressing the image");
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let tmp = out.join(format!("{id}.qcow2.tmp"));
    qemu_img(app, &["convert", "-c", "-O", "qcow2", overlay.to_str().unwrap(), tmp.to_str().unwrap()]).await?;
    std::fs::rename(&tmp, out.join(format!("{id}.qcow2"))).map_err(|e| e.to_string())?;
    let built_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let meta = serde_json::to_string(&Meta { stamp: stamp.to_string(), built_at }).unwrap();
    std::fs::write(out.join(format!("{id}.json")), meta).map_err(|e| e.to_string())?;
    say("done");
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
        let fw = resources(app)?.join("qemu/share/qemu/edk2-aarch64-code.fd");
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
