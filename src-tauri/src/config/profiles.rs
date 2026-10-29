//! Agent profiles: how an agent behaves and which image it runs on. A profile has a name, a
//! system prompt, environment variables and the id of an image (see `images.rs`; `base` is the
//! built-in one). The user creates them in the UI; each is `<id>.json` in the app data dir. The
//! app ships none, except the starter ones the user accepts on first start.
//!
//! Everything in a profile travels in the `Task` when a run starts, nothing is baked into an
//! image (variables baked into an image are set on the image itself).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::scope::Scope;

/// Variables the app or agentd own; a profile may not set them.
const RESERVED: &[&str] = &["CLAUDE_CODE_OAUTH_TOKEN", "PATH", "HOME", "USER", "LOGNAME", "SHELL", "USE_BUILTIN_RIPGREP"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvVar {
    pub name: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// Claude model (an alias like `sonnet` or a full id); `None` is Claude Code's default.
    #[serde(default)]
    pub model: Option<String>,
    /// Thinking level, `low | medium | high | xhigh | max`; `None` is Claude Code's default.
    #[serde(default)]
    pub effort: Option<String>,
    /// Virtual CPUs of the VM; `None` gives it all the host's cores (QEMU limits the number of
    /// vCPUs, not their share of time).
    #[serde(default)]
    pub cpus: Option<u32>,
    /// RAM of the VM in MB.
    #[serde(default = "default_memory")]
    pub memory_mb: u32,
    /// The image the agent runs on.
    #[serde(default = "base_image")]
    pub image: String,
    /// Hosts the agent may reach over HTTPS (port 443), besides the Claude API which is always
    /// allowed: an exact name (`github.com`) or all the subdomains of one (`*.github.com`).
    /// Lowercase, no scheme, port or path. Empty means only the Claude API.
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    /// Names of integrations (Integrations page of the project or the global settings) whose
    /// environment variables and allowed hosts this agent gets, on top of its own.
    #[serde(default)]
    pub integrations: Vec<String>,
}

/// Most entries in a profile's host allowlist.
const MAX_ALLOW_HOSTS: usize = 100;

/// Normalizes one allowlist entry (trim, lowercase, no trailing dot) and checks it is a host name
/// or a `*.` wildcard over one. IP addresses and single-label names are refused.
pub fn clean_host(entry: &str) -> Result<String, String> {
    let host = entry.trim().trim_end_matches('.').to_ascii_lowercase();
    let bad = || format!("{entry:?} is not a host name (use example.com or *.example.com)");
    let name = host.strip_prefix("*.").unwrap_or(&host);
    let labels: Vec<&str> = name.split('.').collect();
    let label_ok = |l: &&str| {
        !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    // The last label must not be all digits, so `1.2.3.4` is not taken for a name.
    let tld_ok = labels.last().is_some_and(|l| l.chars().any(|c| c.is_ascii_alphabetic()));
    if host.len() > 253 || labels.len() < 2 || !labels.iter().all(label_ok) || !tld_ok {
        return Err(bad());
    }
    Ok(host)
}

pub const DEFAULT_MEMORY_MB: u32 = 2048;

fn default_memory() -> u32 {
    DEFAULT_MEMORY_MB
}

fn base_image() -> String {
    "base".into()
}

/// A profile as listed to the UI.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub id: String,
    #[serde(flatten)]
    pub profile: Profile,
}

/// Checks environment variable names (and values, where given) for a profile or an image.
pub fn validate_env<'a>(vars: impl Iterator<Item = (&'a str, Option<&'a str>)>) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for (name, value) in vars {
        let ok = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ok {
            return Err(format!("{name:?} is not a valid variable name"));
        }
        if RESERVED.contains(&name) || name.starts_with("LD_") {
            return Err(format!("{name} is reserved"));
        }
        if !seen.insert(name) {
            return Err(format!("{name} is set twice"));
        }
        if value.is_some_and(|s| s.contains('\0')) {
            return Err(format!("{name} has a NUL in its value"));
        }
    }
    Ok(())
}

