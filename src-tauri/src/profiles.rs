//! Agent profiles: how an agent behaves and which image it runs on. A profile has a name, a
//! system prompt, environment variables and the id of an image (see `images.rs`; `base` is the
//! built-in one). The user creates them in the UI; each is `<id>.json` in the app data dir. The
//! app ships none.
//!
//! Everything in a profile travels in the `Task` when a run starts, nothing is baked into an
//! image. An environment variable with a value is sent as is; one without is supplied by the run
//! engine at that moment (e.g. a token from the keyring).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Variables the app or agentd own; a profile may not set them.
const RESERVED: &[&str] = &["CLAUDE_CODE_OAUTH_TOKEN", "PATH", "HOME", "USER", "LOGNAME", "SHELL", "USE_BUILTIN_RIPGREP"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvVar {
    pub name: String,
    /// `None`: supplied by the run engine when the task starts.
    #[serde(default)]
    pub value: Option<String>,
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
    /// The image the agent runs on.
    #[serde(default = "base_image")]
    pub image: String,
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
        validate_env(self.env.iter().map(|v| (v.name.as_str(), v.value.as_deref())))?;
        if self.system_prompt.len() > 64 * 1024 {
            return Err("the system prompt is too large".into());
        }
        if self.image.is_empty() {
            return Err("pick an image".into());
        }
        Ok(())
    }

    #[allow(dead_code)] // used by the run engine
    /// Names of the variables the app must supply for each run.
    pub fn runtime_env(&self) -> Vec<&str> {
        self.env.iter().filter(|v| v.value.is_none()).map(|v| v.name.as_str()).collect()
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
        Profile { name: name.into(), description: String::new(), system_prompt: "sp".into(), env: vec![], image: "base".into() }
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
        x.env = vec![EnvVar { name: "1BAD".into(), value: None }];
        assert!(x.validate().is_err());
        x.env = vec![EnvVar { name: "PATH".into(), value: Some("/".into()) }];
        assert!(x.validate().is_err());
        x.env = vec![EnvVar { name: "A".into(), value: None }, EnvVar { name: "A".into(), value: None }];
        assert!(x.validate().is_err());
        x.env = vec![EnvVar { name: "GH_TOKEN".into(), value: None }, EnvVar { name: "LANG".into(), value: Some("C".into()) }];
        assert!(x.validate().is_ok());
        assert_eq!(x.runtime_env(), ["GH_TOKEN"]);
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
