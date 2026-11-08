//! The machine half of one agent run: make the VM's disks, start QEMU and hand back the three byte
//! streams the app talks to it over (`agent`, `net`, and `shared` when the session has a folder),
//! plus a handle to watch and end the VM. Everything the app knows about the run (the egress
//! proxy and its secrets, the shared folder's contents, the journal, the deadline) stays on the
//! other side of that line (`runs.rs`), which is what lets a VM run on another computer: only
//! these streams and the handle have to cross.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tauri::AppHandle;
use tokio::process::Command;

use crate::vm::builder::{Serial, Stream, within};
use crate::vm::runs::WorkDisk;

/// The agent VM has no network card at all: what it reaches goes through the `net` port (see
/// `tunnel.rs`).
pub const AGENT_NIC: [&str; 2] = ["-nic", "none"];

/// What the machine half needs to know.
pub struct LaunchSpec {
    /// A name for this attempt, unique among the runs on the machine: its folder under `runs/tmp`.
    pub name: String,
    /// The image's disk; the VM boots a throwaway overlay of it.
    pub disk: PathBuf,
    pub cpus: Option<u32>,
    pub memory_mb: u32,
    /// The agent's disk for the whole session; it outlives the VM.
    pub work_disk: Option<WorkDisk>,
    /// Whether the VM gets the `shared` port.
    pub shared: bool,
    /// The run's deadline plus the grace, to bound the QEMU process's CPU time.
    pub timeout_secs: u64,
    pub grace_secs: u64,
}

pub struct Launched {
    pub agent: Box<dyn Stream>,
    pub net: Box<dyn Stream>,
    pub shared: Option<Box<dyn Stream>>,
    pub vm: Vm,
}

pub enum LaunchError {
    /// The VM could not be started at all (image missing, QEMU missing, a disk that cannot be made).
    Setup(String),
    /// QEMU started but a port did not come up: the run ends as failed, with QEMU's own words.
    Failed(String),
}

/// What the VM costs the machine it runs on, sampled from QEMU's own process.
#[derive(Clone)]
pub struct Stats {
    /// Percent of one core (it can pass 100).
    pub cpu: f32,
    pub mem_mb: u64,
    pub cpus: u32,
}

/// A running VM.
pub enum Vm {
    Local(Box<LocalVm>),
    /// On another computer (`nodes/client.rs`).
    Remote(crate::nodes::client::RemoteVm),
}

impl Vm {
    /// Resolves when QEMU has exited, with how (`None`: unknown). Cancel safe.
    pub async fn wait(&mut self) -> Option<String> {
        match self {
            Vm::Local(v) => v.child.wait().await.ok().map(|s| s.to_string()),
            Vm::Remote(v) => v.wait().await,
        }
    }

    pub fn stats(&mut self) -> Option<Stats> {
        match self {
            Vm::Local(v) => v.stats(),
            Vm::Remote(v) => v.stats(),
        }
    }

    /// The end of what QEMU wrote to its error output.
    pub fn err_tail(&self) -> String {
        match self {
            Vm::Local(v) => tail(&v.errlog),
            Vm::Remote(v) => v.err_tail(),
        }
    }

    /// Stops the VM and removes everything it made except the work disk.
    pub async fn end(self) {
        match self {
            Vm::Local(mut v) => {
                let _ = v.child.kill().await;
                crate::util::remove_dir_retry(&v.work).await;
            }
            Vm::Remote(v) => v.end().await,
        }
    }
}

pub struct LocalVm {
    child: tokio::process::Child,
    errlog: PathBuf,
    work: PathBuf,
    sys: sysinfo::System,
    pid: Option<sysinfo::Pid>,
    cpus: u32,
    /// The sockets' private folders go when these do.
    _ports: Vec<Serial>,
}

