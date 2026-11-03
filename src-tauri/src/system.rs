//! What the About page shows as "system readiness": can this machine run agents? Each check says
//! `ok`, `warn` (works, but not as it should), `error` (agents cannot run) or `info`.

use serde::Serialize;
use tauri::AppHandle;

#[derive(Serialize)]
pub struct Check {
    pub id: &'static str,
    pub level: &'static str,
    pub detail: String,
}

fn check(id: &'static str, level: &'static str, detail: impl Into<String>) -> Check {
    Check { id, level, detail: detail.into() }
}

/// Runs a command and waits at most `secs` for it. A process that never exits (one stuck in the
/// kernel cannot even be killed) must not hold the About page: it is given up on, not waited for.
fn output_within(mut cmd: std::process::Command, secs: u64) -> Result<std::process::Output, String> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(s) => break s,
            None if started.elapsed().as_secs() >= secs => {
                let _ = child.kill();
                // no wait(): that is what would hang
                return Err(format!("no answer after {secs} s"));
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    };
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    if let Some(mut o) = child.stdout.take() {
        let _ = o.read_to_end(&mut stdout);
    }
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_end(&mut stderr);
    }
    Ok(std::process::Output { status, stdout, stderr })
}

/// When `qemu --version` last got no answer: it is not started again for a while, each try would
/// leave one more stuck process behind.
static QEMU_STUCK: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
const QEMU_RETRY_SECS: u64 = 300;

fn qemu(app: &AppHandle) -> Check {
    if QEMU_STUCK.lock().unwrap().is_some_and(|t| t.elapsed().as_secs() < QEMU_RETRY_SECS) {
        return check("qemu", "error", "qemu did not answer a moment ago and that process is still stuck (restarting the computer usually clears it); not trying again for a few minutes");
    }
    let arch = if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" };
    let mut cmd = crate::vm::qemu::command(app, &format!("qemu-system-{arch}"));
    cmd.arg("--version");
    match output_within(cmd, 8) {
        Ok(o) if o.status.success() => check("qemu", "ok", String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or_default()),
        Ok(o) => check("qemu", "error", String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => {
            if e.starts_with("no answer") {
                *QEMU_STUCK.lock().unwrap() = Some(std::time::Instant::now());
            }
            check("qemu", "error", e)
        }
    }
}

/// Hardware acceleration: without it VMs fall back to emulation, which works but is very slow.
fn accel() -> Check {
    #[cfg(target_os = "linux")]
    {
        match std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm") {
            Ok(_) => check("accel", "ok", "KVM"),
            Err(e) => check("accel", "warn", format!("/dev/kvm: {e}")),
        }
    }
    #[cfg(target_os = "macos")]
    {
        let ok = std::process::Command::new("sysctl").args(["-n", "kern.hv_support"]).output().is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "1");
        if ok { check("accel", "ok", "Hypervisor.framework") } else { check("accel", "warn", "Hypervisor.framework is not available") }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        check("accel", "info", "Windows Hypervisor Platform (checked when a VM starts)")
    }
}

fn image(app: &AppHandle) -> Check {
    match crate::vm::images::images_list(app.clone(), None).unwrap_or_default().into_iter().find(|i| i.id == "base") {
        Some(i) if i.state == "ready" => check("image", "ok", ""),
        Some(i) if i.state == "stale" => check("image", "warn", "stale"),
        Some(i) => check("image", if i.state == "missing" && i.error.is_some() { "error" } else { "warn" }, i.error.unwrap_or_else(|| i.state.to_string())),
        None => check("image", "error", "missing"),
    }
}

fn token(app: &AppHandle) -> Check {
    check("token", if crate::config::accounts::usable(app) { "ok" } else { "warn" }, "")
}

/// The global allowlist must let the agent reach the Claude API.
fn network(app: &AppHandle) -> Check {
    let rules = crate::net::egress::Rules::new([crate::config::network::current(app).allow_hosts]);
    if rules.allows("api.anthropic.com") { check("network", "ok", "") } else { check("network", "error", "api.anthropic.com") }
}

async fn tunnel() -> Check {
    match crate::net::tunnel::self_check().await {
        Ok(addr) => check("tunnel", "ok", addr),
        Err(e) => check("tunnel", "error", e),
    }
}

#[tauri::command]
pub async fn system_status(app: AppHandle) -> Vec<Check> {
    let blocking = {
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || vec![qemu(&app), accel(), image(&app), token(&app), network(&app)])
    };
    let mut out = blocking.await.unwrap_or_default();
    out.insert(3, tunnel().await);
    out
}

/// Opens the app's log file (`app.log` in the log directory) with the system's default program.
#[tauri::command]
pub fn app_log_open(app: AppHandle) -> Result<(), String> {
    use tauri::Manager;
    use tauri_plugin_opener::OpenerExt;
    let path = app.path().app_log_dir().map_err(|e| e.to_string())?.join("app.log");
    if !path.exists() {
        return Err(format!("no log yet: {}", path.display()));
    }
    app.opener().open_path(path.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}

/// Whether the OS starts the app when the user logs in (off until the user turns it on; the OS
/// holds the state, not the app's preferences, so it stays true to what the system will do).
#[tauri::command]
pub fn autostart_get(app: tauri::AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn autostart_set(app: tauri::AppHandle, enabled: bool) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    let launch = app.autolaunch();
    if enabled { launch.enable() } else { launch.disable() }.map_err(|e| e.to_string())?;
    launch.is_enabled().map_err(|e| e.to_string())
}


#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_command_that_never_ends_is_given_up_on() {
        let mut slow = std::process::Command::new("sleep");
        slow.arg("30");
        let started = std::time::Instant::now();
        assert!(output_within(slow, 1).unwrap_err().starts_with("no answer"));
        assert!(started.elapsed().as_secs() < 5);
        let mut quick = std::process::Command::new("echo");
        quick.arg("hi");
        assert_eq!(String::from_utf8_lossy(&output_within(quick, 5).unwrap().stdout).trim(), "hi");
    }
}
