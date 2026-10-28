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

fn qemu(app: &AppHandle) -> Check {
    let arch = if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" };
    match crate::vm::qemu::command(app, &format!("qemu-system-{arch}")).arg("--version").output() {
        Ok(o) if o.status.success() => check("qemu", "ok", String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or_default()),
        Ok(o) => check("qemu", "error", String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => check("qemu", "error", e.to_string()),
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
    match crate::vm::images::images_list(app.clone()).into_iter().find(|i| i.id == "base") {
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
