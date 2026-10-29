//! Integrations: a service an agent works with (Jira, GitHub), set up once in one place instead of
//! four. An integration lists the variables, secrets and injections it needs, the hosts it
//! reaches and the environment variables an agent gets; saving it writes the variables, secrets
//! and injections into their own stores (Variables, Secrets, Injections pages), where they stay
//! editable, but cannot be deleted while an integration names them. A profile names the
//! integrations it uses (`Profile::integrations`); at run start their environment variables and
//! hosts are added to the profile's own (`expand`), so editing an integration changes every
//! profile that uses it. Global and project integrations follow the usual rule: a run sees both
//! and the project's wins on a name.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::injections::{self, Injection};
use crate::config::profiles::{EnvVar, Profile, clean_host, validate_env};
use crate::config::scope::Scope;
use crate::config::{secrets, variables};

const MAX_INTEGRATIONS: usize = 100;

/// What is stored: the names of the things the integration owns, not their values.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Integration {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Hosts an agent with this integration may reach, as in `Profile::allow_hosts`.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Environment variables the agent gets (`${var:..}`, `${secret:..}`, `${injection:..}`).
    #[serde(default)]
    pub env: Vec<EnvVar>,
    #[serde(default)]
    pub variables: Vec<String>,
    #[serde(default)]
    pub secrets: Vec<String>,
    #[serde(default)]
    pub injections: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct VarInput {
    pub name: String,
    pub value: String,
}

#[derive(Serialize, Deserialize)]
pub struct SecretInput {
    pub name: String,
    /// New value; `None` keeps the one already saved.
    #[serde(default)]
    pub value: Option<String>,
}

/// What the UI edits: the integration with the values of its variables, secrets (write-only) and
/// the injections' definitions.
#[derive(Serialize, Deserialize)]
pub struct Input {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    #[serde(default)]
    pub variables: Vec<VarInput>,
    #[serde(default)]
    pub secrets: Vec<SecretInput>,
    #[serde(default)]
    pub injections: Vec<Injection>,
}

#[derive(Serialize)]
pub struct View {
    pub name: String,
    pub description: String,
    pub hosts: Vec<String>,
    pub env: Vec<EnvVar>,
    pub variables: Vec<VarInput>,
    /// Names; `set` tells whether the secret has a value.
    pub secrets: Vec<SecretView>,
    pub injections: Vec<Injection>,
}

#[derive(Serialize)]
pub struct SecretView {
    pub name: String,
    pub set: bool,
}

fn file(app: &tauri::AppHandle, scope: &Scope) -> Result<PathBuf, String> {
    Ok(scope.dir(app)?.join("integrations.json"))
}

