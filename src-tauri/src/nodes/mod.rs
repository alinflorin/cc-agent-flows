//! VMs on other computers. The app that runs a session (the **hub**) can start the VM of an agent on
//! another computer running this same app (a **node**): the hub keeps everything that matters (the
//! engine and its journal, the egress proxy with the real Claude token and the injections, the
//! shared folder, the deadline) and only the VM itself runs elsewhere. What crosses the network is
//! what already was a byte stream: the three virtio-serial ports of the VM (`agent`, `net`,
//! `shared`) and a control channel (see `proto.rs`, `vm/launch.rs`). The nodes are trusted: they see
//! the VM's memory and disks and what the agent does; they never see a secret the hub injects.
//!
//! - `server.rs`: the node's side. Off by default (Settings > Nodes > "Let other computers use this
//!   one", or `--node`); a TLS listener (`tls.rs`) that checks the key and runs `vm::launch::launch`.
//! - `client.rs`: the hub's side. Ships images a node lacks, starts the VM, relays the ports.
//! - this file: the settings of both roles, the node list with its status, and the scheduler that
//!   picks the computer for a run (`place`).
//!
//! Where an agent's work disk lives (a disk that outlives the VM) is where its first run was
//! placed, and its later runs follow it (`pins`); if that computer is gone the run fails rather
//! than start with a blank disk.

pub mod client;
pub mod proto;
pub mod server;
pub mod tls;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

pub const DEFAULT_PORT: u16 = 7881;

/// `--node`: accept VMs from other computers whatever the setting says.
pub fn forced() -> bool {
    std::env::args().any(|a| a == "--node")
}

/// Equal without stopping at the first difference.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn new_key() -> String {
    let bytes: [u8; 24] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn store<T: Serialize>(app: &AppHandle, file: &str, value: &T) -> Result<(), String> {
    let p = crate::util::data_dir(app)?.join(file);
    crate::util::write_json(&p, value)?;
    // the files hold keys
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn load<T: for<'a> Deserialize<'a>>(app: &AppHandle, file: &str) -> Option<T> {
    crate::util::read_json(&crate::util::data_dir(app).ok()?.join(file))
}

/// This computer as a node (`node.json`).
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeConfig {
    pub enabled: bool,
    pub bind: String,
    pub port: u16,
    /// What a hub must present.
    pub key: String,
}

impl Default for NodeConfig {
    fn default() -> Self {
        NodeConfig { enabled: false, bind: "0.0.0.0".into(), port: DEFAULT_PORT, key: String::new() }
    }
}

/// A computer that runs VMs for this one.
#[derive(Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub name: String,
    /// `host:port`.
    pub addr: String,
    pub key: String,
    /// The certificate that was trusted when the node was added.
    pub fingerprint: String,
    pub enabled: bool,
    /// VMs this node may run at once for this hub.
    pub slots: u32,
}

/// The computers this one sends VMs to (`nodes.json`).
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HubConfig {
    /// This computer runs VMs too.
    pub local: bool,
    pub nodes: Vec<Node>,
}

impl Default for HubConfig {
    fn default() -> Self {
        HubConfig { local: true, nodes: vec![] }
    }
}

#[derive(Clone, Default, Serialize)]
pub struct NodeStatus {
    /// The last check reached the node and its key was accepted.
    pub online: bool,
    /// Why it is not usable.
    pub error: Option<String>,
    pub version: Option<String>,
    pub arch: Option<String>,
    pub os: Option<String>,
}

#[derive(Default)]
pub struct Nodes {
    node: Mutex<NodeConfig>,
    server: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    server_error: Mutex<Option<String>>,
    hub: Mutex<HubConfig>,
    status: Mutex<HashMap<String, NodeStatus>>,
    /// A semaphore per node, remade when its slots change.
    sems: Mutex<HashMap<String, (u32, Arc<Semaphore>)>>,
    /// Work disk key to where it lives: `local` or a node id.
    pins: Mutex<HashMap<String, String>>,
}

fn state(app: &AppHandle) -> tauri::State<'_, Nodes> {
    app.state::<Nodes>()
}

/// Loads the settings, starts the server when this computer is a node and watches the nodes.
/// Call once at startup, after `app.manage(Nodes::default())`.
pub fn init(app: &AppHandle) {
    let s = state(app);
    let mut node: NodeConfig = load(app, "node.json").unwrap_or_default();
    if node.key.is_empty() {
        node.key = new_key();
        let _ = store(app, "node.json", &node);
    }
    *s.node.lock().unwrap() = node;
    *s.hub.lock().unwrap() = load(app, "nodes.json").unwrap_or_default();
    *s.pins.lock().unwrap() = load(app, "node-pins.json").unwrap_or_default();
    server::start(app);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            check_all(&app).await;
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    });
}