impl Profile {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.name.len() > 80 {
            return Err("the name must be 1 to 80 characters".into());
        }
        validate_env(self.env.iter().map(|v| (v.name.as_str(), Some(v.value.as_str()))))?;
        if self.system_prompt.len() > 64 * 1024 {
            return Err("the system prompt is too large".into());
        }
        if let Some(m) = &self.model
            && (m.is_empty() || m.len() > 100 || !m.chars().all(|c| c.is_ascii_alphanumeric() || "-._:[]".contains(c)))
        {
            return Err("the model is not a valid name".into());
        }
        if let Some(e) = &self.effort
            && !["low", "medium", "high", "xhigh", "max"].contains(&e.as_str())
        {
            return Err("the effort must be low, medium, high, xhigh or max".into());
        }
        if self.cpus.is_some_and(|c| !(1..=256).contains(&c)) {
            return Err("CPUs must be between 1 and 256".into());
        }
        if !(1024..=262_144).contains(&self.memory_mb) {
            return Err("memory must be between 1024 and 262144 MB".into());
        }
        if self.image.is_empty() {
            return Err("pick an image".into());
        }
        if self.allow_hosts.len() > MAX_ALLOW_HOSTS {
            return Err(format!("at most {MAX_ALLOW_HOSTS} allowed hosts"));
        }
        if self.integrations.len() > 50 || self.integrations.iter().any(|n| n.is_empty() || n.len() > 80) {
            return Err("at most 50 integrations".into());
        }
        let mut seen = std::collections::HashSet::new();
        for h in &self.allow_hosts {
            if !h.contains("${var:") && clean_host(h)? != *h {
                return Err(format!("{h:?} is not in normal form"));
            }
            if !seen.insert(h) {
                return Err(format!("{h} is listed twice"));
            }
        }
        Ok(())
    }
}

/// Turns a name into a file-safe id.
pub fn slug(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

fn read_dir(dir: &Path) -> Vec<Entry> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut out = vec![];
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "json")
            && let (Some(id), Ok(text)) = (p.file_stem().and_then(|s| s.to_str()), std::fs::read_to_string(&p))
            && let Ok(profile) = serde_json::from_str::<Profile>(&text)
            && profile.validate().is_ok()
        {
            out.push(Entry { id: id.to_string(), profile });
        }
    }
    out
}

/// All profiles, sorted by name.
pub fn list(user: &Path) -> Vec<Entry> {
    let mut v = read_dir(user);
    v.sort_by_key(|x| x.profile.name.to_lowercase());
    v
}

fn user_path(user: &Path, id: &str) -> Result<PathBuf, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err("bad profile id".into());
    }
    Ok(user.join(format!("{id}.json")))
}

/// Saves a profile. `id` is `None` for a new one (derived from the name). Returns the id.
pub fn save(user: &Path, id: Option<&str>, profile: &Profile) -> Result<String, String> {
    // Normalize the allowlist (case, blanks, duplicates) before checking it.
    let mut profile = profile.clone();
    let mut hosts: Vec<String> = vec![];
    for h in profile.allow_hosts.iter().filter(|h| !h.trim().is_empty()) {
        // a `${var:NAME}` is only known at run time, when the entry is checked again
        let h = if h.contains("${var:") { h.trim().to_string() } else { clean_host(h)? };
        if !hosts.contains(&h) {
            hosts.push(h);
        }
    }
    profile.allow_hosts = hosts;
    let profile = &profile;
    profile.validate()?;
    let id = match id {
        Some(id) => id.to_string(),
        None => {
            let base = slug(&profile.name);
            if base.is_empty() {
                return Err("the name needs letters or digits".into());
            }
            let mut id = base.clone();
            let mut n = 2;
            while user.join(format!("{id}.json")).exists() {
                id = format!("{base}-{n}");
                n += 1;
            }
            id
        }
    };
    let path = user_path(user, &id)?;
    crate::util::write_json(&path, profile)?;
    Ok(id)
}

pub fn delete(user: &Path, id: &str) -> Result<(), String> {
    std::fs::remove_file(user_path(user, id)?).map_err(|e| e.to_string())
}

