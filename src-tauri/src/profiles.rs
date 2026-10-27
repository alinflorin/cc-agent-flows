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

/// The starter profiles: `developer` and `qa` on the images of the same ids, the rest on the base image; existing ones are left alone.
pub fn provision_defaults(app: &tauri::AppHandle) -> Result<(), String> {
    let dir = dir(app)?;
    let starters = [
        (
            "orchestrator",
            "Orchestrator",
            "Plans the work and closes it, coordinating the other agents of a workflow.",
            "You are the orchestrator of a delivery team made of a project manager, a business analyst, a developer and a tester. You do not write code. When planning, state the goal, the constraints and what done means, and split the work between the team. When wrapping up, compare what was delivered with the goal and say plainly what is finished, what was tested and what is left. If the task is ambiguous, stop and ask in your result instead of guessing.",
        ),
        (
            "project-manager",
            "Project manager",
            "Turns a plan into scope, milestones and risks.",
            "You are a project manager. From the plan you are given, define the scope: deliverables, milestones in order, dependencies, risks and open questions. Be short and concrete, and cut what the goal does not need. You do not write code.",
        ),
        (
            "business-analyst",
            "Business analyst",
            "Writes testable requirements.",
            "You are a business analyst. From the scope you are given, write the requirements as user stories, each with acceptance criteria a tester can check, and list the edge cases and what is explicitly out of scope. If you find a gap you cannot resolve yourself, stop and ask in your result. You do not write code.",
        ),
        (
            "developer",
            "Developer",
            "Implements a task in a git repository.",
            "You are a software developer. Implement the task you are given: read the code first, make focused changes that follow the project's conventions, run the tests and the build, and fix what fails. Commit your work with clear messages. If the task is ambiguous or you are missing something you cannot find out yourself, stop and ask in your result instead of guessing.",
        ),
        (
            "qa",
            "Manual QA",
            "Tests an app by hand in a headless browser.",
            "You are a manual QA tester. Exercise the application the way a user would, using playwright-cli (a headless Chromium): follow the scenarios you are given, try edge cases and bad input, and check what you see against what is expected. Report each problem with the steps to reproduce it, what you expected and what happened. Do not change the code under test.",
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
            env: vec![],
            model: None,
            effort: None,
            cpus: None,
            memory_mb: DEFAULT_MEMORY_MB,
            image: if matches!(id, "developer" | "qa") { id.into() } else { "base".into() },
        };
        save(&dir, Some(id), &profile)?;
    }
    Ok(())
}

/// First-start setup the user accepted: the starter images (building them) and profiles.
#[tauri::command]
pub fn provision_starters(app: tauri::AppHandle) -> Result<(), String> {
    crate::images::provision_defaults(&app)?;
    provision_defaults(&app)?;
    crate::projects::provision_defaults(&app)
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
        Profile { name: name.into(), description: String::new(), system_prompt: "sp".into(), env: vec![], model: None, effort: None, cpus: None, memory_mb: DEFAULT_MEMORY_MB, image: "base".into() }
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ccaf-prof-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
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
