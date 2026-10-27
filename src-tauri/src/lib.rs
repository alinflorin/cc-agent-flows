use tauri::Manager;

mod broker;
mod idp;
mod nats;
mod qemu;
mod secrets;

/// Whether the updater can replace this installation. On Linux only an AppImage can;
/// a .deb or .rpm belongs to the package manager.
#[tauri::command]
fn can_self_update() -> bool {
    !cfg!(target_os = "linux") || std::env::var_os("APPIMAGE").is_some()
}

/// A GUI launch inherits a minimal PATH. Ask the user's login shell for the one it would give a
/// terminal, so tools installed by rc files (nvm, npm globals, cargo) are found.
#[cfg(unix)]
fn adopt_shell_path() {
    use std::process::{Command, Stdio};
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = Command::new(shell)
            .args(["-ilc", "printf '<<%s>>' \"$PATH\""])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        let _ = tx.send(out);
    });
    let Ok(Ok(out)) = rx.recv_timeout(std::time::Duration::from_secs(5)) else {
        return;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(shell_path) = text.rsplit_once("<<").and_then(|(_, r)| r.split_once(">>")) else {
        return;
    };
    let mut dirs: Vec<std::path::PathBuf> = std::env::split_paths(shell_path.0).collect();
    if let Some(own) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&own) {
            if !dirs.contains(&d) {
                dirs.push(d);
            }
        }
    }
    if let Ok(joined) = std::env::join_paths(dirs) {
        // SAFETY: runs first thing in `run`, before any other thread exists
        unsafe { std::env::set_var("PATH", joined) };
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(unix)]
    adopt_shell_path();
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::new().build());

    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init());

    builder
        .invoke_handler(tauri::generate_handler![
            can_self_update,
            nats::nats_version,
            broker::broker_status,
            broker::broker_restart,
            qemu::qemu_version,
            secrets::claude_token_set,
            secrets::claude_token_status,
            secrets::claude_token_clear
        ])
        .setup(|app| {
            app.manage(broker::State::default());
            // Started in the background: a missing nats-server must not keep the app from opening.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<broker::State>();
                state.start(&handle).await;
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Dropping the broker kills nats-server.
            if let tauri::RunEvent::Exit = event {
                app.state::<broker::State>().stop();
            }
        });
}