/// Where a project's profiles are.
pub fn dir(app: &tauri::AppHandle, project: &str) -> Result<PathBuf, String> {
    Ok(Scope::project(project).dir(app)?.join("profiles"))
}

/// Ids of the images that the profiles of a project use.
pub fn used_images(app: &tauri::AppHandle, project: &str) -> Vec<String> {
    dir(app, project).map(|d| list(&d).into_iter().map(|e| e.profile.image).collect()).unwrap_or_default()
}

/// The starter profiles of the demo project (`demo/demo.yaml`): the ones that work on code (developer, architect, reviewer) on the `developer` image, `qa` on its own, the rest on the base image; existing ones are left alone.
///
/// `reset`: the starter profiles are written again over what is there (the edits of the user are lost, other profiles are left alone).
pub fn provision_defaults(app: &tauri::AppHandle, project: &str, reset: bool) -> Result<(), String> {
    let dir = dir(app, project)?;
    for (id, profile) in crate::demo::profiles() {
        if !reset && user_path(&dir, &id)?.exists() {
            continue;
        }
        save(&dir, Some(&id), &profile)?;
    }
    Ok(())
}

/// Page Agent profiles > Reset to demo content.
#[tauri::command]
pub fn profiles_reset_demo(app: tauri::AppHandle, project: String) -> Result<(), String> {
    provision_defaults(&app, &project, true)
}

#[tauri::command]
pub fn profiles_list(app: tauri::AppHandle, project: String) -> Result<Vec<Entry>, String> {
    Ok(list(&dir(&app, &project)?))
}

/// Creates (`id` null) or updates a profile; returns its id.
#[tauri::command]
pub fn profiles_save(app: tauri::AppHandle, project: String, id: Option<String>, profile: Profile) -> Result<String, String> {
    save(&dir(&app, &project)?, id.as_deref(), &profile)
}

