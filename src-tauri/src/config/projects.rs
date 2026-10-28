//! Projects: the unit the user works in. A project owns its workflows, sessions, agent profiles,
//! images, variables, secrets and injections, and adds its own allowed hosts and system prompt on
//! top of the global settings (Settings). Files live in `<app data>/projects/<id>/`: `project.json`
//! (this module) next to `workflows/`, `sessions/`, `profiles/`, `images/`, `variables.json`,
//! `secrets.json` and `injections.json`. Secrets values are in the OS credential store.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::scope::Scope;
use crate::util::{read_json, write_json};

pub const FILE: &str = "project.json";
const MAX_PROJECTS: usize = 200;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Added to the system prompt of every agent of the project, before the workflow's and the profile's.
    #[serde(default)]
    pub system_prompt: String,
    /// Hosts every agent of the project may reach (on top of the global ones), same format as the global list.
    #[serde(default)]
    pub allow_hosts: Vec<String>,
}

impl Project {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.name.len() > 80 {
            return Err("the name must be 1 to 80 characters".into());
        }
        if self.description.len() > 2000 {
            return Err("the description is too long".into());
        }
        if self.system_prompt.len() > 64 * 1024 {
            return Err("the system prompt is too large".into());
        }
        Ok(())
    }
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub fn dir_in(data: &Path, id: &str) -> PathBuf {
    data.join("projects").join(id)
}

fn data(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    crate::util::data_dir(app)
}

/// A project's settings, if it exists.
pub fn load(app: &tauri::AppHandle, id: &str) -> Option<Project> {
    if !valid_id(id) {
        return None;
    }
    read_json(&dir_in(&data(app).ok()?, id).join(FILE))
}

pub fn store(app: &tauri::AppHandle, id: &str, project: &Project) -> Result<(), String> {
    project.validate()?;
    if !valid_id(id) {
        return Err("bad project id".into());
    }
    write_json(&dir_in(&data(app)?, id).join(FILE), project)
}

/// The ids of all projects, sorted by name.
pub fn ids(app: &tauri::AppHandle) -> Vec<String> {
    list(app).into_iter().map(|e| e.id).collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub id: String,
    #[serde(flatten)]
    pub project: Project,
    pub workflows: usize,
    pub sessions: usize,
    pub profiles: usize,
}

fn count_json(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |rd| {
        rd.flatten()
            .filter(|e| {
                let p = e.path();
                p.extension().is_some_and(|x| x == "json") && p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| !s.contains('.'))
            })
            .count()
    })
}

pub fn list(app: &tauri::AppHandle) -> Vec<Entry> {
    let Ok(data) = data(app) else { return vec![] };
    let Ok(rd) = std::fs::read_dir(data.join("projects")) else { return vec![] };
    let mut out: Vec<Entry> = rd
        .flatten()
        .filter_map(|e| {
            let id = e.file_name().to_str()?.to_string();
            let dir = e.path();
            let project: Project = read_json(&dir.join(FILE))?;
            valid_id(&id).then(|| Entry {
                id,
                project,
                workflows: count_json(&dir.join("workflows")),
                sessions: count_json(&dir.join("sessions")),
                profiles: count_json(&dir.join("profiles")),
            })
        })
        .collect();
    out.sort_by(|a, b| a.project.name.to_lowercase().cmp(&b.project.name.to_lowercase()).then(a.id.cmp(&b.id)));
    out
}

#[tauri::command]
pub fn projects_list(app: tauri::AppHandle) -> Vec<Entry> {
    list(&app)
}

#[tauri::command]
pub fn projects_get(app: tauri::AppHandle, id: String) -> Result<Project, String> {
    load(&app, &id).ok_or_else(|| "no such project".into())
}

/// Creates (`id` null) or edits a project; returns its id. The allowed hosts are edited with
/// the network commands and are kept as they are.
#[tauri::command]
pub fn projects_save(app: tauri::AppHandle, id: Option<String>, name: String, description: String, system_prompt: String) -> Result<String, String> {
    let id = match id {
        Some(id) => id,
        None => {
            if list(&app).len() >= MAX_PROJECTS {
                return Err(format!("at most {MAX_PROJECTS} projects"));
            }
            let base = crate::config::profiles::slug(&name);
            if base.is_empty() {
                return Err("the name needs letters or digits".into());
            }
            let base: String = base.chars().take(48).collect();
            let (mut id, mut n) = (base.clone(), 2);
            while load(&app, &id).is_some() {
                id = format!("{base}-{n}");
                n += 1;
            }
            id
        }
    };
    let allow_hosts = load(&app, &id).map(|p| p.allow_hosts).unwrap_or_default();
    store(&app, &id, &Project { name: name.trim().into(), description, system_prompt, allow_hosts })?;
    Ok(id)
}

/// Deletes a project with everything in it: workflows, sessions, profiles, images, variables,
/// injections and its secrets (also from the credential store). Refused while something of it runs.
#[tauri::command]
pub fn projects_delete(app: tauri::AppHandle, id: String) -> Result<(), String> {
    use tauri::Manager;
    if load(&app, &id).is_none() {
        return Err("no such project".into());
    }
    if crate::workflow::engine::project_busy(&app, &id) || app.state::<crate::vm::images::Images>().busy_in(&id) {
        return Err("a session or an image build of this project is running".into());
    }
    let scope = Scope::project(&id);
    crate::config::secrets::forget_all(&app, &scope);
    std::fs::remove_dir_all(dir_in(&data(&app)?, &id)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_names() {
        assert!(valid_id("demo") && valid_id("a-1"));
        assert!(!valid_id("") && !valid_id("A") && !valid_id("a/b") && !valid_id("..") && !valid_id(&"a".repeat(65)));
        let p = Project { name: " ".into(), ..Default::default() };
        assert!(p.validate().is_err());
        assert!(Project { name: "x".into(), ..Default::default() }.validate().is_ok());
    }
}
