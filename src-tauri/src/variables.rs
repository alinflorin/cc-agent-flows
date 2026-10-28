//! Variables: named values that are not secret (a site's host, an e-mail address), kept in plain
//! text in `<app data>/variables.json` and edited in Settings > Variables. They are used like
//! secrets: `${var:NAME}` in an agent profile's environment variable (inside a longer value too,
//! `https://${var:HOST}`) or in an injection's host, path pattern or user name. The reference is
//! resolved when the run starts, so changing a variable changes every place that uses it. Unlike a
//! secret, the value is returned to the UI.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

const MAX_VARIABLES: usize = 200;
const MAX_VALUE: usize = 4096;

/// The variables the demo content comes with: the Atlassian site the demo injections and profiles
/// point at, placeholders to be edited.
pub const DEMO_VARIABLES: &[(&str, &str)] = &[("ATLASSIAN_HOST", "your-site.atlassian.net"), ("ATLASSIAN_EMAIL", "you@example.com")];

#[derive(Serialize)]
pub struct Variable {
    name: String,
    value: String,
}

fn file(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("variables.json"))
}

fn load(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save(path: &Path, map: &BTreeMap<String, String>) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(map).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

fn check(name: &str, value: &str) -> Result<(), String> {
    if !crate::secrets::valid_name(name) {
        return Err("the name must be letters, digits and underscores, not starting with a digit (at most 64)".into());
    }
    if value.is_empty() || value.len() > MAX_VALUE || value.chars().any(|c| c.is_control()) {
        return Err(format!("the value must be 1 to {MAX_VALUE} characters, on one line"));
    }
    Ok(())
}

/// All variables, for resolving `${var:NAME}` in a run.
pub fn all(app: &tauri::AppHandle) -> BTreeMap<String, String> {
    file(app).map(|f| load(&f)).unwrap_or_default()
}

/// Part of the demo content: adds the demo variables that do not exist yet.
pub fn provision_demo(app: &tauri::AppHandle) -> Result<(), String> {
    let path = file(app)?;
    let mut map = load(&path);
    for (name, value) in DEMO_VARIABLES {
        map.entry(name.to_string()).or_insert_with(|| value.to_string());
    }
    save(&path, &map)
}

#[tauri::command]
pub fn variables_list(app: tauri::AppHandle) -> Result<Vec<Variable>, String> {
    Ok(load(&file(&app)?).into_iter().map(|(name, value)| Variable { name, value }).collect())
}

/// Creates a variable or replaces its value.
#[tauri::command]
pub fn variables_set(app: tauri::AppHandle, name: String, value: String) -> Result<(), String> {
    check(&name, &value)?;
    let path = file(&app)?;
    let mut map = load(&path);
    if !map.contains_key(&name) && map.len() >= MAX_VARIABLES {
        return Err(format!("at most {MAX_VARIABLES} variables"));
    }
    map.insert(name, value);
    save(&path, &map)
}

#[tauri::command]
pub fn variables_delete(app: tauri::AppHandle, name: String) -> Result<(), String> {
    let path = file(&app)?;
    let mut map = load(&path);
    map.remove(&name);
    save(&path, &map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_variables_are_valid() {
        for (n, v) in DEMO_VARIABLES {
            assert!(check(n, v).is_ok(), "{n}");
        }
    }

    #[test]
    fn checks_names_and_values() {
        assert!(check("A_1", "x").is_ok());
        assert!(check("1A", "x").is_err() && check("A", "").is_err() && check("A", "a\nb").is_err());
        assert!(check("A", &"x".repeat(MAX_VALUE + 1)).is_err());
    }
}
