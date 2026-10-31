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

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};
use tokio::process::Command;

use crate::config::profiles::EnvVar;
use crate::config::scope::Scope;
use crate::vm::builder::{Console, Serial, script_cmd, within};
use crate::vm::machine::{arch, drive_arg, machine, qemu_img, resources};

const LOG_LINES: usize = 1000;
pub const BASE: &str = "base";

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
        crate::config::profiles::validate_env(self.env.iter().map(|v| (v.name.as_str(), Some(v.value.as_str()))))?;
        if self.parent.is_empty() {
            return Err("pick the image to build on".into());
        }
        Ok(())
    }
}

/// An image: the id and where it lives. Global images are `base` and the ones made in Settings;
/// a project's are made in the project. A project's image can be built on a global one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ref {
    pub scope: Scope,
    pub id: String,
}

impl Ref {
    fn new(scope: &Scope, id: &str) -> Ref {
        Ref { scope: scope.clone(), id: id.to_string() }
    }

    fn base() -> Ref {
        Ref::new(&Scope::global(), BASE)
    }

    /// Unique over all scopes: the id, or `<project>/<id>`.
    fn key(&self) -> String {
        match self.scope.id() {
            None => self.id.clone(),
            Some(p) => format!("{p}/{}", self.id),
        }
    }
}

#[derive(Default)]
struct Inner {
    /// The image being built (one at a time).
    building: Option<Ref>,
    /// Images waiting for their turn: the ones built on an image that was just rebuilt.
    queue: Vec<Ref>,
    /// By [`Ref::key`].
    logs: HashMap<String, Vec<String>>,
    errors: HashMap<String, String>,
}

/// Managed state: what the builds are doing.
#[derive(Default)]
pub struct Images(Mutex<Inner>);

impl Images {
    /// Whether an image of the project is being built or waits to be.
    pub fn busy_in(&self, project: &str) -> bool {
        let g = self.0.lock().unwrap();
        g.building.iter().chain(g.queue.iter()).any(|r| r.scope.id() == Some(project))
    }
}

#[derive(Serialize, Deserialize)]
struct Meta {
    stamp: String,
    built_at: u64,
}