fn hub(app: &AppHandle) -> HubConfig {
    state(app).hub.lock().unwrap().clone()
}

fn node_config(app: &AppHandle) -> NodeConfig {
    state(app).node.lock().unwrap().clone()
}

/// Asks every enabled node who it is, in parallel, and keeps the answers.
async fn check_all(app: &AppHandle) {
    let nodes = hub(app).nodes;
    let mut jobs = vec![];
    for n in nodes.iter().filter(|n| n.enabled) {
        let (app, n) = (app.clone(), n.clone());
        jobs.push(tauri::async_runtime::spawn(async move {
            let st = check(&n).await;
            state(&app).status.lock().unwrap().insert(n.id.clone(), st);
        }));
    }
    for j in jobs {
        let _ = j.await;
    }
    let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    state(app).status.lock().unwrap().retain(|k, _| ids.contains(k));
}

async fn check(n: &Node) -> NodeStatus {
    match client::ping(n).await {
        Ok(info) => {
            let mine = crate::vm::machine::arch().unwrap_or("?");
            let error = (info.arch != mine).then(|| format!("it is {}, this computer is {mine}: images are built for one architecture", info.arch));
            NodeStatus { online: true, error, version: Some(info.version), arch: Some(info.arch), os: Some(info.os) }
        }
        Err(e) => NodeStatus { error: Some(e), ..Default::default() },
    }
}

fn usable(st: &NodeStatus) -> bool {
    st.online && st.error.is_none()
}

fn sem_for(app: &AppHandle, n: &Node) -> Arc<Semaphore> {
    let st = state(app);
    let mut sems = st.sems.lock().unwrap();
    match sems.get(&n.id) {
        Some((slots, s)) if *slots == n.slots => s.clone(),
        _ => {
            let s = Arc::new(Semaphore::new(n.slots as usize));
            sems.insert(n.id.clone(), (n.slots, s.clone()));
            s
        }
    }
}

/// Where a run's VM goes.
#[derive(Clone)]
pub enum Target {
    Local,
    Node(Node),
}

impl Target {
    /// The node's name, for the session log; `None` for this computer.
    pub fn node_name(&self) -> Option<&str> {
        match self {
            Target::Local => None,
            Target::Node(n) => Some(&n.name),
        }
    }
}

/// A place for one VM, held for as long as the run (the permit frees the slot).
pub struct Placement {
    pub target: Target,
    _permit: OwnedSemaphorePermit,
}

type Candidate = (Target, String, Arc<Semaphore>, usize);

fn candidates(app: &AppHandle, pinned: Option<&str>) -> Result<Vec<Candidate>, String> {
    let cfg = hub(app);
    let local = || {
        let slots = app.state::<crate::workflow::engine::VmSlots>();
        (Target::Local, "local".to_string(), slots.semaphore(), slots.capacity())
    };
    match pinned {
        Some("local") => {
            if !cfg.local {
                return Err("this agent's work disk is on this computer, which is switched off as a VM host in Settings > Nodes".into());
            }
            Ok(vec![local()])
        }
        Some(id) => {
            let n = cfg.nodes.iter().find(|n| n.id == id).ok_or("the node that holds this agent's work disk was removed")?;
            let ok = n.enabled && state(app).status.lock().unwrap().get(&n.id).is_some_and(usable);
            if !ok {
                return Err(format!("the node {} holds this agent's work disk and is not available", n.name));
            }
            Ok(vec![(Target::Node(n.clone()), n.id.clone(), sem_for(app, n), n.slots as usize)])
        }
        None => {
            let mut out = vec![];
            if cfg.local {
                out.push(local());
            }
            let status = state(app).status.lock().unwrap().clone();
            for n in cfg.nodes.iter().filter(|n| n.enabled && status.get(&n.id).is_some_and(usable)) {
                out.push((Target::Node(n.clone()), n.id.clone(), sem_for(app, n), n.slots as usize));
            }
            if out.is_empty() {
                return Err("no computer is available to run VMs: switch one on in Settings > Nodes".into());
            }
            Ok(out)
        }
    }
}