#[tauri::command]
pub fn profiles_delete(app: tauri::AppHandle, project: String, id: String) -> Result<(), String> {
    delete(&dir(&app, &project)?, &id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str) -> Profile {
        Profile { name: name.into(), description: String::new(), system_prompt: "sp".into(), env: vec![], model: None, effort: None, cpus: None, memory_mb: DEFAULT_MEMORY_MB, image: "base".into(), allow_hosts: vec![], integrations: vec![] }
    }

    #[test]
    fn allowlist_is_normalized_and_validated() {
        let dir = tmp("allow");
        let mut x = p("a");
        x.allow_hosts = vec![" GitHub.com. ".into(), "".into(), "*.NPMjs.org".into(), "github.com".into()];
        let id = save(&dir, None, &x).unwrap();
        let saved = list(&dir).into_iter().find(|e| e.id == id).unwrap();
        assert_eq!(saved.profile.allow_hosts, vec!["github.com", "*.npmjs.org"]);
        for bad in ["1.2.3.4", "localhost", "https://a.com", "a.com:443", "a.com/x", "*", "a.*.com", "-a.com", "a b.com", "a..com"] {
            x.allow_hosts = vec![bad.into()];
            assert!(save(&dir, None, &x).is_err(), "{bad}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn allowlist_accepts_variable_references_unchecked() {
        let dir = tmp("allowvar");
        let mut x = p("a");
        x.allow_hosts = vec!["${var:ATLASSIAN_HOST}".into(), "github.com".into()];
        let id = save(&dir, None, &x).unwrap();
        let saved = list(&dir).into_iter().find(|e| e.id == id).unwrap();
        assert_eq!(saved.profile.allow_hosts, vec!["${var:ATLASSIAN_HOST}", "github.com"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ccaf-prof-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn demo_credentials_follow_least_privilege() {
        let demo_env = |id: &str| crate::demo::profiles().into_iter().find(|(i, _)| i == id).unwrap().1.env;
        let value = |id: &str, name: &str| demo_env(id).into_iter().find(|v| v.name == name).map(|v| v.value);
        assert_eq!(value("developer", "GH_TOKEN").as_deref(), Some("${injection:GITHUB}"));
        assert_eq!(value("reviewer", "GH_TOKEN").as_deref(), Some("${injection:GITHUB}"));
        assert_eq!(value("qa", "GH_TOKEN").as_deref(), Some("${injection:GITHUB_READONLY}"));
        assert_eq!(value("architect", "JIRA_API_TOKEN").as_deref(), Some("${injection:JIRA}"));
        assert_eq!(value("project-manager", "JIRA_API_TOKEN").as_deref(), Some("${injection:JIRA}"));
        assert!(demo_env("orchestrator").is_empty());
        for id in ["orchestrator", "project-manager", "business-analyst", "architect", "developer", "reviewer", "qa"] {
            assert!(validate_env(demo_env(id).iter().map(|v| (v.name.as_str(), Some(v.value.as_str())))).is_ok(), "{id}");
        }
    }

    #[test]
    fn validates_env() {
        let mut x = p("a");
        x.env = vec![EnvVar { name: "1BAD".into(), value: String::new() }];
        assert!(x.validate().is_err());
        x.env = vec![EnvVar { name: "PATH".into(), value: "/".into() }];
        assert!(x.validate().is_err());
        x.env = vec![EnvVar { name: "A".into(), value: String::new() }, EnvVar { name: "A".into(), value: String::new() }];
        assert!(x.validate().is_err());
        x.env = vec![EnvVar { name: "GH_TOKEN".into(), value: String::new() }, EnvVar { name: "LANG".into(), value: "C".into() }];
        assert!(x.validate().is_ok());
    }

    #[test]
    fn validates_model_and_effort() {
        let mut x = p("a");
        x.model = Some("claude-sonnet-5-5".into());
        x.effort = Some("xhigh".into());
        assert!(x.validate().is_ok());
        x.model = Some("bad model; rm".into());
        assert!(x.validate().is_err());
        x.model = Some(String::new());
        assert!(x.validate().is_err());
        x.model = None;
        x.effort = Some("extreme".into());
        assert!(x.validate().is_err());
        // old files have neither
        let old: Profile = serde_json::from_str(r#"{"name":"o"}"#).unwrap();
        assert!(old.model.is_none() && old.effort.is_none());
        assert_eq!((old.cpus, old.memory_mb), (None, 2048));
    }

    #[test]
    fn validates_resources() {
        let mut x = p("a");
        x.cpus = Some(4);
        x.memory_mb = 4096;
        assert!(x.validate().is_ok());
        x.cpus = Some(0);
        assert!(x.validate().is_err());
        x.cpus = None;
        x.memory_mb = 256;
        assert!(x.validate().is_err());
    }

    #[test]
    fn profiles_roundtrip() {
        let usr = tmp("usr");
        let id = save(&usr, None, &p("My Agent!")).unwrap();
        assert_eq!(id, "my-agent");
        assert_eq!(save(&usr, None, &p("My Agent")).unwrap(), "my-agent-2");
        assert_eq!(save(&usr, Some("my-agent"), &p("Renamed")).unwrap(), "my-agent");
        assert!(delete(&usr, "../x").is_err());
        let all = list(&usr);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].profile.name, "My Agent");
        delete(&usr, "my-agent").unwrap();
        assert_eq!(list(&usr).len(), 1);
    }

    #[test]
    fn starter_profiles_list_the_hosts_of_their_injections() {
        let demo = crate::demo::injections();
        for (id, profile) in crate::demo::profiles() {
            // as at run time, the variables are filled in first
            let fill = |h: &str| crate::config::secrets::resolve(h, "var", &|_| Some("me.atlassian.net".to_string())).unwrap();
            let rules = crate::net::egress::Rules::new([profile.allow_hosts.iter().map(|h| fill(h)).collect::<Vec<_>>()]);
            for v in &profile.env {
                let name = v.value.strip_prefix("${injection:").and_then(|n| n.strip_suffix('}'));
                let Some(i) = name.and_then(|n| demo.iter().find(|i| i.name == n)) else { continue };
                for r in &i.rules {
                    assert!(rules.covers(&fill(&r.host)), "{id} uses {} but does not list {}", i.name, r.host);
                }
            }
        }
    }
}