#[derive(Serialize)]
pub struct Info {
    pub id: String,
    /// The project it belongs to; `None` for a global image.
    scope: Option<String>,
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

fn plain_image(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(resources(app)?.join("images").join(format!("alpine-{}.qcow2", arch()?)))
}

fn root(app: &AppHandle, scope: &Scope) -> Result<PathBuf, String> {
    Ok(scope.dir(app)?.join("images"))
}

fn disk(app: &AppHandle, r: &Ref) -> Result<PathBuf, String> {
    Ok(root(app, &r.scope)?.join("built").join(format!("{}.qcow2", r.id)))
}

fn meta_path(app: &AppHandle, r: &Ref) -> Result<PathBuf, String> {
    Ok(root(app, &r.scope)?.join("built").join(format!("{}.json", r.id)))
}

fn spec_dir(app: &AppHandle, scope: &Scope) -> Result<PathBuf, String> {
    Ok(root(app, scope)?.join("specs"))
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id != BASE && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

const SETUP_FILES: [&str; 4] = ["setup.sh", "agentd.sh", "agentd.initd", "ccaf-init.sh"];

/// What the base image is made of: the setup files and the sizes of the plain image and kernel.
fn base_stamp(app: &AppHandle) -> Result<String, String> {
    let dir = resources(app)?.join("agentd");
    let mut h = Sha256::new();
    for f in SETUP_FILES {
        h.update(std::fs::read(dir.join(f)).map_err(|e| format!("{f}: {e}"))?);
    }
    let plain = std::fs::metadata(plain_image(app)?).map_err(|e| format!("plain image: {e}"))?;
    h.update(plain.len().to_le_bytes());
    // the kernel the image boots
    let f = plain_image(app)?.with_extension("vmlinuz");
    h.update(std::fs::metadata(&f).map_err(|e| format!("{}: {e}", f.display()))?.len().to_le_bytes());
    Ok(hex(&h.finalize()))
}

/// The stamp an image should have given its sources now. For a custom image: its script on top
/// of its parent's stamp as it was built (so a rebuilt parent makes it stale, down the chain).
fn wanted_stamp(app: &AppHandle, r: &Ref, spec: Option<&Spec>) -> Result<String, String> {
    match spec {
        None => base_stamp(app),
        Some(spec) => {
            let parent = resolve(app, &r.scope, &spec.parent).ok_or("the parent image does not exist")?;
            let parent: Meta = read_meta(app, &parent).ok_or("the parent image is not built")?;
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

fn read_meta(app: &AppHandle, r: &Ref) -> Option<Meta> {
    let text = std::fs::read_to_string(meta_path(app, r).ok()?).ok()?;
    serde_json::from_str(&text).ok()
}

/// The custom images of one scope.
pub fn read_specs(app: &AppHandle, scope: &Scope) -> Vec<(String, Spec)> {
    let Ok(dir) = spec_dir(app, scope) else { return vec![] };
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

/// The custom images of every scope: the global ones and each project's.
fn all_specs(app: &AppHandle) -> Vec<(Ref, Spec)> {
    let mut scopes = vec![Scope::global()];
    scopes.extend(crate::config::projects::ids(app).iter().map(|p| Scope::project(p)));
    scopes.iter().flat_map(|sc| read_specs(app, sc).into_iter().map(|(id, s)| (Ref::new(sc, &id), s))).collect()
}

/// The image an id means to something in `scope`: the project's own, else the global one (`base`
/// included). A project's image shadows a global one with the same id.
pub fn resolve(app: &AppHandle, scope: &Scope, id: &str) -> Option<Ref> {
    if id == BASE {
        return Some(Ref::base());
    }
    if !scope.is_global() && read_specs(app, scope).iter().any(|(i, _)| i == id) {
        return Some(Ref::new(scope, id));
    }
    read_specs(app, &Scope::global()).iter().any(|(i, _)| i == id).then(|| Ref::new(&Scope::global(), id))
}

fn spec_of(app: &AppHandle, r: &Ref) -> Option<Spec> {
    read_specs(app, &r.scope).into_iter().find(|(i, _)| *i == r.id).map(|(_, s)| s)
}

/// The image a custom image is built on.
fn parent_of(app: &AppHandle, r: &Ref, spec: &Spec) -> Option<Ref> {
    resolve(app, &r.scope, &spec.parent)
}

fn info(app: &AppHandle, r: &Ref, spec: Option<&Spec>) -> Info {
    let inner = app.state::<Images>();
    let g = inner.0.lock().unwrap();
    let (mut state, mut built_at, mut size) = ("missing", None, None);
    if let Some(meta) = read_meta(app, r)
        && let Ok(d) = disk(app, r)
        && d.is_file()
    {
        built_at = Some(meta.built_at);
        size = std::fs::metadata(&d).ok().map(|m| m.len());
        state = if wanted_stamp(app, r, spec).is_ok_and(|s| s == meta.stamp) { "ready" } else { "stale" };
    }
    if g.building.as_ref() == Some(r) {
        state = "building";
    } else if g.queue.contains(r) {
        state = "queued";
    }
    let (name, description, script, docs, env) = match spec {
        Some(s) => (s.name.clone(), s.description.clone(), s.script.clone(), s.docs.clone(), s.env.clone()),
        None => ("Base".into(), "Alpine with agentd, Claude Code and ripgrep".into(), String::new(), vec![], vec![]),
    };
    let key = r.key();
    Info {
        can_build: spec.is_none_or(|s| parent_of(app, r, s).is_some_and(|p| built(app, &p).is_some())),
        parent: spec.map(|s| s.parent.clone()),
        id: r.id.clone(),
        scope: r.scope.id().map(str::to_string),
        builtin: spec.is_none(),
        name,
        description,
        script,
        docs,
        env,
        state,
        built_at,
        size,
        error: g.errors.get(&key).cloned(),
        log: g.logs.get(&key).cloned().unwrap_or_default(),
    }
}

/// The global images (the base first), or those of a project followed by the global ones it can use.
#[tauri::command]
pub fn images_list(app: AppHandle, project: Option<String>) -> Result<Vec<Info>, String> {
    let scope = Scope::of(project)?;
    let mut v = vec![];
    if !scope.is_global() {
        scope.dir(&app)?;
        for (id, spec) in read_specs(&app, &scope) {
            v.push(info(&app, &Ref::new(&scope, &id), Some(&spec)));
        }
    }
    v.push(info(&app, &Ref::base(), None));
    for (id, spec) in read_specs(&app, &Scope::global()) {
        v.push(info(&app, &Ref::new(&Scope::global(), &id), Some(&spec)));
    }
    Ok(v)
}

/// Creates (`id` null) or updates a custom image; returns its id. Editing the script makes the
/// built disk stale, it is not rebuilt until asked.
#[tauri::command]
pub fn images_save(app: AppHandle, project: Option<String>, id: Option<String>, spec: Spec) -> Result<String, String> {
    save_spec(&app, &Scope::of(project)?, id, spec)
}

/// Writes a custom image's definition (checked) and starts its build at once. If it
/// cannot start (the parent is not built, another build is running) it stays missing or out of
/// date, with its build button.
fn save_spec(app: &AppHandle, scope: &Scope, id: Option<String>, spec: Spec) -> Result<String, String> {
    spec.validate()?;
    let dir = spec_dir(app, scope)?;
    let global_ids: Vec<String> = read_specs(app, &Scope::global()).into_iter().map(|(i, _)| i).collect();
    let taken = |id: &str| dir.join(format!("{id}.json")).exists() || (!scope.is_global() && global_ids.iter().any(|g| g == id));
    let id = match id {
        Some(id) if valid_id(&id) => id,
        Some(_) => return Err("bad image id".into()),
        None => {
            let base = crate::config::profiles::slug(&spec.name);
            if base.is_empty() || base == BASE {
                return Err("pick another name".into());
            }
            let (mut id, mut n) = (base.clone(), 2);
            while taken(&id) {
                id = format!("{base}-{n}");
                n += 1;
            }
            id
        }
    };
    let mut specs = read_specs(app, scope);
    if resolve(app, scope, &spec.parent).is_none() {
        return Err("the image to build on does not exist".into());
    }
    specs.retain(|(i, _)| *i != id);
    if creates_cycle(&specs, &id, &spec.parent) {
        return Err("an image cannot be built on itself or on one built on it".into());
    }
    crate::util::write_json(&dir.join(format!("{id}.json")), &spec)?;
    let _ = start(app, &Ref::new(scope, &id));
    Ok(id)
}

#[tauri::command]
pub fn images_delete(app: AppHandle, project: Option<String>, id: String) -> Result<(), String> {
    let scope = Scope::of(project)?;
    if !valid_id(&id) {
        return Err("the base image cannot be deleted".into());
    }
    let r = Ref::new(&scope, &id);
    // profiles of the project, or of every project for a global image (unless it shadows theirs)
    let users: Vec<String> = match scope.id() {
        Some(p) => crate::config::profiles::used_images(&app, Some(p)),
        None => crate::config::projects::ids(&app)
            .iter()
            .filter(|p| !read_specs(&app, &Scope::project(p)).iter().any(|(i, _)| *i == id))
            .flat_map(|p| crate::config::profiles::used_images(&app, Some(p)))
            .chain(crate::config::profiles::used_images(&app, None))
            .collect(),
    };
    if users.contains(&id) {
        return Err("an agent profile uses this image".into());
    }
    if let Some((child, _)) = all_specs(&app).into_iter().find(|(c, s)| *c != r && parent_of(&app, c, s).as_ref() == Some(&r)) {
        return Err(format!("the image {} is built on it", child.id));
    }
    {
        let images = app.state::<Images>();
        let mut g = images.0.lock().unwrap();
        if g.building.as_ref() == Some(&r) {
            return Err("it is being built".into());
        }
        g.queue.retain(|q| *q != r);
    }
    std::fs::remove_file(spec_dir(&app, &scope)?.join(format!("{id}.json"))).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(disk(&app, &r)?);
    let _ = std::fs::remove_file(meta_path(&app, &r)?);
    Ok(())
}

/// Starts a build in the background; progress is in `images_list`.
#[tauri::command]
pub fn images_build(app: AppHandle, project: Option<String>, id: String) -> Result<(), String> {
    start(&app, &Ref::new(&Scope::of(project)?, &id))
}

/// The built disk of an image, if there is one.
fn built(app: &AppHandle, r: &Ref) -> Option<PathBuf> {
    disk(app, r).ok().filter(|p| p.is_file())
}

/// The built disk of the image a run in `scope` means by `id`.
pub fn path(app: &AppHandle, scope: &Scope, id: &str) -> Option<PathBuf> {
    built(app, &resolve(app, scope, id)?)
}

/// Builds the base image if it is missing or stale, and otherwise the custom images that are out
/// of date (run at startup: this is how an app update, or an edit that was never built, reaches
/// the images). When the base is rebuilt, the images built on it follow in the queue.
pub fn ensure_base(app: &AppHandle) {
    if matches!(info(app, &Ref::base(), None).state, "missing" | "stale") {
        let _ = start(app, &Ref::base());
        return;
    }
    let specs = all_specs(app);
    // Only those whose parent is up to date start now; the others follow their parent's build.
    let ready = |p: &Ref| *p == Ref::base() || specs.iter().find(|(i, _)| i == p).is_some_and(|(i, s)| info(app, i, Some(s)).state == "ready");
    let mut due: Vec<Ref> = specs.iter().filter(|(r, s)| info(app, r, Some(s)).state == "stale" && parent_of(app, r, s).is_some_and(|p| ready(&p))).map(|(r, _)| r.clone()).collect();
    if due.is_empty() {
        return;
    }
    let first = due.remove(0);
    app.state::<Images>().0.lock().unwrap().queue.extend(due);
    let _ = start(app, &first);
}

/// Creates the demo images in a project (the ones that do not exist yet) and builds them: now if
/// the base is built, else after it (while the base builds they follow it as the images built on it).
///
/// `reset`: the demo images are written again over what is there (the edits of the user are lost, other images are left alone) and the ones that are then missing or out of date are built. Refused while a build runs.
pub fn provision_defaults(app: &AppHandle, scope: &Scope, reset: bool) -> Result<(), String> {
    let dir = spec_dir(app, scope)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if reset {
        let images = app.state::<Images>();
        let g = images.0.lock().unwrap();
        if g.building.is_some() || !g.queue.is_empty() {
            return Err("an image is being built, wait for it".into());
        }
    }
    let mut created = vec![];
    for (id, spec) in crate::demo::images() {
        let file = dir.join(format!("{id}.json"));
        let r = Ref::new(scope, &id);
        if reset || !file.exists() {
            crate::util::write_json(&file, &spec)?;
            if !reset || info(app, &r, Some(&spec)).state != "ready" {
                created.push(r);
            }
        }
    }
    if info(app, &Ref::base(), None).state == "ready"
        && let Some((first, rest)) = created.split_first()
    {
        app.state::<Images>().0.lock().unwrap().queue.extend(rest.iter().cloned());
        let _ = start(app, first);
    }
    Ok(())
}

/// Images > Reset to demo content.
#[tauri::command]
pub fn images_reset_demo(app: AppHandle, project: Option<String>) -> Result<(), String> {
    provision_defaults(&app, &Scope::of(project)?, true)
}

fn start(app: &AppHandle, r: &Ref) -> Result<(), String> {
    let spec = if *r == Ref::base() { None } else { Some(spec_of(app, r).ok_or("no such image")?) };
    {
        let images = app.state::<Images>();
        let mut g = images.0.lock().unwrap();
        if let Some(b) = &g.building {
            return Err(format!("{} is being built, wait for it", b.id));
        }
        g.building = Some(r.clone());
        g.logs.insert(r.key(), vec![]);
        g.errors.remove(&r.key());
    }
    let (app, r) = (app.clone(), r.clone());
    tauri::async_runtime::spawn(async move {
        let result = build(&app, &r, spec.as_ref()).await;
        {
            let children: Vec<Ref> = if result.is_ok() { all_specs(&app).into_iter().filter(|(c, s)| parent_of(&app, c, s).as_ref() == Some(&r)).map(|(c, _)| c).collect() } else { vec![] };
            let images = app.state::<Images>();
            let mut g = images.0.lock().unwrap();
            g.building = None;
            match result {
                Ok(()) => {
                    // A rebuilt image changes everything built on it: rebuild those next, and
                    // theirs after each of them (a chain reaction, one build at a time).
                    for child in children {
                        if !g.queue.contains(&child) {
                            g.queue.push(child);
                        }
                    }
                }
                Err(e) => {
                    log::error!("image {}: build failed: {e}", r.key());
                    g.logs.entry(r.key()).or_default().push(format!("build failed: {e}"));
                    g.errors.insert(r.key(), e);
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

fn log(app: &AppHandle, r: &Ref, line: &str) {
    let images = app.state::<Images>();
    let mut g = images.0.lock().unwrap();
    log::info!("image {}: {line}", r.key());
    let log = g.logs.entry(r.key()).or_default();
    log.push(line.to_string());
    if log.len() > LOG_LINES {
        log.remove(0);
    }
}

async fn build(app: &AppHandle, r: &Ref, spec: Option<&Spec>) -> Result<(), String> {
    let stamp = wanted_stamp(app, r, spec)?;
    // The base is built from the plain image, a custom image from its finished parent.
    let source = match spec {
        None => plain_image(app)?,
        Some(s) => parent_of(app, r, s).and_then(|p| built(app, &p)).ok_or_else(|| format!("build {} first", s.parent))?,
    };
    let out = root(app, &r.scope)?.join("built");
    let work = root(app, &r.scope)?.join("work");
    crate::util::remove_dir_retry(&work).await;
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = build_in(app, r, spec, &source, &out, &work, &stamp).await;
    crate::util::remove_dir_retry(&work).await;
    result
}

/// Boots a builder VM from an overlay of `source`, runs the setup in it and flattens the disk to
/// `out/<id>.qcow2`.
async fn build_in(app: &AppHandle, r: &Ref, spec: Option<&Spec>, source: &Path, out: &Path, work: &Path, stamp: &str) -> Result<(), String> {
    let id = r.id.as_str();
    let say = |l: &str| log(app, r, l);
    let overlay = work.join("disk.qcow2");
    say("creating the builder disk");
    qemu_img(app, &["create", "-q", "-f", "qcow2", "-b", source.to_str().unwrap(), "-F", "qcow2", overlay.to_str().unwrap()]).await?;

    say("starting the builder VM");
    let serial = Serial::new(work);
    let mut vm = launch(app, &overlay, &serial)?;
    // QEMU's own complaint (bad path, no accelerator...) is the useful part when it dies at once
    let qemu_err = |e: String| match std::fs::read_to_string(work.join("qemu.err")) {
        Ok(t) if !t.trim().is_empty() => format!("{e}: {}", t.trim().lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" | ")),
        _ => e,
    };
    let mut con = Console::new(serial.connect().await.map_err(qemu_err)?);
    within(240, "waiting for the VM to boot", con.login()).await.map_err(qemu_err)?;

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
    let script_secs = u64::from(crate::config::vm_settings::current(app).image_script_mins) * 60;
    let code = within(script_secs, "the setup script", con.run(&script_cmd(script), |l| say(l))).await?;
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
    qemu_img(app, &["convert", "-O", "qcow2", overlay.to_str().unwrap(), tmp.to_str().unwrap()]).await?;
    std::fs::rename(&tmp, out.join(format!("{id}.qcow2"))).map_err(|e| e.to_string())?;
    let built_at = crate::util::unix_secs();
    let meta = serde_json::to_string(&Meta { stamp: stamp.to_string(), built_at }).unwrap();
    std::fs::write(out.join(format!("{id}.json")), meta).map_err(|e| e.to_string())?;
    say("done");
    Ok(())
}

fn launch(app: &AppHandle, disk: &Path, serial: &Serial) -> Result<tokio::process::Child, String> {
    let s = crate::config::vm_settings::current(app);
    let mut cmd = machine(app, s.builder_memory_mb, Some(s.builder_cpus), false)?;
    // builders need the internet (package managers, installers)
    cmd.args(["-nic", "user,model=virtio-net-pci"])
        .arg("-drive")
        .arg(drive_arg(disk))
        .arg("-serial")
        .arg(&serial.qemu_arg);
    // the builder's own folder holds its overlay, console socket and error log
    let work = disk.parent().unwrap_or(disk);
    super::machine::confine(app, &mut cmd, &[disk], &[work]);
    let mut cmd = Command::from(cmd);
    let err = disk.parent().and_then(|d| std::fs::File::create(d.join("qemu.err")).ok());
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(err.map_or_else(Stdio::null, Stdio::from)).kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| super::qemu::spawn_error(app, &format!("qemu-system-{}", arch().unwrap_or("?")), &e))?;
    super::machine::confine_started(app, &child);
    Ok(child)
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
        for (id, spec) in crate::demo::images() {
            assert!(valid_id(&id));
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
