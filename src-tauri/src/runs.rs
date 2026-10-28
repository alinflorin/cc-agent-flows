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

use crate::builder::Serial;
use crate::protocol::{Bootstrap, Event, Final, Outbound, Status, Task, TunnelKeys};

/// Time on top of the task's timeout for the VM to boot and shut down.
const GRACE_SECS: u64 = 300;

/// The runs in flight, to cancel them.
#[derive(Default)]
pub struct Runs(Mutex<HashMap<String, oneshot::Sender<()>>>);

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique, file-safe id for a run.
pub fn new_run_id() -> String {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
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
    Final { status: Status::Failed, result: reason.into(), questions: vec![], session_id: None, cost_usd: None, output: None, exit_code: None }
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
    pub image: String,
    pub identity: String,
    pub cpus: Option<u32>,
    pub memory_mb: u32,
    /// Hosts the agent may reach besides the global list (Settings > Network): the profile's.
    pub allow_hosts: Vec<String>,
    /// The credential injections the profile switched on, applied by the egress proxy.
    pub injections: Vec<crate::injections::Active>,
    pub task: Task,
}

/// A profile's allowed hosts with their `${var:NAME}` references filled in (Settings > Variables) and
/// checked as hosts; an entry that does not come out as one fails the run.
fn resolve_hosts(hosts: &[String], var: &dyn Fn(&str) -> Option<String>) -> Result<Vec<String>, String> {
    hosts
        .iter()
        .map(|h| {
            let h = crate::secrets::resolve(h, "var", var).map_err(|e| format!("allowed host {h}: {e}"))?;
            crate::profiles::clean_host(&h)
        })
        .collect()
}

/// Tells the listener what the VM is doing (`preparing`, `starting`, `booting`, `running`, `off`), as an
/// event like the egress lines: seq 0, not Claude's, ignored by what digests the agent's own events.
fn vm_state(on_msg: &mut impl FnMut(&Outbound), state: &str) {
    on_msg(&Outbound::Event(Event { seq: 0, event: serde_json::json!({"type": "vm", "state": state}) }));
}

/// Boots a VM of `image`, runs `task` in it and returns how it ended. `on_msg` sees every event and
/// the final (also a synthesized one), in order. `Err` only for what stops the VM from starting
/// (no token, image not built, QEMU missing). `cancel` firing ends the run as failed.
pub async fn run_vm(app: &AppHandle, spec: Spec, cancel: oneshot::Receiver<()>, mut on_msg: impl FnMut(&Outbound)) -> Result<Final, String> {
    let Spec { run_id, image, identity, cpus, memory_mb, allow_hosts, injections, task } = spec;
    let token = crate::secrets::claude_token().ok_or("no Claude token is set (Settings > LLM)")?;
    let disk = crate::images::path(app, &image).ok_or_else(|| format!("the image {image} is not built"))?;
    let work = app.path().app_data_dir().map_err(|e| e.to_string())?.join("runs").join("tmp").join(&run_id);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let vars = crate::variables::all(app);
    let allow_hosts = resolve_hosts(&allow_hosts, &|n| vars.get(n).cloned())?;
    let rules = crate::egress::Rules::new([crate::network::current(app).allow_hosts, allow_hosts]);
    // an injection only works on a host the agent may reach: the host is listed in the profile, not added here
    for a in &injections {
        if let Some(r) = a.rules.iter().find(|r| !rules.covers(&r.host)) {
            return Err(format!("the injection {} is for {}, which is not in the profile's allowed hosts (or Settings > Network)", a.name, r.host));
        }
    }
    let result = boot_and_drive(app, &work, &disk, (cpus, memory_mb), &identity, &token, (rules, injections), task, cancel, &mut on_msg).await;
    let _ = std::fs::remove_dir_all(&work);
    let fin = result?;
    on_msg(&Outbound::Final(fin.clone()));
    Ok(fin)
}

