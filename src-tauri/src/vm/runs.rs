//! Starting agents: one disposable VM per run. `run_vm` boots an overlay of an image with the
//! agent channel, hands the VM its identity, the Claude token and the task (`agent::drive`),
//! reports everything the agent sends and always ends with exactly one `Final`, a synthesized
//! `failed` one when the VM dies, hangs or is cancelled. The VM and its overlay are gone afterwards.
//!
//! The tauri command `agent_run` runs one agent of a profile on a prompt, the seam the workflow
//! engine will use for each step. Messages go to the frontend as `agent-run` events and to a log
//! at `<app data>/runs/logs/<run id>.jsonl` (the token is never in it).

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};
use tokio::process::Command;
use tokio::sync::oneshot;

use crate::config::scope::Scope;
use crate::vm::builder::Serial;
use crate::vm::protocol::{Bootstrap, Event, Final, Outbound, Status, Task, TunnelKeys};

/// The runs in flight, to cancel them.
#[derive(Default)]
pub struct Runs(Mutex<HashMap<String, oneshot::Sender<()>>>);

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique, file-safe id for a run.
pub fn new_run_id() -> String {
    let ms = crate::util::unix_millis();
    format!("r-{ms:x}-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// The identity of one run, a SPIFFE-style name: `spiffe://ccaf.local/<kind>/<id>/.../run/<run>`.
/// It is a label that tells logs and the agent who it is, not a credential: nothing verifies it,
/// the channel (one VM, one socket) is what the app trusts. `parts` are `(kind, id)` pairs, ids
/// restricted to `[a-z0-9-]` so the name is safe in a path, a log and an environment variable.
pub fn identity(parts: &[(&str, &str)], run: &str) -> String {
    let clean = |s: &str| -> String { s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { '-' }).collect() };
    let mut out = String::from("spiffe://ccaf.local");
    for (kind, id) in parts {
        out.push_str(&format!("/{}/{}", clean(kind), clean(id)));
    }
    out.push_str(&format!("/run/{}", clean(run)));
    out
}

/// A random UUID v4, as Claude's `--session-id` wants. std's per-instance random hasher seeds the
/// bits, so no extra dependency.
pub fn uuid_v4() -> String {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_mut(8) {
        chunk.copy_from_slice(&RandomState::new().build_hasher().finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let h: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

pub fn failed(reason: impl Into<String>) -> Final {
    Final { status: Status::Failed, result: reason.into(), questions: vec![], session_id: None, tokens: None, output: None, exit_code: None }
}

fn tail(path: &Path) -> String {
    let text = std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let text = text.trim();
    let start = text.len().saturating_sub(1000);
    let start = (start..=text.len()).find(|i| text.is_char_boundary(*i)).unwrap_or(text.len());
    text[start..].to_string()
}

/// What to run: the VM (image, size), who it is and what it does.
pub struct Spec {
    pub run_id: String,
    /// The project the agent runs for: its variables, secrets, hosts and images apply.
    pub project: Scope,
    /// The id of an image of the project or a global one.
    pub image: String,
    pub identity: String,
    pub cpus: Option<u32>,
    pub memory_mb: u32,
    /// Hosts the agent may reach besides the global list (Settings > Network) and the project's: the profile's.
    pub allow_hosts: Vec<String>,
    /// The credential injections the profile switched on, applied by the egress proxy.
    pub injections: Vec<crate::config::injections::Active>,
    /// The profile's own network limits (`None`: the global setting, or no budget).
    pub max_connections: Option<u32>,
    pub max_upload_mb: Option<u32>,
    pub max_download_mb: Option<u32>,
    /// The secrets in the agent's environment, which only the profile's hosts may receive.
    pub watch: Vec<crate::net::leaks::Watched>,
    pub task: Task,
}

/// A profile's allowed hosts with their `${var:NAME}` references filled in (Settings > Variables) and
/// checked as hosts; an entry that does not come out as one fails the run.
fn resolve_hosts(hosts: &[String], var: &dyn Fn(&str) -> Option<String>) -> Result<Vec<String>, String> {
    hosts
        .iter()
        .map(|h| {
            let h = crate::config::secrets::resolve(h, "var", var).map_err(|e| format!("allowed host {h}: {e}"))?;
            crate::config::profiles::clean_host(&h)
        })
        .collect()
}

/// Tells the listener what the VM is doing (`preparing`, `starting`, `booting`, `running`, `off`), as an
/// event like the egress lines: seq 0, not Claude's, ignored by what digests the agent's own events.
fn vm_state(on_msg: &mut impl FnMut(&Outbound), state: &str) {
    on_msg(&Outbound::Event(Event { seq: 0, event: serde_json::json!({"type": "vm", "state": state}) }));
}

/// The agent's traffic so far as an event like the egress lines: seq 0, not Claude's. `up` and `down` are bytes
/// since the run started, from and to the guest.
fn net_event(t: &crate::net::egress::Totals) -> Outbound {
    Outbound::Event(Event { seq: 0, event: serde_json::json!({"type": "net", "up": t.up, "down": t.down, "open": t.open, "connections": t.connections}) })
}

/// Boots a VM of `image`, runs `task` in it and returns how it ended. `on_msg` sees every event and
/// the final (also a synthesized one), in order. `Err` only for what stops the VM from starting
/// (no token, image not built, QEMU missing). `cancel` firing ends the run as failed.
pub async fn run_vm(app: &AppHandle, spec: Spec, mut cancel: oneshot::Receiver<()>, mut on_msg: impl FnMut(&Outbound)) -> Result<Final, String> {
    let Spec { run_id, project, image, identity, cpus, memory_mb, allow_hosts, injections, max_connections, max_upload_mb, max_download_mb, watch, task } = spec;
    let disk = crate::vm::images::path(app, &project, &image).ok_or_else(|| format!("the image {image} is not built"))?;
    let vars = crate::config::variables::all(app, &project);
    let lookup = |n: &str| vars.get(n).cloned();
    let rules = crate::net::egress::Rules::new([
        resolve_hosts(&crate::config::network::current(app).allow_hosts, &lookup)?,
        resolve_hosts(&crate::config::network::project_hosts(app, &project), &lookup)?,
        resolve_hosts(&allow_hosts, &lookup)?,
    ]);
    // an injection only works on a host the agent may reach: the host is listed in the profile, not added here
    for a in &injections {
        if let Some(r) = a.rules.iter().find(|r| !rules.covers(&r.host)) {
            return Err(format!("the injection {} is for {}, which is not in the allowed hosts (the profile's, the project's or Settings > Network)", a.name, r.host));
        }
    }
    let network = crate::config::network::current(app);
    // the secrets of the environment may go only to the hosts the profile names (not the global or project lists)
    let leaks = if network.leak_detection { crate::net::leaks::Leaks::new(watch, crate::net::egress::Rules::new([resolve_hosts(&allow_hosts, &lookup)?])) } else { None };
    let mb = |m: Option<u32>| m.map(|m| u64::from(m) * 1024 * 1024);
    let limits = crate::net::egress::Limits { max_connections: max_connections.unwrap_or(network.max_connections) as usize, max_upload: mb(max_upload_mb), max_download: mb(max_download_mb) };
    let egress = Egress { rules, injections, leaks, limits };
    // Each attempt uses the next Claude account. When Claude refuses the account (limit, rejected
    // token) before any billed work was done, the run starts again on another one.
    let mut tried: Vec<String> = vec![];
    loop {
        let (account, account_name, token) = crate::config::accounts::pick(app, &tried)?;
        tried.push(account.clone());
        on_msg(&Outbound::Event(Event { seq: 0, event: serde_json::json!({"type": "account", "account": account_name}) }));
        let work = crate::util::data_dir(app)?.join("runs").join("tmp").join(format!("{run_id}-{}", tried.len()));
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let result = boot_and_drive(app, &work, &disk, (cpus, memory_mb), &identity, &token, egress.clone(), task.clone(), &mut cancel, &mut on_msg).await;
        crate::util::remove_dir_retry(&work).await;
        let fin = result?;
        if let Some(why) = crate::config::accounts::classify(&fin) {
            crate::config::accounts::rest(&account, why);
            let free = fin.tokens.is_none_or(|t| t == 0);
            if free && crate::config::accounts::pick(app, &tried).is_ok() {
                on_msg(&Outbound::Event(Event { seq: 0, event: serde_json::json!({"type": "account", "account": account_name, "refused": true}) }));
                continue;
            }
        }
        if fin.status == Status::Failed {
            log::warn!("run {run_id} failed: {}", fin.result);
        }
        on_msg(&Outbound::Final(fin.clone()));
        return Ok(fin);
    }
}

/// A fresh qcow2 overlay of `disk` at `overlay`. Making one with `qemu-img` costs a process per run,
/// so an empty overlay is made once per version of the image (path, size and time of the file) and
/// every run gets a copy of it (a reflink where the file system has them).
async fn make_overlay(app: &AppHandle, disk: &Path, overlay: &Path) -> Result<(), String> {
    use std::hash::{Hash, Hasher};
    let meta = std::fs::metadata(disk).map_err(|e| format!("the image: {e}"))?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    disk.hash(&mut h);
    let prefix = format!("{:016x}", h.finish());
    let stamp = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    let dir = runs_dir(app)?.join("overlays");
    let template = dir.join(format!("{prefix}-{}-{stamp}.qcow2", meta.len()));
    if !template.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        // older versions of this image's template are of no use any more
        for old in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if old.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_file(old.path());
            }
        }
        // unique name, then rename: runs that start together do not see a half-written file
        let tmp = dir.join(format!("{prefix}.{}.tmp", new_run_id()));
        let (base, top) = (disk.to_string_lossy(), tmp.to_string_lossy());
        crate::vm::machine::qemu_img(app, &["create", "-q", "-f", "qcow2", "-b", &base, "-F", "qcow2", &top]).await?;
        std::fs::rename(&tmp, &template).map_err(|e| e.to_string())?;
    }
    std::fs::copy(&template, overlay).map(|_| ()).map_err(|e| format!("overlay: {e}"))
}

/// Reads the files a boot needs (disk, kernel) once per process in the background, so
/// the first run after the app starts does not wait on a cold disk; later runs find them cached.
fn warm_cache(app: &AppHandle, disk: &Path) {
    static DONE: std::sync::Mutex<Vec<std::path::PathBuf>> = std::sync::Mutex::new(Vec::new());
    let mut files = vec![disk.to_path_buf()];
    if let (Ok(res), Ok(arch)) = (crate::vm::machine::resources(app), crate::vm::machine::arch()) {
        let dir = res.join("images");
        files.push(dir.join(format!("alpine-{arch}.vmlinuz")));
    }
    let Ok(mut done) = DONE.lock() else { return };
    files.retain(|f| !done.contains(f));
    done.extend(files.iter().cloned());
    drop(done);
    if !files.is_empty() {
        tokio::task::spawn_blocking(move || {
            for f in files {
                if let Ok(mut r) = std::fs::File::open(f) {
                    let _ = std::io::copy(&mut r, &mut std::io::sink());
                }
            }
        });
    }
}

/// What the egress proxy of a run enforces.
#[derive(Clone)]
struct Egress {
    rules: crate::net::egress::Rules,
    injections: Vec<crate::config::injections::Active>,
    leaks: Option<crate::net::leaks::Leaks>,
    limits: crate::net::egress::Limits,
}

#[allow(clippy::too_many_arguments)]
async fn boot_and_drive(
    app: &AppHandle,
    work: &Path,
    disk: &Path,
    size: (Option<u32>, u32),
    identity: &str,
    token: &str,
    egress: Egress,
    task: Task,
    cancel: &mut oneshot::Receiver<()>,
    on_msg: &mut impl FnMut(&Outbound),
) -> Result<Final, String> {
    vm_state(on_msg, "preparing");
    let overlay = work.join("disk.qcow2");
    warm_cache(app, disk);
    // The VM's only way out: its NIC is sealed and QEMU forwards one connection to this SSH server,
    // which hands the agent's traffic to the egress proxy. The server must listen before QEMU
    // starts, which connects to it at once.
    let (egress_tx, mut egress_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let Egress { rules, mut injections, leaks, limits } = egress;
    // The Claude token never enters the VM: the proxy sets it on the requests to the API. Last, so
    // no user injection can override it.
    injections.push(crate::config::injections::claude(token)?);
    let inspector = crate::net::inspect::Inspector::new(injections)?.watching(leaks);
    let ca = Some(inspector.ca_pem().to_string());
    let proxy = crate::net::egress::Proxy::with_limits(rules, Some(inspector), limits);
    let meter = proxy.meter();
    // Never boot the image itself: a throwaway overlay on top of it (made while the tunnel starts).
    let (overlay_made, tunnel) = tokio::join!(
        make_overlay(app, disk, &overlay),
        crate::net::tunnel::Tunnel::start(
            proxy,
            std::sync::Arc::new(move |line| {
                let _ = egress_tx.send(line);
            }),
        )
    );
    overlay_made?;
    let tunnel = tunnel?;

    let channel = Serial::named(work, "agent");
    let errlog = work.join("qemu.err");
    let timeout_secs = task.timeout_secs;
    let grace_secs = crate::config::vm_settings::current(app).run_grace_secs;
    let mut cmd = crate::vm::machine::machine(app, size.1, size.0, true)?;
    cmd.args(["-nic", &sealed_nic(tunnel.port)])
        .args(["-action", "panic=exit-failure", "-serial", "null"])
        .arg("-drive")
        .arg(crate::vm::machine::drive_arg(&overlay))
        .args(["-device", "virtio-serial-pci"])
        .arg("-chardev")
        .arg(format!("{},id=agent", channel.chardev))
        .args(["-device", "virtserialport,chardev=agent,name=agent"]);
    crate::vm::machine::confine(app, &mut cmd, &[overlay.as_path()], &[work]);
    let mut cmd = Command::from(cmd);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).kill_on_drop(true);
    cmd.stderr(std::fs::File::create(&errlog).map(Stdio::from).unwrap_or_else(|_| Stdio::null()));
    let mut child = cmd.spawn().map_err(|e| super::qemu::spawn_error(app, &format!("qemu-system-{}", crate::vm::machine::arch().unwrap_or("?")), &e))?;
    crate::vm::machine::confine_started(app, &child);
    vm_state(on_msg, "starting");

    let stream = match crate::vm::builder::within(30, "the agent channel", channel.connect()).await {
        Ok(s) => s,
        Err(e) => {
            let _ = child.kill().await;
            vm_state(on_msg, "off");
            return Ok(failed(format!("{e}: {}", tail(&errlog))));
        }
    };
    vm_state(on_msg, "booting");
    let mut running = false;
    // The stream stays open inside `drive` until the run is over.
    let mut rx = crate::vm::agent::drive(stream, Bootstrap {
            id: identity.to_string(),
            claude_token: crate::config::injections::CLAUDE_PLACEHOLDER.to_string(),
            tunnel: Some(TunnelKeys { key: tunnel.client_key.clone(), host_key: tunnel.host_key.clone() }),
            ca,
        },
        task,
    );

    let deadline = tokio::time::sleep(Duration::from_secs(timeout_secs.saturating_add(grace_secs)));
    tokio::pin!(deadline);
    let mut exit = None;
    // what the VM costs the host, sampled from QEMU's own process (the guest is not asked)
    let pid = child.id().map(sysinfo::Pid::from_u32);
    let mut sys = sysinfo::System::new();
    let mut sample = tokio::time::interval(Duration::from_secs(2));
    sample.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // what the agent's traffic has been, reported when it changes (the session view turns it into speeds)
    let mut net_tick = tokio::time::interval(Duration::from_secs(2));
    net_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_net = meter.totals();
    let fin = loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Some(Outbound::Final(f)) => break f,
                Some(m) => {
                    // the agent talks only once `agentd` is up in the guest
                    if !std::mem::replace(&mut running, true) {
                        vm_state(on_msg, "running");
                    }
                    on_msg(&m)
                }
                None => {
                    // The stream ended: QEMU is gone (or about to be) and the agent never finished.
                    let status = match exit {
                        Some(s) => s,
                        None => tokio::time::timeout(Duration::from_secs(5), child.wait()).await.ok().and_then(|r| r.ok()),
                    };
                    break failed(format!("the VM ended without a result ({}): {}", status.map_or("unknown exit".into(), |s| s.to_string()), tail(&errlog)));
                }
            },
            Some(line) = egress_rx.recv() => {
                // not Claude's: shown with the agent's events, ignored by those that read them
                on_msg(&Outbound::Event(Event { seq: 0, event: serde_json::json!({"type": "egress", "text": line}) }));
            }
            _ = net_tick.tick() => {
                let now = meter.totals();
                if now != last_net {
                    last_net = now;
                    on_msg(&net_event(&now));
                }
                // the budget counts Claude's own traffic too: past it the agent cannot reach its API and would only retry until the deadline
                if meter.exhausted() {
                    break failed("the data budget of this agent is used up, the run was stopped");
                }
            }
            _ = sample.tick(), if exit.is_none() && pid.is_some() => {
                let pid = pid.unwrap();
                sys.refresh_processes_specifics(
                    sysinfo::ProcessesToUpdate::Some(&[pid]),
                    true,
                    sysinfo::ProcessRefreshKind::nothing().with_cpu().with_memory(),
                );
                if let Some(p) = sys.process(pid) {
                    let cpus = size.0.unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get() as u32));
                    // `cpu` is a percentage of one core (it can pass 100), `mem_mb` the host memory QEMU holds
                    on_msg(&Outbound::Event(Event { seq: 0, event: serde_json::json!({
                        "type": "vm_stats", "cpu": p.cpu_usage(), "mem_mb": p.memory() / (1024 * 1024), "mem_limit_mb": size.1, "cpus": cpus,
                    }) }));
                }
            }
            status = child.wait(), if exit.is_none() => {
                // Keep reading: the final may still be on its way through the channel.
                exit = Some(status.ok());
            }
            _ = &mut deadline => break failed(format!("the run did not end within {}s, the VM was killed", timeout_secs.saturating_add(grace_secs))),
            _ = &mut *cancel => break failed("cancelled"),
        }
    };
    let _ = child.kill().await;
    // the last numbers, so the log holds the run's totals
    let now = meter.totals();
    if now != last_net {
        on_msg(&net_event(&now));
    }
    vm_state(on_msg, "off");
    Ok(fin)
}

