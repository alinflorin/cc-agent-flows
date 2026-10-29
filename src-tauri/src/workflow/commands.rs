//! The Tauri commands of the workflow editor and the session list.

use std::path::PathBuf;

use serde::Serialize;

use super::model::*;
use super::store::*;

/// The directory of a project, where its workflows and sessions live.
pub fn root(app: &tauri::AppHandle, project: &str) -> Result<PathBuf, String> {
    crate::config::scope::Scope::project(project).dir(app)
}

/// The standard workflow, if the project has no workflow yet (ones the user made are left alone).
pub fn provision_defaults(app: &tauri::AppHandle, project: &str) -> Result<(), String> {
    let root = root(app, project)?;
    if workflows(&root).is_empty() {
        let demo = crate::demo::workflow();
        save_workflow(&root, &demo.id, &demo.name, &demo.workflow, &Instructions::default())?;
    }
    Ok(())
}

#[derive(Serialize)]
pub struct WorkflowView {
    pub id: String,
    pub name: String,
    pub workflow: Workflow,
    pub problems: Vec<Problem>,
    pub system_prompt: String,
}

fn view(app: &tauri::AppHandle, project: &str, id: String, name: String, wf: Workflow, system_prompt: String) -> Result<WorkflowView, String> {
    let ids: Vec<String> = crate::config::profiles::available(app, Some(project)).into_iter().map(|e| e.id).collect();
    Ok(WorkflowView { problems: wf.problems(&ids), id, name, workflow: wf, system_prompt })
}

#[tauri::command]
pub fn workflows_list(app: tauri::AppHandle, project: String) -> Result<Vec<WorkflowEntry>, String> {
    Ok(workflows(&root(&app, &project)?))
}

#[tauri::command]
pub fn workflow_get(app: tauri::AppHandle, project: String, id: String) -> Result<WorkflowView, String> {
    let root = root(&app, &project)?;
    let (name, wf, ins) = load_workflow(&root, &id).ok_or("no such workflow")?;
    view(&app, &project, id, name, wf, ins.system_prompt)
}

/// Saves a workflow (an empty `id` creates one); returns it with its (soft) problems.
#[tauri::command]
pub fn workflow_save(app: tauri::AppHandle, project: String, id: String, name: String, workflow: Workflow, system_prompt: String) -> Result<WorkflowView, String> {
    let id = save_workflow(&root(&app, &project)?, &id, &name, &workflow, &Instructions { system_prompt: system_prompt.clone() })?;
    view(&app, &project, id, name.trim().into(), workflow, system_prompt)
}

#[tauri::command]
pub fn workflow_delete(app: tauri::AppHandle, project: String, id: String) -> Result<(), String> {
    delete_workflow(&root(&app, &project)?, &id)
}

/// The standard workflow, not saved (for a new workflow or to replace one's content).
#[tauri::command]
pub fn workflow_standard(app: tauri::AppHandle, project: String) -> Result<WorkflowView, String> {
    view(&app, &project, String::new(), String::new(), standard_workflow(), String::new())
}

#[tauri::command]
pub fn sessions_list(app: tauri::AppHandle, project: String) -> Result<Vec<SessionEntry>, String> {
    Ok(sessions(&root(&app, &project)?))
}

#[tauri::command]
pub fn sessions_create(app: tauri::AppHandle, project: String, workflow: Option<String>) -> Result<String, String> {
    let root = root(&app, &project)?;
    let workflow = match workflow {
        Some(w) => w,
        None => workflows(&root).into_iter().next().map(|w| w.id).ok_or("there is no workflow")?,
    };
    create_session(&root, "", "", &workflow)
}

/// Picks the workflow of a session that has not started.
#[tauri::command]
pub fn sessions_workflow(app: tauri::AppHandle, project: String, id: String, workflow: String) -> Result<(), String> {
    set_session_workflow(&root(&app, &project)?, &id, &workflow)
}

/// Renames a session; an empty name makes it untitled again.
#[tauri::command]
pub fn sessions_rename(app: tauri::AppHandle, project: String, id: String, name: String) -> Result<(), String> {
    edit_session(&root(&app, &project)?, &id, Some(&name), None)
}

/// Whether the agents may ask the user (`ask`) or must decide themselves (`never`).
#[tauri::command]
pub fn sessions_autonomy(app: tauri::AppHandle, project: String, id: String, autonomy: String) -> Result<(), String> {
    set_session_autonomy(&root(&app, &project)?, &id, &autonomy)
}

#[tauri::command]
pub fn sessions_delete(app: tauri::AppHandle, project: String, id: String) -> Result<(), String> {
    delete_session(&root(&app, &project)?, &id)
}
