//! Starting agents: one disposable VM per run. `run_vm` boots an overlay of an image with the
//! agent channel, hands the VM its identity, the Claude token and the task (`agent::drive`),
//! reports everything the agent sends and always ends with exactly one `Final`, a synthesized
//! `failed` one when the VM dies, hangs or is cancelled. The VM and its overlay are gone afterwards.
//!
//! The tauri command `agent_run` runs one agent of a profile on a prompt, the seam the workflow
//! engine will use for each step. Messages go to the frontend as `agent-run` events and to a log
//! at `<app data>/runs/logs/<run id>.jsonl` (the token is never in it).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tauri::AppHandle;
use tokio::sync::oneshot;

use crate::config::scope::Scope;
use crate::vm::launch::{LaunchError, LaunchSpec, Launched};
use crate::vm::protocol::{Bootstrap, Event, Final, Outbound, Status, Task};

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

/// The disk an agent of a session keeps across its runs (`/home/agent/work` in the VM): a sparse
/// qcow2 that lives with the session and is deleted with it.
#[derive(Clone)]
pub struct WorkDisk {
    pub path: PathBuf,
    /// Names the disk to a node that keeps it (`<project>/<session>/<node>`).
    pub key: String,
    /// Size when the disk is made; a disk that exists keeps its own.
    pub size_mb: u32,
}

/// The folder agents of a session share (`/home/agent/shared`), served to each VM over its `shared` port.
#[derive(Clone)]
pub struct SharedDir {
    pub dir: PathBuf,
    pub quota_mb: u32,
}