#[allow(clippy::too_many_arguments)]
async fn boot_and_drive(
    app: &AppHandle,
    work: &Path,
    disk: &Path,
    size: (Option<u32>, u32),
    identity: &str,
    token: &str,
    egress: (crate::egress::Rules, Vec<crate::injections::Active>),
    task: Task,
    mut cancel: oneshot::Receiver<()>,
    on_msg: &mut impl FnMut(&Outbound),
) -> Result<Final, String> {
    vm_state(on_msg, "preparing");
    let overlay = work.join("disk.qcow2");
    // Never boot the image itself: a throwaway overlay on top of it.
    crate::images::qemu_img(
        app,
        &["create", "-q", "-f", "qcow2", "-b", &disk.to_string_lossy(), "-F", "qcow2", &overlay.to_string_lossy()],
    )
    .await?;

    // The VM's only way out: its NIC is sealed and QEMU forwards one connection to this SSH server,
    // which hands the agent's traffic to the egress proxy. The server must listen before QEMU
    // starts, which connects to it at once.
    let (egress_tx, mut egress_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (rules, injections) = egress;
    let inspector = if injections.is_empty() { None } else { Some(crate::inspect::Inspector::new(injections)?) };
    let ca = inspector.as_ref().map(|i| i.ca_pem().to_string());
    let tunnel = crate::tunnel::Tunnel::start(
        crate::egress::Proxy::new(rules, inspector),
        std::sync::Arc::new(move |line| {
            let _ = egress_tx.send(line);
        }),
    )
    .await?;

    let channel = Serial::named(work, "agent");
    let errlog = work.join("qemu.err");
    let timeout_secs = task.timeout_secs;
    let mut cmd = crate::images::machine(app, size.1, size.0)?;
    cmd.args(["-nic", &sealed_nic(tunnel.port)])
        .args(["-action", "panic=exit-failure", "-serial", "null"])
        .arg("-drive")
        .arg(format!("file={},if=virtio,discard=unmap", overlay.display()))
        .args(["-device", "virtio-serial-pci"])
        .arg("-chardev")
        .arg(format!("{},id=agent", channel.chardev))
        .args(["-device", "virtserialport,chardev=agent,name=agent"]);
    let mut cmd = Command::from(cmd);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).kill_on_drop(true);
    cmd.stderr(std::fs::File::create(&errlog).map(Stdio::from).unwrap_or_else(|_| Stdio::null()));
    let mut child = cmd.spawn().map_err(|e| format!("cannot start QEMU: {e}"))?;
    vm_state(on_msg, "starting");

    let stream = match crate::builder::within(30, "the agent channel", channel.connect()).await {
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
    let mut rx = crate::agent::drive(stream, Bootstrap {
            id: identity.to_string(),
            claude_token: token.to_string(),
            tunnel: Some(TunnelKeys { key: tunnel.client_key.clone(), host_key: tunnel.host_key.clone() }),
            ca,
        },
        task,
    );

    let deadline = tokio::time::sleep(Duration::from_secs(timeout_secs.saturating_add(GRACE_SECS)));
    tokio::pin!(deadline);
    let mut exit = None;
    // what the VM costs the host, sampled from QEMU's own process (the guest is not asked)
    let pid = child.id().map(sysinfo::Pid::from_u32);
    let mut sys = sysinfo::System::new();
    let mut sample = tokio::time::interval(Duration::from_secs(2));
    sample.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
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
            _ = &mut deadline => break failed(format!("the run did not end within {}s, the VM was killed", timeout_secs.saturating_add(GRACE_SECS))),
            _ = &mut cancel => break failed("cancelled"),
        }
    };
    let _ = child.kill().await;
    vm_state(on_msg, "off");
    Ok(fin)
}

/// The agent VM's network card: user-mode networking with everything cut off (`restrict=on`)
/// except the guest's connections to 10.0.2.100:22, which QEMU forwards to the tunnel server on
/// the host's loopback `port`. `guestfwd=tcp:` shares one host connection among all guest
/// connections, which is why the guest runs one ssh over it (see `tunnel.rs`).
fn sealed_nic(port: u16) -> String {
    format!("user,restrict=on,guestfwd=tcp:10.0.2.100:22-tcp:127.0.0.1:{port}")
}

fn runs_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("runs"))
}