/// Picks the computer for a run and waits for a free slot on it. `disk_key` names the agent's work
/// disk: a disk that exists pins the run to the computer that has it, a new one is pinned where the
/// run lands. `None` when `cancel` fired first.
pub async fn place(app: &AppHandle, cancel: &mut watch::Receiver<bool>, disk_key: Option<&str>) -> Result<Option<Placement>, String> {
    loop {
        let pinned = disk_key.and_then(|k| state(app).pins.lock().unwrap().get(k).cloned());
        let mut cands = candidates(app, pinned.as_deref())?;
        // the emptiest first (the share of its slots in use), this computer before the others on a tie
        let free = |c: &Candidate| c.2.available_permits() as f64 / c.3.max(1) as f64;
        cands.sort_by(|a, b| free(b).partial_cmp(&free(a)).unwrap_or(std::cmp::Ordering::Equal));
        let mut raced = false;
        for (target, id, sem, _) in cands {
            let Ok(permit) = sem.try_acquire_owned() else { continue };
            if let (Some(k), None) = (disk_key, &pinned) {
                let s = state(app);
                let mut pins = s.pins.lock().unwrap();
                match pins.get(k) {
                    // another run of the same agent was placed meanwhile: go where its disk is
                    Some(p) if *p != id => {
                        raced = true;
                        break;
                    }
                    Some(_) => {}
                    None => {
                        pins.insert(k.to_string(), id);
                        let _ = store(app, "node-pins.json", &*pins);
                    }
                }
            }
            return Ok(Some(Placement { target, _permit: permit }));
        }
        if !raced {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(300)) => {}
                r = cancel.wait_for(|v| *v) => { let _ = r; return Ok(None); }
            }
        }
    }
}

/// A session or project was deleted: forget where its work disks were and tell the nodes to delete them.
pub fn forget(app: &AppHandle, prefix: &str) {
    {
        let s = state(app);
        let mut pins = s.pins.lock().unwrap();
        let before = pins.len();
        pins.retain(|k, _| !k.starts_with(prefix));
        if pins.len() != before {
            let _ = store(app, "node-pins.json", &*pins);
        }
    }
    for n in hub(app).nodes {
        let prefix = prefix.to_string();
        tauri::async_runtime::spawn(async move {
            let _ = client::drop_disks(&n, &prefix).await;
        });
    }
}

// ---- commands: the node role

#[derive(Serialize)]
pub struct RoleView {
    enabled: bool,
    bind: String,
    port: u16,
    key: String,
    fingerprint: String,
    /// On because of `--node`.
    forced: bool,
    error: Option<String>,
}

fn role_view(app: &AppHandle) -> RoleView {
    let c = node_config(app);
    RoleView {
        enabled: c.enabled,
        bind: c.bind,
        port: c.port,
        key: c.key,
        fingerprint: tls::Identity::load(app).map(|i| i.fingerprint()).unwrap_or_default(),
        forced: forced(),
        error: state(app).server_error.lock().unwrap().clone(),
    }
}

#[tauri::command]
pub fn node_get(app: AppHandle) -> RoleView {
    role_view(&app)
}

#[tauri::command]
pub fn node_set(app: AppHandle, enabled: bool, bind: String, port: u16) -> Result<RoleView, String> {
    if port < 1024 {
        return Err("pick a port from 1024 up".into());
    }
    if bind.parse::<std::net::IpAddr>().is_err() {
        return Err("the address must be an IP address, like 0.0.0.0 (every network) or 192.168.1.20".into());
    }
    let mut c = node_config(&app);
    (c.enabled, c.bind, c.port) = (enabled, bind, port);
    store(&app, "node.json", &c)?;
    *state(&app).node.lock().unwrap() = c;
    restart_server(&app);
    Ok(role_view(&app))
}

/// A new key: hubs that had the old one are refused until they are given this one.
#[tauri::command]
pub fn node_new_key(app: AppHandle) -> Result<RoleView, String> {
    let mut c = node_config(&app);
    c.key = new_key();
    store(&app, "node.json", &c)?;
    *state(&app).node.lock().unwrap() = c;
    Ok(role_view(&app))
}

fn restart_server(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // the answer to the command that asked for it goes out first
        tokio::time::sleep(Duration::from_millis(400)).await;
        server::start(&app);
    });
}

// ---- commands: the hub role

#[derive(Serialize)]
pub struct NodeView {
    id: String,
    name: String,
    addr: String,
    fingerprint: String,
    enabled: bool,
    slots: u32,
    running: usize,
    #[serde(flatten)]
    status: NodeStatus,
}

#[derive(Serialize)]
pub struct HubView {
    local: bool,
    local_slots: usize,
    local_running: usize,
    nodes: Vec<NodeView>,
}

#[tauri::command]
pub fn nodes_get(app: AppHandle) -> HubView {
    let cfg = hub(&app);
    let slots = app.state::<crate::workflow::engine::VmSlots>();
    let status = state(&app).status.lock().unwrap().clone();
    let nodes = cfg
        .nodes
        .iter()
        .map(|n| NodeView {
            id: n.id.clone(),
            name: n.name.clone(),
            addr: n.addr.clone(),
            fingerprint: n.fingerprint.clone(),
            enabled: n.enabled,
            slots: n.slots,
            running: (n.slots as usize).saturating_sub(sem_for(&app, n).available_permits()),
            status: status.get(&n.id).cloned().unwrap_or_default(),
        })
        .collect();
    HubView { local: cfg.local, local_slots: slots.capacity(), local_running: slots.capacity().saturating_sub(slots.semaphore().available_permits()), nodes }
}