impl LocalVm {
    fn stats(&mut self) -> Option<Stats> {
        let pid = self.pid?;
        self.sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::Some(&[pid]), true, sysinfo::ProcessRefreshKind::nothing().with_cpu().with_memory());
        let p = self.sys.process(pid)?;
        Some(Stats { cpu: p.cpu_usage(), mem_mb: p.memory() / (1024 * 1024), cpus: self.cpus })
    }
}

fn tail(path: &Path) -> String {
    let text = std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let text = text.trim();
    let start = text.len().saturating_sub(1000);
    let start = (start..=text.len()).find(|i| text.is_char_boundary(*i)).unwrap_or(text.len());
    text[start..].to_string()
}

/// Starts the VM of a run where the scheduler put it: here, or on a node.
pub async fn launch_on(app: &AppHandle, target: &crate::nodes::Target, spec: LaunchSpec, progress: &mut (dyn FnMut(&str) + Send)) -> Result<Launched, LaunchError> {
    match target {
        crate::nodes::Target::Local => launch(app, spec, progress).await,
        crate::nodes::Target::Node(n) => crate::nodes::client::launch(n, spec, progress).await,
    }
}

/// Starts the VM of a run on this computer. `progress` is told `starting` once QEMU is spawned.
pub async fn launch(app: &AppHandle, spec: LaunchSpec, progress: &mut (dyn FnMut(&str) + Send)) -> Result<Launched, LaunchError> {
    let work = crate::util::data_dir(app).map_err(LaunchError::Setup)?.join("runs").join("tmp").join(&spec.name);
    std::fs::create_dir_all(&work).map_err(|e| LaunchError::Setup(e.to_string()))?;
    let res = start(app, &work, &spec, progress).await;
    if res.is_err() {
        crate::util::remove_dir_retry(&work).await;
    }
    res
}

