//! Secrets live in the OS credential store, never in `settings.json`. The token is write-only
//! from the frontend: commands report whether one is set but never return it.
//!
//! Besides the Claude token there are named secrets (Settings > Secrets). The credential store
//! cannot list its entries, so the names are kept in `<app data>/secrets.json` (no values). A
//! profile's environment variable may refer to one as `${secret:NAME}`; the reference is resolved
//! when the run starts (`resolve_env`), so the value is only in the app and the run's `Task`.

use std::path::{Path, PathBuf};

use keyring::Entry;
use serde::{Deserialize, Serialize};

use crate::profiles::EnvVar;
use crate::protocol::TaskEnv;

const SERVICE: &str = "cc-agent-flows";
const CLAUDE_TOKEN: &str = "claude-setup-token";

fn entry() -> Result<Entry, String> {
    Entry::new(SERVICE, CLAUDE_TOKEN).map_err(|e| e.to_string())
}

/// The stored Claude setup token, for the backend to hand to an agent VM.
#[allow(dead_code)]
pub fn claude_token() -> Option<String> {
    entry().ok()?.get_password().ok()
}

#[tauri::command]
pub fn claude_token_set(token: String) -> Result<(), String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("empty token".into());
    }
    entry()?.set_password(token).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn claude_token_status() -> Result<bool, String> {
    match entry()?.get_password() {
        Ok(_) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub fn claude_token_clear() -> Result<(), String> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

const MAX_SECRETS: usize = 200;
const MAX_VALUE: usize = 16 * 1024;

#[derive(Default, Serialize, Deserialize)]
struct Index {
    names: Vec<String>,
}

fn index_file(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("secrets.json"))
}

fn load_index(path: &Path) -> Index {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_index(path: &Path, index: &Index) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(index).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn secret_entry(name: &str) -> Result<Entry, String> {
    Entry::new(SERVICE, &format!("secret:{name}")).map_err(|e| e.to_string())
}

fn secret_value(name: &str) -> Option<String> {
    secret_entry(name).ok()?.get_password().ok()
}

/// Replaces every `${secret:NAME}` in `value` with what `lookup` returns; fails on an unknown name.
/// Other text, including `${` that is not a reference, is left as it is.
pub fn resolve(value: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    const OPEN: &str = "${secret:";
    let mut out = String::new();
    let mut rest = value;
    while let Some(at) = rest.find(OPEN) {
        let after = &rest[at + OPEN.len()..];
        let Some(end) = after.find('}') else { break };
        let name = &after[..end];
        if !valid_name(name) {
            out.push_str(&rest[..at + OPEN.len()]);
            rest = after;
            continue;
        }
        out.push_str(&rest[..at]);
        out.push_str(&lookup(name).ok_or_else(|| format!("the secret {name} is not set (Settings > Secrets)"))?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// A profile's variables for a `Task`, with the secret references filled in.
pub fn resolve_env(vars: &[EnvVar]) -> Result<Vec<TaskEnv>, String> {
    vars.iter()
        .map(|v| {
            let value = resolve(&v.value, &secret_value).map_err(|e| format!("{}: {e}", v.name))?;
            Ok(TaskEnv { name: v.name.clone(), value })
        })
        .collect()
}

/// The names of the saved secrets.
#[tauri::command]
pub fn secrets_list(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    Ok(load_index(&index_file(&app)?).names)
}

/// Creates a secret or replaces its value.
#[tauri::command]
pub fn secrets_set(app: tauri::AppHandle, name: String, value: String) -> Result<(), String> {
    if !valid_name(&name) {
        return Err("the name must be letters, digits and underscores, not starting with a digit (at most 64)".into());
    }
    if value.is_empty() || value.len() > MAX_VALUE || value.contains('\0') {
        return Err(format!("the value must be 1 to {MAX_VALUE} bytes"));
    }
    let path = index_file(&app)?;
    let mut index = load_index(&path);
    if !index.names.contains(&name) {
        if index.names.len() >= MAX_SECRETS {
            return Err(format!("at most {MAX_SECRETS} secrets"));
        }
        index.names.push(name.clone());
        index.names.sort();
    }
    secret_entry(&name)?.set_password(&value).map_err(|e| e.to_string())?;
    save_index(&path, &index)
}

#[tauri::command]
pub fn secrets_delete(app: tauri::AppHandle, name: String) -> Result<(), String> {
    match secret_entry(&name)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(e) => return Err(e.to_string()),
    }
    let path = index_file(&app)?;
    let mut index = load_index(&path);
    index.names.retain(|n| *n != name);
    save_index(&path, &index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(n: &str) -> Option<String> {
        (n == "A").then(|| "1".to_string()).or_else(|| (n == "B_2").then(|| "x y".to_string()))
    }

    #[test]
    fn resolves_references() {
        assert_eq!(resolve("plain", &look).unwrap(), "plain");
        assert_eq!(resolve("${secret:A}", &look).unwrap(), "1");
        assert_eq!(resolve("Bearer ${secret:A}-${secret:B_2}!", &look).unwrap(), "Bearer 1-x y!");
        // Not references: left alone.
        assert_eq!(resolve("${HOME} ${secret:} ${secret:a b} ${secret:A", &look).unwrap(), "${HOME} ${secret:} ${secret:a b} ${secret:A");
        assert!(resolve("${secret:NOPE}", &look).unwrap_err().contains("NOPE"));
    }

    #[test]
    fn names() {
        assert!(valid_name("GH_TOKEN") && valid_name("_x1"));
        assert!(!valid_name("") && !valid_name("1a") && !valid_name("a-b") && !valid_name("a b"));
    }
}
