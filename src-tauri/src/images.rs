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

/// A variable baked into the image.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

/// `export` lines for `/etc/conf.d/agentd`: OpenRC sources that file when it starts the service,
/// so agentd and the `claude` it runs inherit the variables. Appended to what the parent image
/// has, so a later definition wins.
fn env_lines(env: &[EnvVar]) -> String {
    env.iter().map(|v| format!("export {}='{}'\n", v.name, v.value.replace('\'', "'\\''"))).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Spec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The image this one is built on top of: `base` or another custom image.
    #[serde(default = "base_id")]
    pub parent: String,
    /// Environment variables baked into the image, on top of the parent's.
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// Shell script (busybox ash) run as root in the builder VM, on top of the parent.
    #[serde(default)]
    pub script: String,
}

fn base_id() -> String {
    BASE.into()
}

/// Whether making `parent` the parent of `id` would loop (or the chain is absurdly long).
fn creates_cycle(specs: &[(String, Spec)], id: &str, parent: &str) -> bool {
    let mut cur = parent;
    for _ in 0..=specs.len() {
        if cur == BASE {
            return false;
        }
        if cur == id {
            return true;
        }
        match specs.iter().find(|(i, _)| i == cur) {
            Some((_, s)) => cur = &s.parent,
            None => return false,
        }
    }
    true
}

impl Spec {
    fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.name.len() > 80 {
            return Err("the name must be 1 to 80 characters".into());
        }
        if self.script.len() > 256 * 1024 {
            return Err("the script is too large".into());
        }
        crate::profiles::validate_env(self.env.iter().map(|v| (v.name.as_str(), Some(v.value.as_str()))))?;
        if self.parent.is_empty() {
            return Err("pick the image to build on".into());
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
    env: Vec<EnvVar>,
    /// `missing`, `building`, `ready` or `stale`.
    state: &'static str,
    /// Whether it can be built now (its parent must be built).
    can_build: bool,
    /// What it is built on (`base` for the base's own plain image).
    parent: Option<String>,
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
/// of its parent's stamp as it was built (so a rebuilt parent makes it stale, down the chain).
fn wanted_stamp(app: &AppHandle, spec: Option<&Spec>) -> Result<String, String> {
    match spec {
        None => base_stamp(app),
        Some(spec) => {
            let parent: Meta = read_meta(app, &spec.parent).ok_or("the parent image is not built")?;
            let mut h = Sha256::new();
            h.update(&parent.stamp);
            h.update(env_lines(&spec.env).as_bytes());
            h.update([0]);
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
    let (name, description, script, env) = match spec {
        Some(s) => (s.name.clone(), s.description.clone(), s.script.clone(), s.env.clone()),
        None => ("Base".into(), "Alpine with agentd, Claude Code and ripgrep".into(), String::new(), vec![]),
    };
    Info {
        can_build: spec.is_none_or(|s| path(app, &s.parent).is_some()),
        parent: spec.map(|s| s.parent.clone()),
        id: id.to_string(),
        builtin: spec.is_none(),
        name,
        description,
        script,
        env,
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
    let mut specs = read_specs(&app);
    if spec.parent != BASE && !specs.iter().any(|(i, _)| *i == spec.parent) {
        return Err("the image to build on does not exist".into());
    }
    specs.retain(|(i, _)| *i != id);
    if creates_cycle(&specs, &id, &spec.parent) {
        return Err("an image cannot be built on itself or on one built on it".into());
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(&spec).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(format!("{id}.json")), text).map_err(|e| e.to_string())?;
    // Build it now. If it cannot start (the parent is not built, another build is running) it
    // stays missing or out of date, with its build button.
    let _ = start(&app, &id);
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
    if let Some((child, _)) = read_specs(&app).into_iter().find(|(_, s)| s.parent == id) {
        return Err(format!("the image {child} is built on it"));
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
    // The base is built from the plain image, a custom image from its finished parent.
    let source = match spec {
        None => plain_image(app)?,
        Some(s) => path(app, &s.parent).ok_or_else(|| format!("build {} first", s.parent))?,
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
            if !spec.env.is_empty() {
                con.put("/tmp/setup/env.sh", env_lines(&spec.env).as_bytes()).await?;
                con.run("cat /tmp/setup/env.sh >>/etc/conf.d/agentd", |_| {}).await?;
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(parent: &str) -> Spec {
        Spec { name: "x".into(), description: String::new(), parent: parent.into(), env: vec![], script: String::new() }
    }

    #[test]
    fn env_lines_are_shell_quoted() {
        let env = vec![EnvVar { name: "A".into(), value: "it's a \"b\" $c".into() }];
        assert_eq!(env_lines(&env), "export A='it'\\''s a \"b\" $c'\n");
    }

    #[test]
    fn parents_cannot_loop() {
        let specs = vec![("a".to_string(), spec("base")), ("b".to_string(), spec("a")), ("c".to_string(), spec("b"))];
        assert!(!creates_cycle(&specs, "d", "c"), "a new image on the end of a chain");
        assert!(!creates_cycle(&specs, "c", "a"), "moving down the chain");
        assert!(creates_cycle(&specs, "a", "c"), "a on top of its own descendant");
        assert!(creates_cycle(&specs, "a", "a"), "on itself");
        assert!(!creates_cycle(&specs, "a", "base"));
    }
}
