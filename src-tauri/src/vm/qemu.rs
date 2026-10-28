use std::path::PathBuf;
use std::process::Command;
use tauri::AppHandle;

/// The bundled QEMU tree (`bin/`, `lib/`, `share/qemu/`), if this build shipped one.
fn bundled_root(app: &AppHandle) -> Option<PathBuf> {
    let root = crate::util::resource_dir(app).ok()?.join("qemu");
    root.join("bin").is_dir().then_some(root)
}

/// A command for a QEMU program (`qemu-system-x86_64`, `qemu-img`, ...). Uses the bundled copy,
/// falling back to whatever is on PATH when the app was built without one.
pub fn command(app: &AppHandle, program: &str) -> Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut cmd = match bundled_root(app) {
        Some(root) => {
            let exe = root
                .join("bin")
                .join(format!("{program}{}", std::env::consts::EXE_SUFFIX));
            let mut cmd = Command::new(exe);
            if program.starts_with("qemu-system-") {
                cmd.arg("-L").arg(root.join("share").join("qemu"));
            }
            cmd
        }
        None => Command::new(program),
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Why a QEMU program could not start. "File not found" usually means the app has no bundled QEMU
/// (the `qemu` folder is missing from its resources) and none is on PATH either.
pub fn spawn_error(app: &AppHandle, program: &str, e: &std::io::Error) -> String {
    if e.kind() != std::io::ErrorKind::NotFound {
        return format!("cannot start {program}: {e}");
    }
    match bundled_root(app) {
        Some(root) => format!("cannot start {program}: {e} (looked in {})", root.join("bin").display()),
        None => format!("cannot start {program}: {e} (this app has no bundled QEMU and none is on PATH)"),
    }
}

/// First line of `qemu-system-<arch> --version`, to check the bundled QEMU runs.
#[tauri::command]
pub async fn qemu_version(app: AppHandle, arch: String) -> Result<String, String> {
    if !matches!(arch.as_str(), "x86_64" | "aarch64") {
        return Err(format!("unsupported architecture: {arch}"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let out = command(&app, &format!("qemu-system-{arch}"))
            .arg("--version")
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        let text = String::from_utf8_lossy(&out.stdout);
        Ok(text.lines().next().unwrap_or_default().to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
