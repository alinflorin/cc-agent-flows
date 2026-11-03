//! The session's shared folder as the user sees it (the "Files" tab): list, download, upload,
//! new folder, delete. The folder is the one the agents mount at `/home/agent/shared`
//! (`vm/shared.rs`); what an agent wrote is untrusted, so there is no "open": a file is saved
//! somewhere the user chooses, never run. Uploads and deletes go through the same quota the
//! agents' servers use.

use std::path::PathBuf;

use tauri_plugin_dialog::DialogExt;

use super::commands::root;
use super::store::{session_file, shared_dir};
use crate::vm::shared::{self, FileEntry};

/// The folder of a session that exists.
fn folder(app: &tauri::AppHandle, project: &str, id: &str) -> Result<PathBuf, String> {
    let root = root(app, project)?;
    if !session_file(&root, id, "json")?.is_file() {
        return Err("no such session".into());
    }
    shared_dir(&root, id)
}

fn quota(app: &tauri::AppHandle, dir: &std::path::Path) -> std::sync::Arc<shared::Quota> {
    shared::quota_for(dir, u64::from(crate::config::vm_settings::current(app).shared_quota_mb) * 1024 * 1024)
}

#[tauri::command]
pub fn shared_list(app: tauri::AppHandle, project: String, id: String, path: String) -> Result<Vec<FileEntry>, String> {
    shared::list(&folder(&app, &project, &id)?, &path)
}

#[tauri::command]
pub fn shared_mkdir(app: tauri::AppHandle, project: String, id: String, path: String, name: String) -> Result<(), String> {
    let dir = folder(&app, &project, &id)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    shared::make_dir(&dir, &path, &name)
}

#[tauri::command]
pub fn shared_delete(app: tauri::AppHandle, project: String, id: String, path: String) -> Result<(), String> {
    let dir = folder(&app, &project, &id)?;
    shared::remove(&dir, &path, &quota(&app, &dir))
}

/// Asks for files and copies them into the folder `path`. Returns how many were copied (0 if the
/// dialog was cancelled); the first problem is an error, after the files before it were copied.
#[tauri::command]
pub async fn shared_upload(app: tauri::AppHandle, project: String, id: String, path: String) -> Result<usize, String> {
    let dir = folder(&app, &project, &id)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dialog_app = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || dialog_app.dialog().file().blocking_pick_files()).await.map_err(|e| e.to_string())?;
    let Some(files) = picked else { return Ok(0) };
    tauri::async_runtime::spawn_blocking(move || {
        let q = quota(&app, &dir);
        let mut n = 0;
        for f in files {
            let src = f.into_path().map_err(|e| e.to_string())?;
            shared::import_file(&dir, &path, &src, &q)?;
            n += 1;
        }
        Ok(n)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Asks where to put a file (or, for a folder, which folder to copy it into) and copies it there.
/// `false` if the dialog was cancelled.
#[tauri::command]
pub async fn shared_download(app: tauri::AppHandle, project: String, id: String, path: String, dir: bool) -> Result<bool, String> {
    let root = folder(&app, &project, &id)?;
    let name = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("shared").to_string();
    let dialog_app = app.clone();
    let chosen = tauri::async_runtime::spawn_blocking(move || {
        let d = dialog_app.dialog().file();
        if dir { d.blocking_pick_folder() } else { d.set_file_name(&name).blocking_save_file() }
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(dest) = chosen else { return Ok(false) };
    let dest = dest.into_path().map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || shared::export(&root, &path, &dest)).await.map_err(|e| e.to_string())??;
    Ok(true)
}

/// Shows the folder in the system's file manager.
#[tauri::command]
pub fn shared_reveal(app: tauri::AppHandle, project: String, id: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = folder(&app, &project, &id)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener().open_path(dir.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}
