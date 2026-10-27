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
use crate::protocol::{Bootstrap, Final, Outbound, Status, Task, TaskEnv};

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
    pub task: Task,
}

/// Boots a VM of `image`, runs `task` in it and returns how it ended. `on_msg` sees every event and
/// the final (also a synthesized one), in order. `Err` only for what stops the VM from starting
/// (no token, image not built, QEMU missing). `cancel` firing ends the run as failed.
pub async fn run_vm(app: &AppHandle, spec: Spec, cancel: oneshot::Receiver<()>, mut on_msg: impl FnMut(&Outbound)) -> Result<Final, String> {
    let Spec { run_id, image, identity, cpus, memory_mb, task } = spec;
    let token = crate::secrets::claude_token().ok_or("no Claude token is set (Settings > LLM)")?;
    let disk = crate::images::path(app, &image).ok_or_else(|| format!("the image {image} is not built"))?;
    let work = app.path().app_data_dir().map_err(|e| e.to_string())?.join("runs").join("tmp").join(&run_id);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = boot_and_drive(app, &work, &disk, (cpus, memory_mb), &identity, &token, task, cancel, &mut on_msg).await;
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
    task: Task,
    mut cancel: oneshot::Receiver<()>,
    on_msg: &mut impl FnMut(&Outbound),
) -> Result<Final, String> {
    let overlay = work.join("disk.qcow2");
    // Never boot the image itself: a throwaway overlay on top of it.
    crate::images::qemu_img(
        app,
        &["create", "-q", "-f", "qcow2", "-b", &disk.to_string_lossy(), "-F", "qcow2", &overlay.to_string_lossy()],
    )
    .await?;

    let channel = Serial::named(work, "agent");
    let errlog = work.join("qemu.err");
    let timeout_secs = task.timeout_secs;
    let mut cmd = crate::images::machine(app, size.1, size.0)?;
    cmd.args(["-action", "panic=exit-failure", "-serial", "null"])
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

    let stream = match crate::builder::within(30, "the agent channel", channel.connect()).await {
        Ok(s) => s,
        Err(e) => {
            let _ = child.kill().await;
            return Ok(failed(format!("{e}: {}", tail(&errlog))));
        }
    };
    // The stream stays open inside `drive` until the run is over.
    let mut rx = crate::agent::drive(stream, Bootstrap { id: identity.to_string(), claude_token: token.to_string() }, task);

    let deadline = tokio::time::sleep(Duration::from_secs(timeout_secs.saturating_add(GRACE_SECS)));
    tokio::pin!(deadline);
    let mut exit = None;
    let fin = loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Some(Outbound::Final(f)) => break f,
                Some(m) => on_msg(&m),
                None => {
                    // The stream ended: QEMU is gone (or about to be) and the agent never finished.
                    let status = match exit {
                        Some(s) => s,
                        None => tokio::time::timeout(Duration::from_secs(5), child.wait()).await.ok().and_then(|r| r.ok()),
                    };
                    break failed(format!("the VM ended without a result ({}): {}", status.map_or("unknown exit".into(), |s| s.to_string()), tail(&errlog)));
                }
            },
            status = child.wait(), if exit.is_none() => {
                // Keep reading: the final may still be on its way through the channel.
                exit = Some(status.ok());
            }
            _ = &mut deadline => break failed(format!("the run did not end within {}s, the VM was killed", timeout_secs.saturating_add(GRACE_SECS))),
            _ = &mut cancel => break failed("cancelled"),
        }
    };
    let _ = child.kill().await;
    Ok(fin)
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
    let task = Task {
        prompt,
        model: entry.profile.model.clone(),
        effort: entry.profile.effort.clone(),
        session_id: uuid_v4(),
        resume: false,
        schema: crate::protocol::RESULT_SCHEMA.into(),
        system_prompt: Some(entry.profile.system_prompt.clone()).filter(|s| !s.trim().is_empty()),
        env: entry.profile.env.iter().map(|v| TaskEnv { name: v.name.clone(), value: v.value.clone() }).collect(),
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
        let spec = Spec { run_id: id.clone(), image: entry.profile.image.clone(), identity: who, cpus: entry.profile.cpus, memory_mb: entry.profile.memory_mb, task };
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
        let channel = Serial::named(&work, "agent");
        let mut vm = Command::new(q.join("bin/qemu-system-x86_64"))
            .arg("-L")
            .arg(q.join("share/qemu"))
            .args(["-accel", "kvm", "-cpu", "max", "-m", "2048", "-smp", "2", "-display", "none", "-monitor", "none"])
            .args(["-no-reboot", "-action", "panic=exit-failure", "-nic", "user", "-serial", "null"])
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
        let mut rx = crate::agent::drive(stream, Bootstrap { id: who, claude_token: "sk-ant-oat01-fake".into() }, task);
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
        let exited = tokio::time::timeout(Duration::from_secs(60), vm.wait()).await.expect("the VM did not power off").unwrap();
        eprintln!("qemu exit: {exited}");
        let _ = std::fs::remove_dir_all(work);
    }
}
