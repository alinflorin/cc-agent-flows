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
fn sh_quote(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\\''"))
}

fn env_lines(env: &[EnvVar]) -> String {
    env.iter().map(|v| format!("export {}={}\n", v.name, sh_quote(&v.value))).collect()
}

/// The shell script that writes an image's docs file under `dir`: text as is, a command as
/// `$ command` and its output (stdout and stderr, a failing command does not stop the build) in a
/// code block. The values are quoted, nothing in them is interpreted by this script itself.
fn docs_script(dir: &str, id: &str, docs: &[Doc]) -> String {
    let mut out = format!("d={}\nmkdir -p \"$d\"\nn=$(ls \"$d\" | wc -l)\nf=\"$d/$(printf %03d $((n + 1)))-{id}.md\"\n: >\"$f\"\n", sh_quote(dir));
    for doc in docs {
        match doc.kind {
            DocKind::Text => out += &format!("printf '%s\\n\\n' {} >>\"$f\"\n", sh_quote(doc.value.trim())),
            DocKind::Command => {
                let cmd = doc.value.trim();
                out += &format!(
                    "{{ printf '```\\n$ %s\\n' {q}; NO_COLOR=1 sh -c {q} </dev/null 2>&1 || true; printf '```\\n\\n'; }} >>\"$f\"\n",
                    q = sh_quote(cmd)
                );
            }
        }
    }
    out
}

/// Where the docs of an image's tools are written inside it, one `NNN-<image>.md` per image
/// (numbered in build order, so a parent's come first); `agentd` appends them to the system prompt.
const DOCS_DIR: &str = "/usr/share/ccaf/tools";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DocKind {
    /// Written as is: a title, a few lines of advice.
    Text,
    /// Run in the builder VM after the script; what it prints (`jira --help`) is written.
    Command,
}

