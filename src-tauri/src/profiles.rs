//! Agent profiles: how an agent behaves and which image it runs on. A profile has a name, a
//! system prompt, environment variables and the id of an image (see `images.rs`; `base` is the
//! built-in one). The user creates them in the UI; each is `<id>.json` in the app data dir. The
//! app ships none, except the starter ones the user accepts on first start.
//!
//! Everything in a profile travels in the `Task` when a run starts, nothing is baked into an
//! image (variables baked into an image are set on the image itself).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
        let mut seen = std::collections::HashSet::new();
        for h in &self.allow_hosts {
            if clean_host(h)? != *h {
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
        let h = clean_host(h)?;
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
    std::fs::create_dir_all(user).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(profile).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(id)
}

pub fn delete(user: &Path, id: &str) -> Result<(), String> {
    std::fs::remove_file(user_path(user, id)?).map_err(|e| e.to_string())
}

fn dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("profiles"))
}

/// Ids of the images that profiles use.
pub fn used_images(app: &tauri::AppHandle) -> Vec<String> {
    dir(app).map(|d| list(&d).into_iter().map(|e| e.profile.image).collect()).unwrap_or_default()
}

/// The demo profiles' credentials, as injections (see `injections::demo`): only the developer
/// writes to GitHub (the others get the read-only variant); Jira and Confluence have one token each.
fn demo_env(id: &str) -> Vec<EnvVar> {
    let var = |name: &str, injection: &str| EnvVar { name: name.into(), value: format!("${{injection:{injection}}}") };
    let github = match id {
        "developer" => Some("GITHUB"),
        "architect" | "reviewer" | "qa" => Some("GITHUB_READONLY"),
        _ => None,
    };
    let atlassian = matches!(id, "project-manager" | "business-analyst" | "architect" | "developer" | "reviewer" | "qa");
    let mut env = vec![];
    if let Some(g) = github {
        env.push(var("GH_TOKEN", g));
    }
    if atlassian {
        env.push(var("JIRA_API_TOKEN", "JIRA"));
        env.push(var("CFL_API_TOKEN", "CONFLUENCE"));
        // jtk and cfl both read these; the site and e-mail are the demo variables (Settings > Variables), shared with the injections
        env.push(EnvVar { name: "ATLASSIAN_URL".into(), value: "https://${var:ATLASSIAN_HOST}".into() });
        env.push(EnvVar { name: "ATLASSIAN_EMAIL".into(), value: "${var:ATLASSIAN_EMAIL}".into() });
    }
    env
}

