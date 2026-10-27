//! Network settings that apply to every agent VM: the global host allowlist, stored as
//! `<app data>/network.json`. A profile can allow more hosts (`Profile::allow_hosts`); the
//! hosts here are allowed for all agents. Nothing enforces the lists yet.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::profiles::clean_host;

/// What Claude Code needs to work: the API and its telemetry endpoint.
pub const DEFAULT_ALLOW_HOSTS: &[&str] = &["api.anthropic.com", "statsig.anthropic.com"];

const MAX_HOSTS: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Network {
    /// Hosts every agent may reach over HTTPS, same format as `Profile::allow_hosts`.
    pub allow_hosts: Vec<String>,
}

impl Default for Network {
    fn default() -> Self {
        Network { allow_hosts: DEFAULT_ALLOW_HOSTS.iter().map(|s| s.to_string()).collect() }
    }
}

impl Network {
    /// Normalizes the list (case, blanks, duplicates) and checks every entry.
    fn cleaned(&self) -> Result<Network, String> {
        let mut hosts: Vec<String> = vec![];
        for h in self.allow_hosts.iter().filter(|h| !h.trim().is_empty()) {
            let h = clean_host(h)?;
            if !hosts.contains(&h) {
                hosts.push(h);
            }
        }
        if hosts.len() > MAX_HOSTS {
            return Err(format!("at most {MAX_HOSTS} allowed hosts"));
        }
        Ok(Network { allow_hosts: hosts })
    }
}

fn file(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("network.json"))
}

/// The saved settings, or the defaults when there are none (or the file is unreadable).
pub fn load(path: &Path) -> Network {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Network>(&t).ok()).and_then(|n| n.cleaned().ok()).unwrap_or_default()
}

pub fn save(path: &Path, network: &Network) -> Result<Network, String> {
    let network = network.cleaned()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&network).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(network)
}

#[tauri::command]
pub fn network_get(app: tauri::AppHandle) -> Result<Network, String> {
    Ok(load(&file(&app)?))
}

/// Saves the settings; returns them as stored (normalized).
#[tauri::command]
pub fn network_set(app: tauri::AppHandle, network: Network) -> Result<Network, String> {
    save(&file(&app)?, &network)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_then_normalized_save() {
        let dir = std::env::temp_dir().join(format!("ccaf-net-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("network.json");
        assert_eq!(load(&path), Network::default());
        assert!(load(&path).allow_hosts.contains(&"api.anthropic.com".to_string()));

        let saved = save(&path, &Network { allow_hosts: vec![" API.anthropic.com ".into(), "".into(), "*.Example.com".into(), "api.anthropic.com".into()] }).unwrap();
        assert_eq!(saved.allow_hosts, vec!["api.anthropic.com", "*.example.com"]);
        assert_eq!(load(&path), saved);

        // an empty list is a valid choice, not a reason to fall back to the defaults
        save(&path, &Network { allow_hosts: vec![] }).unwrap();
        assert!(load(&path).allow_hosts.is_empty());

        assert!(save(&path, &Network { allow_hosts: vec!["10.0.0.1".into()] }).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