/// The agent VM's network card: user-mode networking with everything cut off (`restrict=on`)
/// except the guest's connections to 10.0.2.100:22, which QEMU forwards to the tunnel server on
/// the host's loopback `port`. `guestfwd=tcp:` shares one host connection among all guest
/// connections, which is why the guest runs one ssh over it (see `tunnel.rs`).
fn sealed_nic(port: u16) -> String {
    format!("user,model=virtio-net-pci,restrict=on,guestfwd=tcp:10.0.2.100:22-tcp:127.0.0.1:{port}")
}

fn runs_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(crate::util::data_dir(app)?.join("runs"))
}

/// Starts an agent of a profile on a prompt; returns the run id at once. Progress arrives as
/// `agent-run` events `{run, msg}`; the `final` message is the last one.
#[tauri::command]
pub fn agent_run(app: AppHandle, project: String, profile: String, prompt: String) -> Result<String, String> {
    if prompt.trim().is_empty() || prompt.len() > 100_000 {
        return Err("the prompt must be 1 to 100000 bytes".into());
    }
    let scope = Scope::project(&project);
    let all = crate::config::profiles::available(&app, Some(&project));
    let entry = all.iter().find(|e| e.id == profile).ok_or("no such profile")?;
    // the profile with what it extends
    let resolved = crate::config::profiles::resolve(&entry.profile, &all)?;
    // Fail fast with a clear message instead of an event later.
    if !crate::config::accounts::usable(&app) {
        return Err("no Claude account is ready: add a setup token in Settings > LLM".into());
    }
    if crate::vm::images::path(&app, &scope, resolved.image_id()).is_none() {
        return Err(format!("the image {} is not built yet", resolved.image_id()));
    }

    let run_id = new_run_id();
    let who = identity(&[("project", &project), ("profile", &entry.id)], &run_id);
    let (profile_env, profile_hosts) = crate::config::integrations::expand(&app, &scope, &resolved)?;
    let (env, injections) = crate::config::injections::prepare(&app, &scope, &profile_env)?;
    let task = Task {
        prompt,
        model: resolved.model.clone(),
        effort: Some(resolved.effort_or_default()),
        session_id: uuid_v4(),
        resume: false,
        schema: crate::vm::protocol::RESULT_SCHEMA.into(),
        // the global and the project's prompt first, then the profile's (with its parents')
        system_prompt: crate::workflow::combine_system_prompts(&crate::config::system_prompt::with_project(&app, &project), &resolved.system_prompt),
        env,
        timeout_secs: 30 * 60,
    };

    let (cancel_tx, cancel_rx) = oneshot::channel();
    app.state::<Runs>().0.lock().unwrap().insert(run_id.clone(), cancel_tx);

    let log_path = runs_dir(&app)?.join("logs").join(format!("{run_id}.jsonl"));
    let _ = std::fs::create_dir_all(log_path.parent().unwrap());
    let mut log = std::fs::File::create(&log_path).ok();
    if let Some(f) = log.as_mut() {
        let head = serde_json::json!({"kind": "start", "body": {"run": run_id, "identity": who, "project": project, "profile": entry.id, "image": resolved.image_id(), "prompt": task.prompt}});
        let _ = writeln!(f, "{head}");
    }

    let id = run_id.clone();
    tauri::async_runtime::spawn(async move {
        let emit = |msg: &Outbound| {
            if let Some(f) = log.as_mut() {
                let _ = writeln!(f, "{}", serde_json::to_string(msg).unwrap_or_default());
            }
            let _ = app.emit("agent-run", serde_json::json!({"run": id, "msg": msg}));
        };
        let mut emit = emit;
        let watch = crate::net::leaks::watched(&profile_env, &|n| crate::config::secrets::secret_value(&scope, n));
        let spec = Spec { run_id: id.clone(), project: scope, image: resolved.image_id().to_string(), identity: who, cpus: resolved.cpus, memory_mb: resolved.memory(), allow_hosts: profile_hosts, injections, max_connections: resolved.max_connections, max_upload_mb: resolved.max_upload_mb, max_download_mb: resolved.max_download_mb, watch, task };
        if let Err(e) = run_vm(&app, spec, cancel_rx, &mut emit).await {
            emit(&Outbound::Final(failed(e)));
        }
        app.state::<Runs>().0.lock().unwrap().remove(&id);
    });
    Ok(run_id)
}