fn load(path: &Path) -> Vec<Integration> {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn store(path: &Path, list: &[Integration]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(list).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

fn in_scope(app: &tauri::AppHandle, scope: &Scope) -> Vec<Integration> {
    file(app, scope).map(|f| load(&f)).unwrap_or_default()
}

/// Refuses to delete a variable, secret or injection (`kind`) that an integration of the scope names.
pub fn check_free(app: &tauri::AppHandle, scope: &Scope, kind: &str, name: &str) -> Result<(), String> {
    let used = in_scope(app, scope).into_iter().find(|i| {
        let list = match kind {
            "variable" => &i.variables,
            "secret" => &i.secrets,
            _ => &i.injections,
        };
        list.iter().any(|n| n == name)
    });
    match used {
        Some(i) => Err(format!("the {kind} {name} is used by the integration {}: remove it there first", i.name)),
        None => Ok(()),
    }
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 64 && name == name.trim() && !name.chars().any(|c| c.is_control())
}

/// Checks and normalizes the stored part.
fn clean(mut i: Integration) -> Result<Integration, String> {
    if !valid_name(&i.name) {
        return Err("the name must be 1 to 64 characters, without control characters".into());
    }
    if i.description.len() > 2000 {
        return Err("the description is too long".into());
    }
    let mut hosts: Vec<String> = vec![];
    for h in i.hosts.iter().filter(|h| !h.trim().is_empty()) {
        let h = if h.contains("${var:") { h.trim().to_string() } else { clean_host(h)? };
        if !hosts.contains(&h) {
            hosts.push(h);
        }
    }
    if hosts.len() > 100 {
        return Err("at most 100 hosts".into());
    }
    i.hosts = hosts;
    validate_env(i.env.iter().map(|v| (v.name.as_str(), Some(v.value.as_str()))))?;
    for (kind, list) in [("variable", &i.variables), ("secret", &i.secrets), ("injection", &i.injections)] {
        let mut seen = BTreeSet::new();
        for n in list {
            if !secrets::valid_name(n) {
                return Err(format!("{n:?} is not a valid {kind} name"));
            }
            if !seen.insert(n) {
                return Err(format!("the {kind} {n} is listed twice"));
            }
        }
    }
    Ok(i)
}

fn view(app: &tauri::AppHandle, scope: &Scope, i: Integration) -> View {
    let vars = variables::own(app, scope);
    let have: Vec<String> = secrets::names(app, scope);
    let injs = injections::in_scope(app, scope);
    View {
        variables: i.variables.iter().map(|n| VarInput { name: n.clone(), value: vars.get(n).cloned().unwrap_or_default() }).collect(),
        secrets: i.secrets.iter().map(|n| SecretView { name: n.clone(), set: have.contains(n) }).collect(),
        injections: i.injections.iter().filter_map(|n| injs.iter().find(|x| x.name == *n).cloned()).collect(),
        name: i.name,
        description: i.description,
        hosts: i.hosts,
        env: i.env,
    }
}

/// The integrations defined in the global settings or in one project (not merged).
#[tauri::command]
pub fn integrations_list(app: tauri::AppHandle, project: Option<String>) -> Result<Vec<View>, String> {
    let scope = Scope::of(project)?;
    let mut list = in_scope(&app, &scope);
    list.sort_by_key(|i| i.name.to_lowercase());
    Ok(list.into_iter().map(|i| view(&app, &scope, i)).collect())
}

/// Creates (`previous` null) or replaces an integration (`previous` is its current name, so it can
/// be renamed), writing its variables, secrets and injections into their stores.
#[tauri::command]
pub fn integrations_save(app: tauri::AppHandle, project: Option<String>, previous: Option<String>, integration: Input) -> Result<(), String> {
    let scope = Scope::of(project)?;
    let Input { name, description, hosts, env, variables: vars, secrets: secs, injections: injs } = integration;
    let stored = clean(Integration {
        name,
        description,
        hosts,
        env,
        variables: vars.iter().map(|v| v.name.clone()).collect(),
        secrets: secs.iter().map(|s| s.name.clone()).collect(),
        injections: injs.iter().map(|i| i.name.clone()).collect(),
    })?;
    let path = file(&app, &scope)?;
    let mut list = load(&path);
    let at = previous.as_ref().and_then(|p| list.iter().position(|i| i.name == *p));
    if list.iter().enumerate().any(|(k, i)| i.name == stored.name && Some(k) != at) {
        return Err(format!("there is already an integration named {}", stored.name));
    }
    if at.is_none() && list.len() >= MAX_INTEGRATIONS {
        return Err(format!("at most {MAX_INTEGRATIONS} integrations"));
    }
    // check everything before writing anything
    let have = secrets::names(&app, &scope);
    for s in &secs {
        if s.value.as_deref().is_none_or(str::is_empty) && !have.contains(&s.name) {
            return Err(format!("the secret {} needs a value", s.name));
        }
    }
    for i in &injs {
        i.validate()?;
    }
    for v in &vars {
        variables::set(&app, &scope, &v.name, &v.value)?;
    }
    for s in &secs {
        if let Some(value) = s.value.as_deref().filter(|v| !v.is_empty()) {
            secrets::set(&app, &scope, &s.name, value)?;
        }
    }
    injections::upsert(&app, &scope, &injs)?;
    match at {
        Some(k) => list[k] = stored,
        None => list.push(stored),
    }
    store(&path, &list)
}

/// Deletes the integration (its variables, secrets and injections stay, now free to delete);
/// refused while a profile of the project uses it.
#[tauri::command]
pub fn integrations_delete(app: tauri::AppHandle, project: Option<String>, name: String) -> Result<(), String> {
    let scope = Scope::of(project)?;
    if let Some(p) = scope.id() {
        let dir = crate::config::profiles::dir(&app, p)?;
        if let Some(e) = crate::config::profiles::list(&dir).into_iter().find(|e| e.profile.integrations.contains(&name)) {
            return Err(format!("the agent profile {} uses {name}", e.profile.name));
        }
    }
    let path = file(&app, &scope)?;
    let mut list = load(&path);
    list.retain(|i| i.name != name);
    store(&path, &list)
}

/// What a profile runs with: its own environment variables and hosts plus those of its
/// integrations (the profile's own win on a name). A missing integration fails.
pub fn expand(app: &tauri::AppHandle, scope: &Scope, profile: &Profile) -> Result<(Vec<EnvVar>, Vec<String>), String> {
    let mut own = in_scope(app, scope);
    if !scope.is_global() {
        own.extend(in_scope(app, &Scope::global()));
    }
    merge(profile, &own)
}

/// [`expand`] over a list where earlier entries win on a name.
fn merge(profile: &Profile, available: &[Integration]) -> Result<(Vec<EnvVar>, Vec<String>), String> {
    let mut env = profile.env.clone();
    let mut hosts = profile.allow_hosts.clone();
    for name in &profile.integrations {
        let i = available.iter().find(|i| i.name == *name).ok_or_else(|| format!("the integration {name} does not exist"))?;
        for v in &i.env {
            if !env.iter().any(|e| e.name == v.name) {
                env.push(v.clone());
            }
        }
        for h in &i.hosts {
            if !hosts.contains(h) {
                hosts.push(h.clone());
            }
        }
    }
    Ok((env, hosts))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(n: &str, v: &str) -> EnvVar {
        EnvVar { name: n.into(), value: v.into() }
    }

    fn jira() -> Integration {
        Integration {
            name: "Jira".into(),
            hosts: vec!["${var:HOST}".into(), "api.media.atlassian.com".into()],
            env: vec![ev("JIRA_API_TOKEN", "${injection:JIRA}"), ev("ATLASSIAN_URL", "https://${var:HOST}")],
            variables: vec!["HOST".into()],
            secrets: vec!["JIRA_TOKEN".into()],
            injections: vec!["JIRA".into()],
            ..Default::default()
        }
    }

    fn profile(integrations: &[&str]) -> Profile {
        Profile {
            name: "p".into(),
            description: String::new(),
            system_prompt: String::new(),
            env: vec![ev("ATLASSIAN_URL", "mine")],
            model: None,
            effort: None,
            cpus: None,
            memory_mb: Some(2048),
            image: Some("base".into()),
            allow_hosts: vec!["github.com".into(), "api.media.atlassian.com".into()],
            integrations: integrations.iter().map(|s| s.to_string()).collect(),
            max_connections: None,
            max_upload_mb: None,
            max_download_mb: None,
            extends: None,
        }
    }

    #[test]
    fn a_profile_gets_the_env_and_hosts_of_its_integrations() {
        let (env, hosts) = merge(&profile(&["Jira"]), &[jira()]).unwrap();
        assert_eq!(env, vec![ev("ATLASSIAN_URL", "mine"), ev("JIRA_API_TOKEN", "${injection:JIRA}")], "the profile's own value wins");
        assert_eq!(hosts, vec!["github.com", "api.media.atlassian.com", "${var:HOST}"]);
        assert_eq!(merge(&profile(&[]), &[jira()]).unwrap().0.len(), 1);
        assert!(merge(&profile(&["Nope"]), &[jira()]).unwrap_err().contains("Nope"));
    }

    #[test]
    fn checks_the_stored_part() {
        let mut i = jira();
        i.hosts.push(" GitHub.com ".into());
        assert_eq!(clean(i.clone()).unwrap().hosts.last().unwrap(), "github.com");
        i.secrets.push("JIRA_TOKEN".into());
        assert!(clean(i).is_err());
        let mut bad = jira();
        bad.name = " x".into();
        assert!(clean(bad).is_err());
        let mut bad = jira();
        bad.env.push(ev("PATH", "x"));
        assert!(clean(bad).is_err());
    }
}
