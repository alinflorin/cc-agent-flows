mod agent;
mod builder;
mod egress;
mod engine;
mod images;
mod injections;
mod inspect;
mod network;
#[cfg(test)]
mod agentd_e2e;
mod profiles;
mod workflow;
mod protocol;
mod qemu;
mod runs;
mod secrets;
mod variables;
mod tunnel;

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
            qemu::qemu_version,
            images::images_list,
            images::images_save,
            images::images_delete,
            images::images_build,
            images::images_reset_demo,
            network::network_get,
            network::network_set,
            profiles::provision_starters,
            profiles::profiles_list,
            profiles::profiles_save,
            profiles::profiles_delete,
            profiles::profiles_reset_demo,
            workflow::workflow_get,
            workflow::workflow_save,
            workflow::workflow_standard,
            workflow::sessions_list,
            workflow::sessions_create,
            workflow::sessions_rename,
            workflow::sessions_autonomy,
            workflow::sessions_delete,
            engine::session_start,
            engine::session_continue,
            engine::session_cancel,
            engine::session_reply,
            engine::session_get,
            engine::session_log,
            runs::agent_run,
            runs::agent_run_cancel,
            secrets::claude_token_set,
            secrets::claude_token_status,
            secrets::claude_token_clear,
            secrets::secrets_list,
            secrets::secrets_set,
            secrets::secrets_delete,
            secrets::secrets_reset_demo,
            variables::variables_list,
            variables::variables_set,
            variables::variables_delete,
            variables::variables_reset_demo,
            injections::injections_get,
            injections::injections_save,
            injections::injections_reset_demo
        ])
        .setup(|app| {
            use tauri::Manager;
            app.manage(images::Images::default());
            app.manage(runs::Runs::default());
            app.manage(engine::Sessions::default());
            engine::recover(app.handle());
            images::ensure_base(app.handle());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_, _| {});
}
