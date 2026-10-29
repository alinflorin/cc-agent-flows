//! The global system prompt (Settings > LLM): text every agent of every project gets first, before
//! the project's, the workflow's and the profile's. A JSON file `system_prompt.json` in the app
//! data dir; missing or damaged = empty.

use serde::{Deserialize, Serialize};

use crate::util::{read_json, write_json};

/// Longest text accepted (bytes), as for a profile's system prompt.
const MAX_LEN: usize = 64 * 1024;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Stored {
    #[serde(default)]
    system_prompt: String,
}

fn path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(crate::util::data_dir(app)?.join("system_prompt.json"))
}

/// The global system prompt, empty when none is set.
pub fn current(app: &tauri::AppHandle) -> String {
    path(app).ok().and_then(|p| read_json::<Stored>(&p)).map(|s| s.system_prompt).unwrap_or_default()
}

/// The text a project's runs start from: the global prompt, then the project's.
pub fn with_project(app: &tauri::AppHandle, project: &str) -> String {
    let project_prompt = crate::config::projects::load(app, project).map(|p| p.system_prompt).unwrap_or_default();
    crate::workflow::combine_system_prompts(&current(app), &project_prompt).unwrap_or_default()
}

fn check(text: &str) -> Result<(), String> {
    if text.len() > MAX_LEN {
        return Err("the system prompt is too large".into());
    }
    Ok(())
}

#[tauri::command]
pub fn system_prompt_get(app: tauri::AppHandle) -> String {
    current(&app)
}

#[tauri::command]
pub fn system_prompt_set(app: tauri::AppHandle, text: String) -> Result<(), String> {
    check(&text)?;
    write_json(&path(&app)?, &Stored { system_prompt: text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_size_is_limited() {
        assert!(check("be brief").is_ok());
        assert!(check(&"x".repeat(MAX_LEN + 1)).is_err());
    }

    #[test]
    fn a_partial_file_reads_as_empty() {
        let s: Stored = serde_json::from_str("{}").unwrap();
        assert_eq!(s.system_prompt, "");
    }
}