/// One piece of the system prompt appendix about the image's command-line tools.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Doc {
    pub kind: DocKind,
    pub value: String,
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
    /// The appendix to the system prompt of agents running on this image (and on images built on it).
    #[serde(default)]
    pub docs: Vec<Doc>,
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
        if self.docs.len() > 100 || self.docs.iter().map(|d| d.value.len()).sum::<usize>() > 64 * 1024 {
            return Err("too many documentation entries".into());
        }
        if self.docs.iter().any(|d| d.value.trim().is_empty()) {
            return Err("a documentation entry is empty".into());
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
    /// Images waiting for their turn: the ones built on an image that was just rebuilt.
    queue: Vec<String>,
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
    pub id: String,
    /// The built-in image: not editable or deletable.
    builtin: bool,
    name: String,
    description: String,
    script: String,
    docs: Vec<Doc>,
    env: Vec<EnvVar>,
    /// `missing`, `queued`, `building`, `ready` or `stale`.
    pub state: &'static str,
    /// Whether it can be built now (its parent must be built).
    can_build: bool,
    /// What it is built on (`base` for the base's own plain image).
    parent: Option<String>,
    built_at: Option<u64>,
    size: Option<u64>,
    pub error: Option<String>,
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

pub fn valid_id(id: &str) -> bool {
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
            if !spec.docs.is_empty() {
                // only when there are some, so images without docs keep their stamp
                h.update([0]);
                h.update(docs_script(DOCS_DIR, "", &spec.docs).as_bytes());
            }
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

pub fn read_specs(app: &AppHandle) -> Vec<(String, Spec)> {
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
    } else if g.queue.iter().any(|q| q == id) {
        state = "queued";
    }
    let (name, description, script, docs, env) = match spec {
        Some(s) => (s.name.clone(), s.description.clone(), s.script.clone(), s.docs.clone(), s.env.clone()),
        None => ("Base".into(), "Alpine with agentd, Claude Code and ripgrep".into(), String::new(), vec![], vec![]),
    };
    Info {
        can_build: spec.is_none_or(|s| path(app, &s.parent).is_some()),
        parent: spec.map(|s| s.parent.clone()),
        id: id.to_string(),
        builtin: spec.is_none(),
        name,
        description,
        script,
        docs,
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
    save_spec(&app, id, spec, true)
}

/// Writes a custom image's definition (checked); with `build` its build starts at once. If it
/// cannot start (the parent is not built, another build is running) it stays missing or out of
/// date, with its build button.
pub fn save_spec(app: &AppHandle, id: Option<String>, spec: Spec, build: bool) -> Result<String, String> {
    spec.validate()?;
    let dir = spec_dir(app)?;
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
    let mut specs = read_specs(app);
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
    if build {
        let _ = start(app, &id);
    }
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
    {
        let images = app.state::<Images>();
        let mut g = images.0.lock().unwrap();
        if g.building.as_deref() == Some(id.as_str()) {
            return Err("it is being built".into());
        }
        g.queue.retain(|q| *q != id);
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
pub fn path(app: &AppHandle, id: &str) -> Option<PathBuf> {
    disk(app, id).ok().filter(|p| p.is_file())
}

/// Builds the base image if it is missing or stale, and otherwise the custom images that are out
/// of date (run at startup: this is how an app update, or an edit that was never built, reaches
/// the images). When the base is rebuilt, the images built on it follow in the queue.
pub fn ensure_base(app: &AppHandle) {
    if matches!(info(app, BASE, None).state, "missing" | "stale") {
        let _ = start(app, BASE);
        return;
    }
    let specs = read_specs(app);
    // Only those whose parent is up to date start now; the others follow their parent's build.
    let ready = |id: &str| id == BASE || info(app, id, specs.iter().find(|(i, _)| i == id).map(|(_, s)| s)).state == "ready";
    let mut due: Vec<String> = specs.iter().filter(|(id, s)| info(app, id, Some(s)).state == "stale" && ready(&s.parent)).map(|(id, _)| id.clone()).collect();
    if due.is_empty() {
        return;
    }
    let first = due.remove(0);
    app.state::<Images>().0.lock().unwrap().queue.extend(due);
    let _ = start(app, &first);
}

/// The images offered on first start (`developer`, `qa`): id and spec, scripts in `default-images/`.
fn defaults() -> Vec<(&'static str, Spec)> {
    let text = |t: &str| Doc { kind: DocKind::Text, value: t.into() };
    let cmd = |c: &str| Doc { kind: DocKind::Command, value: c.into() };
    let spec = |name: &str, description: &str, parent: &str, env: &[(&str, &str)], script: &str, docs: Vec<Doc>| Spec {
        name: name.into(),
        description: description.into(),
        parent: parent.into(),
        env: env.iter().map(|(n, v)| EnvVar { name: n.to_string(), value: v.to_string() }).collect(),
        script: script.into(),
        docs,
    };
    vec![
        (
            "manager",
            spec(
                "Manager",
                "The Jira CLI (jtk) on the base image, for agents that plan and coordinate; the other starter images build on it.",
                BASE,
                &[],
                include_str!("../default-images/manager.sh"),
                vec![
                    text("## Jira\n\nThe `jtk` CLI is installed and authenticated through the environment; never log in or print the token. If a command fails because of access or configuration, report it instead of working around it."),
                    cmd("jtk --help"),
                    cmd("jtk issues --help"),
                    cmd("jtk issues list --help"),
                    cmd("jtk issues get --help"),
                    cmd("jtk issues search --help"),
                    cmd("jtk issues create --help"),
                    cmd("jtk transitions do --help"),
                    cmd("jtk comments add --help"),
                ],
            ),
        ),
        (
            "developer",
            spec(
                "Developer",
                "Git, GitHub CLI, Python and Node.js, for agents that write code.",
                "manager",
                &[],
                include_str!("../default-images/developer.sh"),
                vec![],
            ),
        ),
        (
            "qa",
            spec(
                "Manual QA",
                "Headless Chromium and agent-browser, for agents that test an app by hand in a browser.",
                "manager",
                &[("AGENT_BROWSER_EXECUTABLE_PATH", "/usr/bin/chromium"), ("AGENT_BROWSER_ARGS", "--no-sandbox,--disable-dev-shm-usage")],
                include_str!("../default-images/qa.sh"),
                vec![
                    text("## Browser\n\n`agent-browser` drives a headless Chromium; a background daemon keeps the page between commands. Take a new `snapshot -i` after every page change, element refs (`@e3`) go stale."),
                    cmd("agent-browser --help"),
                ],
            ),
        ),
    ]
}

/// Creates the default images (the ones that do not exist yet) and builds them: now if the base is
/// built, else after it (while the base builds they follow it as the images built on it).
///
/// `reset`: the starter images are written again over what is there (the edits of the user are lost, other images are left alone) and the ones that are then missing or out of date are built. Refused while a build runs.
pub fn provision_defaults(app: &AppHandle, reset: bool) -> Result<(), String> {
    let dir = spec_dir(app)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if reset {
        let images = app.state::<Images>();
        let g = images.0.lock().unwrap();
        if g.building.is_some() || !g.queue.is_empty() {
            return Err("an image is being built, wait for it".into());
        }
    }
    let mut created = vec![];
    for (id, spec) in defaults() {
        let file = dir.join(format!("{id}.json"));
        if reset || !file.exists() {
            let text = serde_json::to_string_pretty(&spec).map_err(|e| e.to_string())?;
            std::fs::write(&file, text).map_err(|e| e.to_string())?;
            if !reset || info(app, id, Some(&spec)).state != "ready" {
                created.push(id.to_string());
            }
        }
    }
    if info(app, BASE, None).state == "ready"
        && let Some((first, rest)) = created.split_first()
    {
        app.state::<Images>().0.lock().unwrap().queue.extend(rest.iter().cloned());
        let _ = start(app, first);
    }
    Ok(())
}

/// Settings > Images > Reset to demo content.
#[tauri::command]
pub fn images_reset_demo(app: AppHandle) -> Result<(), String> {
    provision_defaults(&app, true)
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
        {
            let images = app.state::<Images>();
            let mut g = images.0.lock().unwrap();
            g.building = None;
            match result {
                Ok(()) => {
                    // A rebuilt image changes everything built on it: rebuild those next, and
                    // theirs after each of them (a chain reaction, one build at a time).
                    for (child, _) in read_specs(&app).into_iter().filter(|(_, s)| s.parent == id) {
                        if !g.queue.contains(&child) {
                            g.queue.push(child);
                        }
                    }
                }
                Err(e) => {
                    g.logs.entry(id.clone()).or_default().push(format!("build failed: {e}"));
                    g.errors.insert(id.clone(), e);
                }
            }
        }
        loop {
            let next = {
                let images = app.state::<Images>();
                let mut g = images.0.lock().unwrap();
                if g.queue.is_empty() { None } else { Some(g.queue.remove(0)) }
            };
            let Some(n) = next else { break };
            if start(&app, &n).is_ok() {
                break;
            }
            // Busy (someone started a build meanwhile): that build will drain the queue when it
            // ends. Otherwise the image is gone, so skip it.
            let images = app.state::<Images>();
            let mut g = images.0.lock().unwrap();
            if g.building.is_some() {
                g.queue.insert(0, n);
                break;
            }
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
    if let Some(spec) = spec.filter(|s| !s.docs.is_empty()) {
        con.put("/tmp/setup/docs.sh", docs_script(DOCS_DIR, id, &spec.docs).as_bytes()).await?;
        say("writing the tool docs");
        let code = within(300, "the tool docs", con.run("sh /tmp/setup/docs.sh", |l| say(l))).await?;
        if code != 0 {
            return Err(format!("writing the tool docs failed (exit {code})"));
        }
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

/// The host's logical cores: "no CPU limit" for a VM.
fn host_cpus() -> u32 {
    std::thread::available_parallelism().map_or(2, |n| n.get() as u32)
}

/// A QEMU command for a headless guest of this machine's architecture: hardware acceleration with
/// a software fallback, `-cpu max` (Claude Code needs SSE4.2), no reboot. The caller adds the
/// network card (`-nic`): open for builders, sealed for agents.
/// `cpus` is the number of vCPUs (`None`: all of the host's). The caller adds the disk and the channels.
pub fn machine(app: &AppHandle, mem_mb: u32, cpus: Option<u32>) -> Result<std::process::Command, String> {
    let arch = arch()?;
    let mut cmd = crate::qemu::command(app, &format!("qemu-system-{arch}"));
    let accel = if cfg!(target_os = "linux") {
        "kvm"
    } else if cfg!(target_os = "macos") {
        "hvf"
    } else {
        "whpx"
    };
    cmd.args(["-accel", accel, "-accel", "tcg", "-cpu", "max", "-m", &mem_mb.to_string(), "-smp", &cpus.unwrap_or_else(host_cpus).to_string()]);
    if arch == "aarch64" {
        let fw = resources(app)?.join("qemu/share/qemu/edk2-aarch64-code.fd");
        cmd.args(["-M", "virt", "-bios"]).arg(fw);
    }
    cmd.args(["-display", "none", "-monitor", "none", "-no-reboot"]);
    Ok(cmd)
}

fn launch(app: &AppHandle, disk: &Path, serial: &Serial) -> Result<tokio::process::Child, String> {
    let mut cmd = machine(app, 1024, Some(2))?;
    // builders need the internet (package managers, installers)
    cmd.args(["-nic", "user"])
        .arg("-drive")
        .arg(format!("file={},if=virtio,discard=unmap", disk.display()))
        .arg("-serial")
        .arg(&serial.qemu_arg);
    let mut cmd = Command::from(cmd);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    cmd.spawn().map_err(|e| format!("cannot start QEMU: {e}"))
}

pub async fn qemu_img(app: &AppHandle, args: &[&str]) -> Result<(), String> {
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
        Spec { name: "x".into(), description: String::new(), parent: parent.into(), env: vec![], script: String::new(), docs: vec![] }
    }

    #[test]
    fn env_lines_are_shell_quoted() {
        let env = vec![EnvVar { name: "A".into(), value: "it's a \"b\" $c".into() }];
        assert_eq!(env_lines(&env), "export A='it'\\''s a \"b\" $c'\n");
    }

    #[test]
    fn docs_script_writes_text_and_command_output() {
        let dir = std::env::temp_dir().join(format!("ccaf-docs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("001-parent.md"), "x").unwrap();
        let docs = vec![
            Doc { kind: DocKind::Text, value: "## Tool\n\nit's \"quoted\" $HOME".into() },
            Doc { kind: DocKind::Command, value: "echo it\\'s out; echo err >&2; exit 3".into() },
        ];
        let script = docs_script(dir.to_str().unwrap(), "mine", &docs);
        let out = std::process::Command::new("sh").arg("-c").arg(&script).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let text = std::fs::read_to_string(dir.join("002-mine.md")).unwrap();
        assert_eq!(text, "## Tool\n\nit's \"quoted\" $HOME\n\n```\n$ echo it\\'s out; echo err >&2; exit 3\nit's out\nerr\n```\n\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_images_are_valid() {
        for (id, spec) in defaults() {
            assert!(valid_id(id));
            assert!(spec.validate().is_ok(), "{id}");
        }
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