#[tauri::command]
pub fn nodes_set_local(app: AppHandle, local: bool) -> Result<(), String> {
    let mut cfg = hub(&app);
    if !local && !cfg.nodes.iter().any(|n| n.enabled) {
        return Err("switch a node on first: some computer has to run the VMs".into());
    }
    cfg.local = local;
    store(&app, "nodes.json", &cfg)?;
    *state(&app).hub.lock().unwrap() = cfg;
    Ok(())
}

/// Connects to `addr` and returns the fingerprint of its certificate, for the user to compare with
/// the one the node shows. Nothing secret is sent.
#[tauri::command]
pub async fn nodes_probe(addr: String) -> Result<String, String> {
    check_addr(&addr)?;
    let (_, fp) = tls::dial(addr.trim(), None).await?;
    Ok(fp)
}

fn check_addr(addr: &str) -> Result<(), String> {
    let ok = addr.trim().rsplit_once(':').is_some_and(|(h, p)| !h.is_empty() && p.parse::<u16>().is_ok_and(|p| p > 0));
    if ok { Ok(()) } else { Err("the address must be host:port, like 192.168.1.20:7881".into()) }
}

/// Adds a node or changes one (`id`). Checks that the node answers with the trusted certificate and the key.
/// An empty `key` keeps the saved one.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn nodes_save(app: AppHandle, id: Option<String>, name: String, addr: String, key: Option<String>, fingerprint: String, enabled: bool, slots: u32) -> Result<String, String> {
    let name = name.trim().to_string();
    if name.is_empty() || name.chars().count() > 40 {
        return Err("the name must be 1 to 40 characters".into());
    }
    check_addr(&addr)?;
    if !(1..=32).contains(&slots) {
        return Err("slots must be from 1 to 32".into());
    }
    if fingerprint.trim().is_empty() {
        return Err("check the node's certificate first".into());
    }
    let mut cfg = hub(&app);
    let old = id.as_ref().and_then(|i| cfg.nodes.iter().find(|n| &n.id == i)).cloned();
    if id.is_some() && old.is_none() {
        return Err("no such node".into());
    }
    let key = key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).or_else(|| old.as_ref().map(|o| o.key.clone())).ok_or("the node's key is needed")?;
    let node = Node { id: old.as_ref().map_or_else(|| format!("n-{:x}", crate::util::unix_millis()), |o| o.id.clone()), name, addr: addr.trim().to_string(), key, fingerprint: fingerprint.trim().to_string(), enabled, slots };
    if cfg.nodes.iter().any(|n| n.id != node.id && n.addr == node.addr) {
        return Err("a node with this address exists".into());
    }
    // the check that the node takes this key and certificate, even when it is switched off (a typo shows now, not at the first run)
    let st = check(&node).await;
    if !st.online {
        return Err(st.error.unwrap_or_else(|| "the node does not answer".into()));
    }
    match cfg.nodes.iter_mut().find(|n| n.id == node.id) {
        Some(slot) => *slot = node.clone(),
        None => cfg.nodes.push(node.clone()),
    }
    store(&app, "nodes.json", &cfg)?;
    *state(&app).hub.lock().unwrap() = cfg;
    state(&app).status.lock().unwrap().insert(node.id.clone(), st);
    Ok(node.id)
}

#[tauri::command]
pub fn nodes_delete(app: AppHandle, id: String) -> Result<(), String> {
    let mut cfg = hub(&app);
    let Some(n) = cfg.nodes.iter().find(|n| n.id == id) else { return Err("no such node".into()) };
    if state(&app).pins.lock().unwrap().values().any(|v| *v == id) {
        log::warn!("node {} removed while agents of sessions keep work disks on it; their next runs will fail", n.name);
    }
    cfg.nodes.retain(|n| n.id != id);
    if !cfg.local && !cfg.nodes.iter().any(|n| n.enabled) {
        cfg.local = true;
    }
    store(&app, "nodes.json", &cfg)?;
    *state(&app).hub.lock().unwrap() = cfg;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_need_a_host_and_a_port() {
        assert!(check_addr("192.168.1.20:7881").is_ok());
        assert!(check_addr("pc-12.local:7881").is_ok());
        assert!(check_addr("[::1]:7881").is_ok());
        assert!(check_addr("pc-12").is_err());
        assert!(check_addr(":7881").is_err());
        assert!(check_addr("pc:0").is_err());
    }

    #[test]
    fn keys_compare_whole() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "ab"));
    }
}