async fn start(app: &AppHandle, work: &Path, spec: &LaunchSpec, progress: &mut (dyn FnMut(&str) + Send)) -> Result<Launched, LaunchError> {
    use LaunchError::{Failed, Setup};
    let overlay = work.join("disk.qcow2");
    warm_cache(app, &spec.disk);
    // the overlay is copied while the agent's own disk is made (empty the first time: the guest formats a blank one)
    let overlay_job = make_overlay(app, &spec.disk, &overlay);
    let disk_job = async {
        let Some(d) = &spec.work_disk else { return Ok(()) };
        if d.path.exists() {
            return Ok(());
        }
        if let Some(dir) = d.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("work disk: {e}"))?;
        }
        crate::vm::machine::qemu_img(app, &["create", "-q", "-f", "qcow2", &d.path.to_string_lossy(), &format!("{}M", d.size_mb)]).await
    };
    let (overlay_made, disk_made) = tokio::join!(overlay_job, disk_job);
    overlay_made.map_err(Setup)?;
    disk_made.map_err(Setup)?;

    let channel = Serial::named(work, "agent");
    let net_port = Serial::named(work, "net");
    let shared_port = spec.shared.then(|| Serial::named(work, "shared"));
    let errlog = work.join("qemu.err");
    let mut cmd = crate::vm::machine::machine(app, spec.memory_mb, spec.cpus, true).map_err(Setup)?;
    cmd.args(AGENT_NIC).args(["-action", "panic=exit-failure", "-serial", "null"]).arg("-drive").arg(crate::vm::machine::drive_arg(&overlay));
    if let Some(d) = &spec.work_disk {
        // second virtio disk = /dev/vdb; it must survive the VM, so QEMU flushes when the guest asks
        cmd.arg("-drive").arg(crate::vm::machine::persistent_drive_arg(&d.path));
    }
    cmd.args(["-device", "virtio-serial-pci"])
        .arg("-chardev")
        .arg(format!("{},id=agent", channel.chardev))
        .args(["-device", "virtserialport,chardev=agent,name=agent"])
        .arg("-chardev")
        .arg(format!("{},id=net", net_port.chardev))
        .args(["-device", "virtserialport,chardev=net,name=net"]);
    if let Some(p) = &shared_port {
        // the shared folder: the guest runs 9P over this port, the app answers (`shared.rs`)
        cmd.arg("-chardev").arg(format!("{},id=shared", p.chardev)).args(["-device", "virtserialport,chardev=shared,name=shared"]);
    }
    // An agent VM's QEMU connects to nothing over TCP (it has no network card; both ports are
    // sockets it serves); it needs the guest's memory and some headroom, and, as a backstop for a
    // VM that outlives the app, no more CPU time than every core for the whole deadline.
    let cores = spec.cpus.unwrap_or_else(host_cpus);
    let vm_limits = crate::vm::confine::Limits {
        connect_ports: Some(vec![]),
        mem_mb: Some(u64::from(spec.memory_mb)),
        cpu_secs: Some(spec.timeout_secs.saturating_add(spec.grace_secs).saturating_mul(u64::from(cores) + 1).saturating_add(120)),
    };
    // the work disk is written, so its folder is writable (the plan's `disks` are read only)
    let mut disks = vec![overlay.as_path()];
    let mut writable = vec![work];
    writable.extend([&channel, &net_port].into_iter().chain(shared_port.as_ref()).filter_map(|s| s.short_dir()));
    if let Some(d) = &spec.work_disk {
        disks.push(d.path.as_path());
        writable.extend(d.path.parent());
    }
    crate::vm::machine::confine(app, &mut cmd, &disks, &writable, vm_limits);
    let mut cmd = Command::from(cmd);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).kill_on_drop(true);
    cmd.stderr(std::fs::File::create(&errlog).map(Stdio::from).unwrap_or_else(|_| Stdio::null()));
    let mut child = cmd.spawn().map_err(|e| Setup(super::qemu::spawn_error(app, &format!("qemu-system-{}", crate::vm::machine::arch().unwrap_or("?")), &e)))?;
    crate::vm::machine::confine_started(app, &child);
    progress("starting");

    // The agent channel is up first, so a failure on a later port ends the run like one on the channel does.
    let mut ports = vec![("the agent channel", &channel), ("the network port", &net_port)];
    if let Some(p) = &shared_port {
        ports.push(("the shared folder port", p));
    }
    let mut streams = Vec::new();
    for (what, port) in ports {
        match within(30, what, port.connect()).await {
            Ok(s) => streams.push(s),
            Err(e) => {
                let _ = child.kill().await;
                return Err(Failed(format!("{e}: {}", tail(&errlog))));
            }
        }
    }
    let mut streams = streams.into_iter();
    let (agent, net, shared) = (streams.next().unwrap(), streams.next().unwrap(), streams.next());
    let pid = child.id().map(sysinfo::Pid::from_u32);
    let mut _ports = vec![channel, net_port];
    _ports.extend(shared_port);
    let vm = LocalVm { child, errlog, work: work.to_path_buf(), sys: sysinfo::System::new(), pid, cpus: cores, _ports };
    Ok(Launched { agent, net, shared, vm: Vm::Local(Box::new(vm)) })
}

fn host_cpus() -> u32 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
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
    let dir = crate::util::data_dir(app)?.join("runs").join("overlays");
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
        let tmp = dir.join(format!("{prefix}.{}.tmp", crate::vm::runs::new_run_id()));
        let (base, top) = (disk.to_string_lossy(), tmp.to_string_lossy());
        crate::vm::machine::qemu_img(app, &["create", "-q", "-f", "qcow2", "-b", &base, "-F", "qcow2", &top]).await?;
        std::fs::rename(&tmp, &template).map_err(|e| e.to_string())?;
    }
    // std's copy clones the file where the file system can (reflink, copy_file_range, clonefile);
    // either way it blocks, so it runs off the async executor
    let (from, to) = (template, overlay.to_path_buf());
    tokio::task::spawn_blocking(move || std::fs::copy(from, to).map(|_| ()))
        .await
        .map_err(|e| format!("overlay: {e}"))?
        .map_err(|e| format!("overlay: {e}"))
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
