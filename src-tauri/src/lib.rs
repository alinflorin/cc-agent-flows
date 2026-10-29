mod config;
mod demo;
mod net;
mod system;
mod transfer;
mod util;
mod vm;
mod workflow;

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
        .plugin(
            tauri_plugin_log::Builder::new()
                .clear_targets()
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir { file_name: Some("app".into()) }),
                ])
                .max_file_size(2_000_000)
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepOne)
                .level(log::LevelFilter::Info)
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::new().build());

    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init());

    builder
        .invoke_handler(tauri::generate_handler![
            can_self_update,
            config::vm_settings::vm_settings_get,
            config::vm_settings::vm_settings_set,
            transfer::transfer_export,
            transfer::transfer_import,
            system::app_log_open,
            vm::qemu::qemu_version,
            system::system_status,
            vm::images::images_list,
            vm::images::images_save,
            vm::images::images_delete,
            vm::images::images_build,
            vm::images::images_reset_demo,
            config::network::network_get,
            config::network::network_set,
            config::projects::projects_list,
            config::projects::projects_get,
            config::projects::projects_save,
            config::projects::projects_delete,
            demo::provision_starters,
            config::profiles::profiles_list,
            config::profiles::profiles_save,
            config::profiles::profiles_delete,
            config::profiles::profiles_reset_demo,
            workflow::commands::workflows_list,
            workflow::commands::workflow_get,
            workflow::commands::workflow_delete,
            workflow::commands::workflow_save,
            workflow::commands::workflow_standard,
            workflow::commands::sessions_list,
            workflow::commands::sessions_create,
            workflow::commands::sessions_rename,
            workflow::commands::sessions_autonomy,
            workflow::commands::sessions_workflow,
            workflow::commands::sessions_delete,
            workflow::engine::session_start,
            workflow::engine::session_continue,
            workflow::engine::session_cancel,
            workflow::engine::session_reply,
            workflow::engine::session_message,
            workflow::analytics::analytics_get,
            workflow::triggers::triggers_status,
            workflow::triggers::trigger_fire,
            workflow::triggers::cron_preview,
            workflow::webhook::webhook_get,
            workflow::webhook::webhook_set,
            workflow::engine::session_get,
            workflow::engine::session_log,
            vm::runs::agent_run,
            vm::runs::agent_run_cancel,
            config::accounts::accounts_list,
            config::accounts::accounts_add,
            config::accounts::accounts_set_token,
            config::accounts::accounts_update,
            config::accounts::accounts_wake,
            config::accounts::accounts_remove,
            config::accounts::accounts_mode,
            config::accounts::accounts_set_mode,
            config::secrets::secrets_list,
            config::secrets::secrets_set,
            config::secrets::secrets_delete,
            config::secrets::secrets_reset_demo,
            config::variables::variables_list,
            config::variables::variables_set,
            config::variables::variables_delete,
            config::variables::variables_reset_demo,
            config::injections::injections_get,
            config::injections::injections_save,
            config::injections::injections_reset_demo,
            config::integrations::integrations_list,
            config::integrations::integrations_save,
            config::integrations::integrations_delete
        ])
        .setup(|app| {
            use tauri::Manager;
            app.manage(vm::images::Images::default());
            app.manage(vm::runs::Runs::default());
            app.manage(workflow::engine::Sessions::default());
            app.manage(workflow::engine::VmSlots::new(app.handle()));
            workflow::engine::recover(app.handle());
            vm::images::ensure_base(app.handle());
            app.manage(workflow::webhook::Webhook::default());
            workflow::triggers::start(app.handle());
            workflow::webhook::start(app.handle());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_, _| {});
}
