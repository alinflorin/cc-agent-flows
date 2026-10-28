//! Network settings that apply to agent VMs: the global host allowlist, stored as
//! `<app data>/network.json`, and a project's own list (in its `project.json`). A profile can
//! allow more hosts (`Profile::allow_hosts`); a run may reach the union of the three. `runs.rs`
//! enforces them through the egress proxy.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::profiles::clean_host;
use crate::config::scope::Scope;

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
            // a `${var:NAME}` is only known at run time, when the entry is checked again
            let h = if h.contains("${var:") { h.trim().to_string() } else { clean_host(h)? };
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
    Ok(crate::util::data_dir(app)?.join("network.json"))
}

/// The saved settings, or the defaults when there are none (or the file is unreadable).
pub fn load(path: &Path) -> Network {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Network>(&t).ok()).and_then(|n| n.cleaned().ok()).unwrap_or_default()
}

/// The saved settings of this installation.
pub fn current(app: &tauri::AppHandle) -> Network {
    file(app).map(|p| load(&p)).unwrap_or_default()
}

pub fn save(path: &Path, network: &Network) -> Result<Network, String> {
    let network = network.cleaned()?;
    crate::util::write_json(path, &network)?;
    Ok(network)
}

/// The hosts a project adds to the global ones (empty for an unknown project).
pub fn project_hosts(app: &tauri::AppHandle, project: &Scope) -> Vec<String> {
    project.id().and_then(|id| crate::config::projects::load(app, id)).map(|p| p.allow_hosts).unwrap_or_default()
}

/// The global hosts, or a project's own.
#[tauri::command]
pub fn network_get(app: tauri::AppHandle, project: Option<String>) -> Result<Network, String> {
    let scope = Scope::of(project)?;
    if scope.is_global() {
        return Ok(load(&file(&app)?));
    }
    crate::config::projects::load(&app, scope.id().unwrap_or_default()).map(|p| Network { allow_hosts: p.allow_hosts }).ok_or_else(|| "no such project".into())
}

/// Saves the global hosts or a project's; returns them as stored (normalized).
#[tauri::command]
pub fn network_set(app: tauri::AppHandle, project: Option<String>, network: Network) -> Result<Network, String> {
    let scope = Scope::of(project)?;
    let Some(id) = scope.id() else {
        return save(&file(&app)?, &network);
    };
    let mut p = crate::config::projects::load(&app, id).ok_or("no such project")?;
    let network = network.cleaned()?;
    p.allow_hosts = network.allow_hosts.clone();
    crate::config::projects::store(&app, id, &p)?;
    Ok(network)
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