/// One lock per work disk: an agent whose runs overlap (the same node called twice at once) must
/// not have two VMs on one ext4.
fn disk_lock(path: &Path) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    static LOCKS: Mutex<Option<HashMap<PathBuf, std::sync::Arc<tokio::sync::Mutex<()>>>>> = Mutex::new(None);
    let mut map = LOCKS.lock().unwrap();
    map.get_or_insert_with(HashMap::new).entry(path.to_path_buf()).or_default().clone()
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
    /// The agent's disk for the whole session; `None` (single runs): `work` is a scratch folder.
    pub work_disk: Option<WorkDisk>,
    /// The session's shared folder; `None` for single runs.
    pub shared: Option<SharedDir>,
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
    /// Where the VM runs, and the slot it holds there until the run is over.
    pub placement: crate::nodes::Placement,
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
fn vm_state(on_msg: &mut (impl FnMut(&Outbound) + Send), state: &str) {
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
pub async fn run_vm(app: &AppHandle, spec: Spec, mut cancel: oneshot::Receiver<()>, mut on_msg: impl FnMut(&Outbound) + Send) -> Result<Final, String> {
    let Spec { run_id, project, image, identity, cpus, memory_mb, work_disk, shared, allow_hosts, injections, max_connections, max_upload_mb, max_download_mb, watch, task, placement } = spec;
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
    let _disk_guard = match &work_disk {
        Some(d) => {
            let lock = disk_lock(&d.path);
            tokio::select! {
                g = lock.lock_owned() => Some(g),
                _ = &mut cancel => return Ok(failed("cancelled while waiting for the agent's work disk")),
            }
        }
        None => None,
    };
    loop {
        let (account, account_name, token) = crate::config::accounts::pick(app, &tried)?;
        tried.push(account.clone());
        on_msg(&Outbound::Event(Event { seq: 0, event: serde_json::json!({"type": "account", "account": account_name}) }));
        if let Some(n) = placement.target.node_name() {
            on_msg(&Outbound::Event(Event { seq: 0, event: serde_json::json!({"type": "node", "node": n}) }));
        }
        let name = format!("{run_id}-{}", tried.len());
        let fin = boot_and_drive(app, &placement.target, &name, &disk, (cpus, memory_mb), work_disk.as_ref(), shared.as_ref(), &identity, &token, egress.clone(), task.clone(), &mut cancel, &mut on_msg).await?;
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
    target: &crate::nodes::Target,
    name: &str,
    disk: &Path,
    size: (Option<u32>, u32),
    work_disk: Option<&WorkDisk>,
    shared: Option<&SharedDir>,
    identity: &str,
    token: &str,
    egress: Egress,
    task: Task,
    cancel: &mut oneshot::Receiver<()>,
    on_msg: &mut (impl FnMut(&Outbound) + Send),
) -> Result<Final, String> {
    vm_state(on_msg, "preparing");
    // The VM's only way out: it has no network card, just a virtio-serial port whose far end is a
    // yamux session (`tunnel.rs`) that hands the agent's connections to the egress proxy.
    let (egress_tx, mut egress_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let Egress { rules, mut injections, leaks, limits } = egress;
    // The Claude token never enters the VM: the proxy sets it on the requests to the API. Last, so
    // no user injection can override it.
    injections.push(crate::config::injections::claude(token)?);
    let timeout_secs = task.timeout_secs;
    let grace_secs = crate::config::vm_settings::current(app).run_grace_secs;
    // The proxy is made (the run's CA is generated off the executor, its connections and
    // certificates warmed) while the machine half makes the disks and boots the VM.
    let net_job = async {
        let inspector = tokio::task::spawn_blocking(move || crate::net::inspect::Inspector::new(injections))
            .await
            .map_err(|e| e.to_string())??
            .watching(leaks);
        let ca = inspector.ca_pem().to_string();
        let proxy = crate::net::egress::Proxy::with_limits(rules, Some(inspector), limits);
        let meter = proxy.meter();
        // Connect to the Claude API while the VM boots, so its first requests find a connection ready.
        proxy.warm();
        Ok::<_, String>((proxy, meter, ca))
    };
    let spec = LaunchSpec { name: name.to_string(), disk: disk.to_path_buf(), cpus: size.0, memory_mb: size.1, work_disk: work_disk.cloned(), shared: shared.is_some(), timeout_secs, grace_secs };
    let mut progress = |s: &str| vm_state(on_msg, s);
    let (net, launched) = tokio::join!(net_job, crate::vm::launch::launch_on(app, target, spec, &mut progress));
    let Launched { agent, net: net_stream, shared: shared_stream, mut vm } = match launched {
        Ok(l) => l,
        Err(LaunchError::Setup(e)) => return Err(e),
        Err(LaunchError::Failed(e)) => {
            vm_state(on_msg, "off");
            return Ok(failed(e));
        }
    };
    let (proxy, meter, ca) = match net {
        Ok(n) => n,
        Err(e) => {
            vm.end().await;
            return Err(e);
        }
    };
    let ca = Some(ca);
    // The network port: the session lives as long as this value does.
    let tunnel = crate::net::tunnel::Tunnel::start(
        net_stream,
        proxy,
        std::sync::Arc::new(move |line| {
            let _ = egress_tx.send(line);
        }),
    );
    // the shared folder's server lives as long as the VM
    let shared_server = match (shared, shared_stream) {
        (Some(sd), Some(s)) => {
            let quota = crate::vm::shared::quota_for(&sd.dir, u64::from(sd.quota_mb) * 1024 * 1024);
            Some(tokio::spawn(crate::vm::shared::serve(s, sd.dir.clone(), quota)))
        }
        _ => None,
    };
    vm_state(on_msg, "booting");
    let mut running = false;
    // The stream stays open inside `drive` until the run is over.
    let mut rx = crate::vm::agent::drive(agent, Bootstrap {
            id: identity.to_string(),
            claude_token: crate::config::injections::CLAUDE_PLACEHOLDER.to_string(),
            tunnel: true,
            ca,
        },
        task,
    );

    let deadline = tokio::time::sleep(Duration::from_secs(timeout_secs.saturating_add(grace_secs)));
    tokio::pin!(deadline);
    let mut exit = None;
    // what the VM costs the host, sampled from QEMU's own process (the guest is not asked)
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
                        None => tokio::time::timeout(Duration::from_secs(5), vm.wait()).await.ok().flatten(),
                    };
                    break failed(format!("the VM ended without a result ({}): {}", status.unwrap_or_else(|| "unknown exit".into()), vm.err_tail()));
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
            _ = sample.tick(), if exit.is_none() => {
                if let Some(s) = vm.stats() {
                    // `cpu` is a percentage of one core (it can pass 100), `mem_mb` the host memory QEMU holds
                    on_msg(&Outbound::Event(Event { seq: 0, event: serde_json::json!({
                        "type": "vm_stats", "cpu": s.cpu, "mem_mb": s.mem_mb, "mem_limit_mb": size.1, "cpus": s.cpus,
                    }) }));
                }
            }
            status = vm.wait(), if exit.is_none() => {
                // Keep reading: the final may still be on its way through the channel.
                exit = Some(status);
            }
            _ = &mut deadline => break failed(format!("the run did not end within {}s, the VM was killed", timeout_secs.saturating_add(grace_secs))),
            _ = &mut *cancel => break failed("cancelled"),
        }
    };
    vm.end().await;
    drop(tunnel);
    if let Some(h) = shared_server {
        h.abort();
    }
    // the last numbers, so the log holds the run's totals
    let now = meter.totals();
    if now != last_net {
        on_msg(&net_event(&now));
    }
    vm_state(on_msg, "off");
    Ok(fin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::protocol::TaskEnv;
    use crate::vm::builder::Serial;
    use std::process::Stdio;
    use tokio::process::Command;

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
        // no NIC, the `net` port and the tunnel the app uses, with the global default allowlist
        let seen = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = seen.clone();
        let rules = crate::net::egress::Rules::new([crate::config::network::Network::default().allow_hosts]);
        let proxy = crate::net::egress::Proxy::new(rules, None);
        let channel = Serial::named(&work, "agent");
        let net_port = Serial::named(&work, "net");
        let t0 = std::time::Instant::now();
        let mut vm = Command::new(q.join("bin/qemu-system-x86_64"))
            .arg("-L")
            .arg(q.join("share/qemu"))
            .args(["-accel", if Path::new("/dev/kvm").exists() { "kvm" } else { "tcg" }, "-cpu", "max", "-m", "2048", "-smp", "2", "-M", "pc,usb=off,vmport=off", "-nodefaults", "-no-user-config", "-display", "none", "-monitor", "none"])
            .args(crate::vm::machine::boot_args(&res.join("images"), "x86_64", true))
            .args(["-no-reboot", "-action", "panic=exit-failure", "-serial", "null"])
            .args(crate::vm::launch::AGENT_NIC)
            .arg("-drive")
            .arg(crate::vm::machine::drive_arg(&overlay))
            .args(["-device", "virtio-serial-pci", "-chardev"])
            .arg(format!("{},id=agent", channel.chardev))
            .args(["-device", "virtserialport,chardev=agent,name=agent"])
            .arg("-chardev")
            .arg(format!("{},id=net", net_port.chardev))
            .args(["-device", "virtserialport,chardev=net,name=net"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stream = channel.connect().await.unwrap();
        let _tunnel = crate::net::tunnel::Tunnel::start(net_port.connect().await.unwrap(), proxy, std::sync::Arc::new(move |l| sink.lock().unwrap().push(l)));
        let task = Task {
            prompt: "say hi".into(),
            model: None,
            effort: None,
            session_id: uuid_v4(),
            resume: false,
            schema: crate::vm::protocol::RESULT_SCHEMA.into(),
            system_prompt: Some("be brief".into()),
            env: vec![TaskEnv { name: "TEAM".into(), value: "red".into() }],
            device_id: None,
            mcp_config: None,
            timeout_secs: 120,
            debug: false,
        };
        let who = identity(&[("test", "boot")], "r-test");
        let mut rx = crate::vm::agent::drive(stream, Bootstrap {
                id: who,
                claude_token: "sk-ant-oat01-fake".into(),
                tunnel: true,
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
