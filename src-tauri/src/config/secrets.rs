//! Secrets live in the OS credential store, never in `settings.json`; they are write-only from the
//! frontend. (The Claude setup tokens are in `accounts.rs`.) These are the named secrets, global
//! (Settings > Secrets) or of a project (its Secrets page): `secret:NAME` and `secret:<project>:NAME`
//! in the store. A run looks a name up in the project first, then in the global ones. The credential store
//! cannot list its entries, so the names are kept in `secrets.json` of the scope (no values). A
//! profile's environment variable may refer to one as `${secret:NAME}`; the reference is resolved
//! when the run starts (`injections::prepare`), so the value is only in the app and the run's `Task`.

use std::path::{Path, PathBuf};

use keyring::Entry;
use serde::{Deserialize, Serialize};

use crate::config::scope::Scope;

use crate::util::KEYRING_SERVICE as SERVICE;
const MAX_SECRETS: usize = 200;
const MAX_VALUE: usize = 16 * 1024;

#[derive(Default, Serialize, Deserialize)]
struct Index {
    names: Vec<String>,
}

fn index_file(app: &tauri::AppHandle, scope: &Scope) -> Result<PathBuf, String> {
    Ok(scope.dir(app)?.join("secrets.json"))
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

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn secret_entry(scope: &Scope, name: &str) -> Result<Entry, String> {
    Entry::new(SERVICE, &scope.secret_account(name)).map_err(|e| e.to_string())
}

/// The value of a secret as a run in `project` sees it: the project's own, else the global one.
pub fn secret_value(project: &Scope, name: &str) -> Option<String> {
    let own = (!project.is_global()).then(|| secret_entry(project, name).ok()?.get_password().ok()).flatten();
    own.or_else(|| secret_entry(&Scope::global(), name).ok()?.get_password().ok())
}

/// Removes every secret of a scope from the credential store (when a project is deleted).
pub fn forget_all(app: &tauri::AppHandle, scope: &Scope) {
    let Ok(path) = index_file(app, scope) else { return };
    for name in load_index(&path).names {
        if let Ok(e) = secret_entry(scope, &name) {
            let _ = e.delete_credential();
        }
    }
}

/// Replaces every `${<kind>:NAME}` in `value` with what `lookup` returns; fails on an unknown
/// name. Other text, including `${` that is not a reference, is left as it is.
pub fn resolve(value: &str, kind: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let open = format!("${{{kind}:");
    let mut out = String::new();
    let mut rest = value;
    while let Some(at) = rest.find(&open) {
        let after = &rest[at + open.len()..];
        let Some(end) = after.find('}') else { break };
        let name = &after[..end];
        if !valid_name(name) {
            out.push_str(&rest[..at + open.len()]);
            rest = after;
            continue;
        }
        out.push_str(&rest[..at]);
        let tab = match kind {
            "secret" => "Secrets",
            "var" => "Variables",
            _ => "Injections",
        };
        out.push_str(&lookup(name).ok_or_else(|| format!("the {kind} {name} does not exist ({tab}, in the project or in Settings)"))?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

const DUMMY: &str = "dummy";

/// Part of the demo content: adds the demo secrets with a dummy value. One that is already listed
/// or already in the credential store is left as it is. A secret of the same name that older
/// versions kept in the global store is moved into the scope instead of a dummy (and removed
/// from the global one), so values the user already entered are not lost.
///
/// `reset`: the demo secrets get the dummy value again, a real one is lost.
pub fn provision_demo(app: &tauri::AppHandle, scope: &Scope, reset: bool) -> Result<(), String> {
    let path = index_file(app, scope)?;
    let mut index = load_index(&path);
    let global = Scope::global();
    let global_path = index_file(app, &global)?;
    let mut global_index = load_index(&global_path);
    for name in crate::demo::secrets() {
        if !reset && index.names.iter().any(|n| n == name) {
            continue;
        }
        let entry = secret_entry(scope, name)?;
        let old = secret_entry(&global, name)?;
        let moved = if scope.is_global() { None } else { old.get_password().ok() };
        match entry.get_password() {
            Ok(_) if !reset => {}
            Ok(_) | Err(keyring::Error::NoEntry) => entry.set_password(moved.as_deref().unwrap_or(DUMMY)).map_err(|e| e.to_string())?,
            Err(e) => return Err(e.to_string()),
        }
        if moved.is_some() {
            let _ = old.delete_credential();
            global_index.names.retain(|n| n != name);
        }
        if !index.names.iter().any(|n| n == name) {
            index.names.push(name.to_string());
        }
    }
    index.names.sort();
    save_index(&path, &index)?;
    if !scope.is_global() && global_path.is_file() {
        save_index(&global_path, &global_index)?;
    }
    Ok(())
}

/// Secrets > Reset to demo content.
#[tauri::command]
pub fn secrets_reset_demo(app: tauri::AppHandle, project: Option<String>) -> Result<(), String> {
    provision_demo(&app, &Scope::of(project)?, true)
}

/// The names of the saved secrets.
#[tauri::command]
pub fn secrets_list(app: tauri::AppHandle, project: Option<String>) -> Result<Vec<String>, String> {
    Ok(load_index(&index_file(&app, &Scope::of(project)?)?).names)
}

/// Creates a secret or replaces its value.
#[tauri::command]
pub fn secrets_set(app: tauri::AppHandle, project: Option<String>, name: String, value: String) -> Result<(), String> {
    let scope = Scope::of(project)?;
    if !valid_name(&name) {
        return Err("the name must be letters, digits and underscores, not starting with a digit (at most 64)".into());
    }
    if value.is_empty() || value.len() > MAX_VALUE || value.contains('\0') {
        return Err(format!("the value must be 1 to {MAX_VALUE} bytes"));
    }
    let path = index_file(&app, &scope)?;
    let mut index = load_index(&path);
    if !index.names.contains(&name) {
        if index.names.len() >= MAX_SECRETS {
            return Err(format!("at most {MAX_SECRETS} secrets"));
        }
        index.names.push(name.clone());
        index.names.sort();
    }
    secret_entry(&scope, &name)?.set_password(&value).map_err(|e| e.to_string())?;
    save_index(&path, &index)
}

#[tauri::command]
pub fn secrets_delete(app: tauri::AppHandle, project: Option<String>, name: String) -> Result<(), String> {
    let scope = Scope::of(project)?;
    match secret_entry(&scope, &name)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(e) => return Err(e.to_string()),
    }
    let path = index_file(&app, &scope)?;
    let mut index = load_index(&path);
    index.names.retain(|n| *n != name);
    save_index(&path, &index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_secret_names_are_valid() {
        assert!(crate::demo::secrets().iter().all(|n| valid_name(n)));
    }

    fn look(n: &str) -> Option<String> {
        (n == "A").then(|| "1".to_string()).or_else(|| (n == "B_2").then(|| "x y".to_string()))
    }

    #[test]
    fn resolves_references() {
        assert_eq!(resolve("plain", "secret", &look).unwrap(), "plain");
        assert_eq!(resolve("${secret:A}", "secret", &look).unwrap(), "1");
        assert_eq!(resolve("Bearer ${secret:A}-${secret:B_2}!", "secret", &look).unwrap(), "Bearer 1-x y!");
        // Not references: left alone.
        assert_eq!(resolve("${HOME} ${secret:} ${secret:a b} ${secret:A", "secret", &look).unwrap(), "${HOME} ${secret:} ${secret:a b} ${secret:A");
        assert!(resolve("${secret:NOPE}", "secret", &look).unwrap_err().contains("NOPE"));
    }

    #[test]
    fn names() {
        assert!(valid_name("GH_TOKEN") && valid_name("_x1"));
        assert!(!valid_name("") && !valid_name("1a") && !valid_name("a-b") && !valid_name("a b"));
    }
}