/// Cancels a run in flight; it ends as failed.
#[tauri::command]
pub fn agent_run_cancel(app: AppHandle, run: String) -> bool {
    app.state::<Runs>().0.lock().unwrap().remove(&run).is_some_and(|tx| tx.send(()).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::protocol::TaskEnv;

    #[test]
    fn allowed_hosts_take_variables() {
        let vars = |n: &str| (n == "HOST").then(|| " Me.Atlassian.net. ".to_string());
        let hosts = vec!["github.com".to_string(), "${var:HOST}".to_string()];
        assert_eq!(resolve_hosts(&hosts, &vars).unwrap(), vec!["github.com", "me.atlassian.net"]);
        assert!(resolve_hosts(&["${var:NOPE}".to_string()], &vars).is_err());
        let bad = |_: &str| Some("10.0.0.1".to_string());
        assert!(resolve_hosts(&["${var:HOST}".to_string()], &bad).is_err());
    }

    #[test]
    fn identity_is_safe_and_structured() {
        assert_eq!(
            identity(&[("project", "demo"), ("agent", "Dev/../x")], "r-1f-0"),
            "spiffe://ccaf.local/project/demo/agent/dev----x/run/r-1f-0"
        );
    }

    #[test]
    fn uuid_looks_like_v4() {
        let u = uuid_v4();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert!("89ab".contains(&u[19..20]));
        assert_ne!(u, uuid_v4());
    }

    #[test]
    fn run_ids_differ() {
        assert_ne!(new_run_id(), new_run_id());
    }

    /// Boots the built base image the way `run_vm` does (same QEMU arguments, minus the app handle)
    /// with a fake token: `agentd` must start as a service, read the bootstrap and task over the
    /// virtio port, run `claude` (which rejects the token) and answer with a failed final, then
    /// the VM must power off. Needs KVM, the bundled QEMU and the base image built by the app:
    /// `cargo test boots_an_agent_vm -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn boots_an_agent_vm() {
        let res = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources");
        // CCAF_BASE=<path> picks another image than the one the app built.
        let base = std::env::var_os("CCAF_BASE").map(PathBuf::from).unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap()).join(".local/share/com.alinflorin.ccagentflows/images/built/base.qcow2")
        });
        if !base.exists() || !res.join("qemu/bin/qemu-system-x86_64").exists() {
            eprintln!("needs KVM, the bundled QEMU and a built base image, skipping");
            return;
        }
        let work = std::env::temp_dir().join(format!("ccaf-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).unwrap();
        let q = res.join("qemu");
        let overlay = work.join("disk.qcow2");
        assert!(
            std::process::Command::new(q.join("bin/qemu-img"))
                .args(["create", "-q", "-f", "qcow2", "-b"])
                .arg(&base)
                .args(["-F", "qcow2"])
                .arg(&overlay)
                .status()
                .unwrap()
                .success()
        );
        // the sealed NIC and the tunnel the app uses, with the global default allowlist
        let seen = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = seen.clone();
        let rules = crate::net::egress::Rules::new([crate::config::network::Network::default().allow_hosts]);
        let tunnel = crate::net::tunnel::Tunnel::start(crate::net::egress::Proxy::new(rules, None), std::sync::Arc::new(move |l| sink.lock().unwrap().push(l))).await.unwrap();
        let channel = Serial::named(&work, "agent");
        let t0 = std::time::Instant::now();
        let mut vm = Command::new(q.join("bin/qemu-system-x86_64"))
            .arg("-L")
            .arg(q.join("share/qemu"))
            .args(["-accel", if Path::new("/dev/kvm").exists() { "kvm" } else { "tcg" }, "-cpu", "max", "-m", "2048", "-smp", "2", "-M", "pc,usb=off,vmport=off", "-nodefaults", "-no-user-config", "-display", "none", "-monitor", "none"])
            .args(crate::vm::machine::boot_args(&res.join("images"), "x86_64", true))
            .args(["-no-reboot", "-action", "panic=exit-failure", "-serial", "null"])
            .args(["-nic", &sealed_nic(tunnel.port)])
            .arg("-drive")
            .arg(crate::vm::machine::drive_arg(&overlay))
            .args(["-device", "virtio-serial-pci", "-chardev"])
            .arg(format!("{},id=agent", channel.chardev))
            .args(["-device", "virtserialport,chardev=agent,name=agent"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stream = channel.connect().await.unwrap();
        let task = Task {
            prompt: "say hi".into(),
            model: None,
            effort: None,
            session_id: uuid_v4(),
            resume: false,
            schema: crate::vm::protocol::RESULT_SCHEMA.into(),
            system_prompt: Some("be brief".into()),
            env: vec![TaskEnv { name: "TEAM".into(), value: "red".into() }],
            timeout_secs: 120,
        };
        let who = identity(&[("test", "boot")], "r-test");
        let mut rx = crate::vm::agent::drive(stream, Bootstrap {
                id: who,
                claude_token: "sk-ant-oat01-fake".into(),
                tunnel: Some(TunnelKeys { key: tunnel.client_key.clone(), host_key: tunnel.host_key.clone() }),
                ca: None,
            }, task);
        // where the time goes, from the QEMU spawn: `ready` is agentd up in the guest (boot over)
        let mark = |what: &str| eprintln!("TIMING {:>6} ms  {what}", t0.elapsed().as_millis());
        mark("channel connected");
        let fin = tokio::time::timeout(Duration::from_secs(240), async {
            let mut first = true;
            loop {
                match rx.recv().await.expect("channel closed without a final") {
                    Outbound::Final(f) => return f,
                    Outbound::Event(e) => {
                        if std::mem::take(&mut first) {
                            mark("first event from claude");
                        }
                        eprintln!("event {}: {}", e.seq, e.event.to_string().chars().take(200).collect::<String>())
                    }
                    Outbound::Ready => mark("ready (agentd up, asks for its task)"),
                }
            }
        })
        .await
        .expect("no final in time");
        mark("final");
        eprintln!("final: {:?} {}", fin.status, fin.result);
        assert_eq!(fin.status, Status::Failed, "a fake token cannot succeed");
        // claude got to the API through the tunnel and the proxy (the 401 is the API's answer)
        let log = seen.lock().unwrap().join("\n");
        eprintln!("egress log:\n{log}");
        assert!(log.contains("api.anthropic.com:443 allowed"), "the API was not reached through the proxy");
        assert!(!log.contains("denied"), "something was denied:\n{log}");
        let exited = tokio::time::timeout(Duration::from_secs(60), vm.wait()).await.expect("the VM did not power off").unwrap();
        mark("QEMU exited");
        eprintln!("qemu exit: {exited}");
        let _ = std::fs::remove_dir_all(work);
    }
}
