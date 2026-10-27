use std::path::PathBuf;
use std::process::Command;
use tauri::{AppHandle, Manager};

/// The bundled `nats-server`, if this build shipped one.
fn bundled(app: &AppHandle) -> Option<PathBuf> {
    let exe = app
        .path()
        .resource_dir()
        .ok()?
        .join("nats")
        .join(format!("nats-server{}", std::env::consts::EXE_SUFFIX));
    exe.is_file().then_some(exe)
}

/// A command for `nats-server`. Uses the bundled copy, falling back to PATH when the app was
/// built without one. Spawn the broker through it.
#[allow(dead_code)]
pub fn command(app: &AppHandle) -> Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut cmd = Command::new(bundled(app).unwrap_or_else(|| "nats-server".into()));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// `nats-server --version`, to check the bundled broker runs.
#[tauri::command]
pub async fn nats_version(app: AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let out = command(&app).arg("--version").output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