/// Starts an agent of a profile on a prompt; returns the run id at once. Progress arrives as
/// `agent-run` events `{run, msg}`; the `final` message is the last one.
#[tauri::command]
pub fn agent_run(app: AppHandle, profile: String, prompt: String) -> Result<String, String> {
    if prompt.trim().is_empty() || prompt.len() > 100_000 {
        return Err("the prompt must be 1 to 100000 bytes".into());
    }
    let profiles_dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("profiles");
    let entry = crate::profiles::list(&profiles_dir).into_iter().find(|e| e.id == profile).ok_or("no such profile")?;
    // Fail fast with a clear message instead of an event later.
    if crate::secrets::claude_token().is_none() {
        return Err("no Claude token is set (Settings > LLM)".into());
    }
    if crate::images::path(&app, &entry.profile.image).is_none() {
        return Err(format!("the image {} is not built yet", entry.profile.image));
    }

    let run_id = new_run_id();
    let who = identity(&[("profile", &entry.id)], &run_id);
    let (env, injections) = crate::injections::prepare(&app, &entry.profile.env)?;
    let task = Task {
        prompt,
        model: entry.profile.model.clone(),
        effort: entry.profile.effort.clone(),
        session_id: uuid_v4(),
        resume: false,
        schema: crate::protocol::RESULT_SCHEMA.into(),
        system_prompt: Some(entry.profile.system_prompt.clone()).filter(|s| !s.trim().is_empty()),
        env,
        timeout_secs: 30 * 60,
    };

    let (cancel_tx, cancel_rx) = oneshot::channel();
    app.state::<Runs>().0.lock().unwrap().insert(run_id.clone(), cancel_tx);

    let log_path = runs_dir(&app)?.join("logs").join(format!("{run_id}.jsonl"));
    let _ = std::fs::create_dir_all(log_path.parent().unwrap());
    let mut log = std::fs::File::create(&log_path).ok();
    if let Some(f) = log.as_mut() {
        let head = serde_json::json!({"kind": "start", "body": {"run": run_id, "identity": who, "profile": entry.id, "image": entry.profile.image, "prompt": task.prompt}});
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
        let spec = Spec { run_id: id.clone(), image: entry.profile.image.clone(), identity: who, cpus: entry.profile.cpus, memory_mb: entry.profile.memory_mb, allow_hosts: entry.profile.allow_hosts.clone(), injections, task };
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
    use crate::protocol::TaskEnv;

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
        if !Path::new("/dev/kvm").exists() || !base.exists() || !res.join("qemu/bin/qemu-system-x86_64").exists() {
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
        let rules = crate::egress::Rules::new([crate::network::Network::default().allow_hosts]);
        let tunnel = crate::tunnel::Tunnel::start(crate::egress::Proxy::new(rules, None), std::sync::Arc::new(move |l| sink.lock().unwrap().push(l))).await.unwrap();
        let channel = Serial::named(&work, "agent");
        let mut vm = Command::new(q.join("bin/qemu-system-x86_64"))
            .arg("-L")
            .arg(q.join("share/qemu"))
            .args(["-accel", "kvm", "-cpu", "max", "-m", "2048", "-smp", "2", "-display", "none", "-monitor", "none"])
            .args(["-no-reboot", "-action", "panic=exit-failure", "-serial", "null"])
            .args(["-nic", &sealed_nic(tunnel.port)])
            .arg("-drive")
            .arg(format!("file={},if=virtio,discard=unmap", overlay.display()))
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
            schema: crate::protocol::RESULT_SCHEMA.into(),
            system_prompt: Some("be brief".into()),
            env: vec![TaskEnv { name: "TEAM".into(), value: "red".into() }],
            timeout_secs: 120,
        };
        let who = identity(&[("test", "boot")], "r-test");
        let mut rx = crate::agent::drive(stream, Bootstrap {
                id: who,
                claude_token: "sk-ant-oat01-fake".into(),
                tunnel: Some(TunnelKeys { key: tunnel.client_key.clone(), host_key: tunnel.host_key.clone() }),
                ca: None,
            }, task);
        let fin = tokio::time::timeout(Duration::from_secs(240), async {
            loop {
                match rx.recv().await.expect("channel closed without a final") {
                    Outbound::Final(f) => return f,
                    Outbound::Event(e) => eprintln!("event {}: {}", e.seq, e.event.to_string().chars().take(200).collect::<String>()),
                    Outbound::Ready => {}
                }
            }
        })
        .await
        .expect("no final in time");
        eprintln!("final: {:?} {}", fin.status, fin.result);
        assert_eq!(fin.status, Status::Failed, "a fake token cannot succeed");
        // claude got to the API through the tunnel and the proxy (the 401 is the API's answer)
        let log = seen.lock().unwrap().join("\n");
        eprintln!("egress log:\n{log}");
        assert!(log.contains("api.anthropic.com:443 allowed"), "the API was not reached through the proxy");
        assert!(!log.contains("denied"), "something was denied:\n{log}");
        let exited = tokio::time::timeout(Duration::from_secs(60), vm.wait()).await.expect("the VM did not power off").unwrap();
        eprintln!("qemu exit: {exited}");
        let _ = std::fs::remove_dir_all(work);
    }
}