/// The starter profiles: the ones that work on code (developer, architect, reviewer) on the `developer` image, `qa` on its own, the rest on the base image; existing ones are left alone.
pub fn provision_defaults(app: &tauri::AppHandle) -> Result<(), String> {
    let dir = dir(app)?;
    let starters = [
        (
            "orchestrator",
            "Orchestrator",
            "Takes the user's request and hands it to the right agent; reports the outcome back.",
            "You are the orchestrator of a delivery team. You do not write code. You restate the request precisely (goal, constraints, what done means), hand it to the agents you may call, and when they report back you compare what was delivered with the request and say plainly what is finished, what was tested and what is left. Ask the user only when you cannot go on without the answer; otherwise assume and say what you assumed.",
        ),
        (
            "project-manager",
            "Project manager",
            "Breaks a request into work items and coordinates the team that delivers each one.",
            "You are a project manager. You turn a request into work items, fetch them from the tracker if the request refers to one, and see each one through, one at a time, by coordinating specialists (business analyst, architect, developer, reviewer, tester). You do not write code yourself. You give each specialist everything it needs, because it knows nothing else; you pass identifiers (branch, pull request, build) from one to the next; when a specialist reports a problem you decide whether to send the work back, go on or give up. You keep track of what is done, blocked and left, and you report briefly and concretely.",
        ),
        (
            "business-analyst",
            "Business analyst",
            "Writes testable requirements.",
            "You are a business analyst. From the item you are given, write the requirements as user stories, each with acceptance criteria a tester can check, and list the edge cases and what is explicitly out of scope. Make reasonable assumptions and list them instead of stopping to ask. You do not write code.",
        ),
        (
            "architect",
            "Architect",
            "Designs a change before it is implemented.",
            "You are a software architect. Read the code and design the change you are given: which parts change and how, the interfaces involved, the risks and the order of the work. Keep the design as small as the requirements allow. You do not write code.",
        ),
        (
            "developer",
            "Developer",
            "Implements a task in a git repository.",
            "You are a software developer. Implement the task you are given: read the code first, make focused changes that follow the project's conventions, run the tests and the build, and fix what fails. Work in a branch, commit with clear messages and open a pull request. If something is unclear and you cannot find it out yourself, assume the most reasonable thing and say so in your report instead of stopping.",
        ),
        (
            "reviewer",
            "Code reviewer",
            "Reviews a pull request.",
            "You are a code reviewer. Read the pull request and the code around it, check it against its requirements, and look for bugs, missing tests and deviations from the project's conventions. Leave your comments on the pull request itself, then report whether you approve it or ask for changes, with the list of changes. Do not push to the branch.",
        ),
        (
            "qa",
            "Manual QA",
            "Tests an app by hand in a headless browser.",
            "You are a manual QA tester. Exercise the application the way a user would, using agent-browser (a headless Chromium; run `agent-browser --help` for its commands): follow the scenarios you are given, try edge cases and bad input, and check what you see against what is expected. Report each problem with the steps to reproduce it, what you expected and what happened. Do not change the code under test.",
        ),
    ];
    for (id, name, description, prompt) in starters {
        if user_path(&dir, id)?.exists() {
            continue;
        }
        let profile = Profile {
            name: name.into(),
            description: description.into(),
            system_prompt: prompt.into(),
            // the credentials come from the demo injections (Settings > Injections): the agent only sees a placeholder
            env: demo_env(id),
            model: None,
            effort: None,
            cpus: None,
            memory_mb: DEFAULT_MEMORY_MB,
            image: match id {
                "qa" => "qa".into(),
                "developer" | "architect" | "reviewer" => "developer".into(),
                "project-manager" | "business-analyst" => "manager".into(),
                _ => "base".into(),
            },
            // the developer tools (git, npm, pip) need their registries once the network is sealed
            allow_hosts: match id {
                "developer" | "architect" | "reviewer" | "qa" => {
                    ["github.com", "*.githubusercontent.com", "registry.npmjs.org", "pypi.org", "files.pythonhosted.org"].map(String::from).to_vec()
                }
                _ => vec![],
            },
        };
        save(&dir, Some(id), &profile)?;
    }
    Ok(())
}

/// First-start setup the user accepted: the starter images (building them), profiles, secrets and workflow.
#[tauri::command]
pub fn provision_starters(app: tauri::AppHandle) -> Result<(), String> {
    crate::images::provision_defaults(&app)?;
    provision_defaults(&app)?;
    crate::secrets::provision_demo(&app)?;
    crate::variables::provision_demo(&app)?;
    crate::injections::provision_demo(&app)?;
    crate::workflow::provision_defaults(&app)
}

#[tauri::command]
pub fn profiles_list(app: tauri::AppHandle) -> Result<Vec<Entry>, String> {
    Ok(list(&dir(&app)?))
}

/// Creates (`id` null) or updates a profile; returns its id.
#[tauri::command]
pub fn profiles_save(app: tauri::AppHandle, id: Option<String>, profile: Profile) -> Result<String, String> {
    save(&dir(&app)?, id.as_deref(), &profile)
}

#[tauri::command]
pub fn profiles_delete(app: tauri::AppHandle, id: String) -> Result<(), String> {
    delete(&dir(&app)?, &id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str) -> Profile {
        Profile { name: name.into(), description: String::new(), system_prompt: "sp".into(), env: vec![], model: None, effort: None, cpus: None, memory_mb: DEFAULT_MEMORY_MB, image: "base".into(), allow_hosts: vec![] }
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

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ccaf-prof-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn demo_credentials_follow_least_privilege() {
        let value = |id: &str, name: &str| demo_env(id).into_iter().find(|v| v.name == name).map(|v| v.value);
        assert_eq!(value("developer", "GH_TOKEN").as_deref(), Some("${injection:GITHUB}"));
        assert_eq!(value("reviewer", "GH_TOKEN").as_deref(), Some("${injection:GITHUB_READONLY}"));
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
}
