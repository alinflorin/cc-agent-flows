//! Projects: a named group of sessions with a piece of system prompt that every agent of those
//! sessions gets (after the workflow's instructions, before the profile's own prompt).
//! Stored as `<app data>/projects/<id>.json`; a session names its project in `Session.project`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::workflow::{self, read_json, safe_id, write_json};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub system_prompt: String,
}

#[derive(Serialize)]
pub struct ProjectEntry {
    pub id: String,
    #[serde(flatten)]
    pub project: Project,
    /// How many sessions are in it.
    pub sessions: usize,
    pub workflows: usize,
}

fn dir(root: &Path) -> PathBuf {
    root.join("projects")
}

fn path(root: &Path, id: &str) -> Result<PathBuf, String> {
    Ok(dir(root).join(format!("{}.json", safe_id(id)?)))
}

pub fn exists(root: &Path, id: &str) -> bool {
    path(root, id).is_ok_and(|p| p.exists())
}

/// A project, or an empty one if it is gone (a session then runs without its prompt).
pub fn get(root: &Path, id: &str) -> Project {
    path(root, id).ok().and_then(|p| read_json(&p)).unwrap_or_default()
}

pub fn list(root: &Path) -> Vec<ProjectEntry> {
    let Ok(rd) = std::fs::read_dir(dir(root)) else { return vec![] };
    let sessions = workflow::sessions(root);
    let workflows = workflow::workflows(root, None);
    let mut out: Vec<ProjectEntry> = rd
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let id = p.file_stem()?.to_str()?.to_string();
            let count = sessions.iter().filter(|s| s.session.project == id).count();
            let wf = workflows.iter().filter(|w| w.project == id).count();
            Some(ProjectEntry { project: read_json(&p)?, sessions: count, workflows: wf, id })
        })
        .collect();
    out.sort_by_key(|e| e.project.name.to_lowercase());
    out
}

/// Saves a project (`None` id creates one); returns its id.
pub fn save(root: &Path, id: Option<&str>, project: &Project) -> Result<String, String> {
    let name = project.name.trim();
    if name.is_empty() || name.len() > 80 {
        return Err("the name is 1 to 80 characters".into());
    }
    if project.description.len() > 500 || project.system_prompt.len() > 64 * 1024 {
        return Err("the description is up to 500 characters and the prompt up to 64 KB".into());
    }
    let id = match id {
        Some(id) => {
            if !exists(root, id) {
                return Err("no such project".into());
            }
            id.to_string()
        }
        None => {
            let slug: String = name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
            let base = slug.trim_matches('-').chars().take(40).collect::<String>();
            let base = if base.is_empty() { "project".to_string() } else { base };
            let (mut id, mut n) = (base.clone(), 2);
            while exists(root, &id) {
                id = format!("{base}-{n}");
                n += 1;
            }
            id
        }
    };
    write(root, &id, project)?;
    Ok(id)
}

/// Writes a project under a fixed id (the demo project), replacing what is there.
pub fn write(root: &Path, id: &str, project: &Project) -> Result<(), String> {
    let stored = Project { name: project.name.trim().into(), description: project.description.trim().into(), system_prompt: project.system_prompt.clone() };
    write_json(&path(root, id)?, &stored)
}

/// Deletes a project no session is in, with its workflows.
pub fn delete(root: &Path, id: &str) -> Result<(), String> {
    let p = path(root, id)?;
    if workflow::sessions(root).iter().any(|s| s.session.project == id) {
        return Err("sessions are in this project: move or delete them first".into());
    }
    for w in workflow::workflows(root, Some(id)) {
        workflow::delete_workflow(root, &w.id)?;
    }
    std::fs::remove_file(p).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn projects_list(app: tauri::AppHandle) -> Result<Vec<ProjectEntry>, String> {
    Ok(list(&workflow::root(&app)?))
}

#[tauri::command]
pub fn projects_save(app: tauri::AppHandle, id: Option<String>, project: Project) -> Result<String, String> {
    save(&workflow::root(&app)?, id.as_deref(), &project)
}

#[tauri::command]
pub fn projects_delete(app: tauri::AppHandle, id: String) -> Result<(), String> {
    delete(&workflow::root(&app)?, &id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_lists_and_refuses_to_delete_a_used_project() {
        let root = std::env::temp_dir().join(format!("ccaf-projects-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let p = Project { name: " Website Redesign! ".into(), description: String::new(), system_prompt: "Use Tailwind.".into() };
        let id = save(&root, None, &p).unwrap();
        assert_eq!(id, "website-redesign");
        assert_eq!(save(&root, None, &p).unwrap(), "website-redesign-2");
        assert_eq!(get(&root, &id).system_prompt, "Use Tailwind.");
        assert!(save(&root, None, &Project::default()).is_err());
        assert!(save(&root, Some("nope"), &p).is_err());

        let wid = workflow::save_workflow(&root, "", &id, "W", &workflow::standard_workflow(), &workflow::Instructions::default()).unwrap();
        let sid = workflow::create_session(&root, "", "", &wid).unwrap();
        assert_eq!(workflow::read_session(&root, &sid).unwrap().project, id);
        let e = list(&root).into_iter().find(|e| e.id == id).unwrap();
        assert_eq!((e.sessions, e.workflows), (1, 1));
        assert!(delete(&root, &id).is_err());
        workflow::delete_session(&root, &sid).unwrap();
        delete(&root, &id).unwrap();
        assert!(workflow::workflows(&root, None).is_empty(), "its workflows go with it");
        let _ = std::fs::remove_dir_all(&root);
    }
}
