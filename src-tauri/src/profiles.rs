//! Agent profiles: what an agent VM is set up with on top of the base image. A profile has a
//! name, a system prompt, environment variables and a customization script (run as root in a
//! builder VM at image build). Presets ship with the app (read-only, buildable); user profiles
//! live in the app data dir as `<id>.json`.
//!
//! An environment variable with a value is baked into the image; one without is injected by the
//! app when the task starts (the run engine supplies the value, e.g. from the keyring).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Variables the app or agentd own; a profile may not set them.
const RESERVED: &[&str] = &["CLAUDE_CODE_OAUTH_TOKEN", "PATH", "HOME", "USER", "LOGNAME", "SHELL", "USE_BUILTIN_RIPGREP"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvVar {
    pub name: String,
    /// `None`: injected at run time.
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
    /// Shell script run as root (busybox ash) in the builder VM after the base image boots.
    #[serde(default)]
    pub script: String,
}

/// A profile as listed to the UI.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub id: String,
    pub preset: bool,
    #[serde(flatten)]
    pub profile: Profile,
}

impl Profile {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.name.len() > 80 {
            return Err("the name must be 1 to 80 characters".into());
        }
        let mut seen = std::collections::HashSet::new();
        for v in &self.env {
            let ok = v.name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && v.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !ok {
                return Err(format!("{:?} is not a valid variable name", v.name));
            }
            if RESERVED.contains(&v.name.as_str()) || v.name.starts_with("LD_") {
                return Err(format!("{} is reserved", v.name));
            }
            if !seen.insert(&v.name) {
                return Err(format!("{} is set twice", v.name));
            }
            if v.value.as_deref().is_some_and(|s| s.contains('\0')) {
                return Err(format!("{} has a NUL in its value", v.name));
            }
        }
        if self.system_prompt.len() > 64 * 1024 || self.script.len() > 256 * 1024 {
            return Err("the system prompt or script is too large".into());
        }
        Ok(())
    }

    #[allow(dead_code)] // used by the run engine
    /// Names of the variables the app must supply for each run.
    pub fn runtime_env(&self) -> Vec<&str> {
        self.env.iter().filter(|v| v.value.is_none()).map(|v| v.name.as_str()).collect()
    }

    #[allow(dead_code)] // used by the image builder
    /// Identifies the image this profile builds: everything baked into it plus the base image's
    /// stamp. The name, description and runtime variables do not change the image.
    pub fn image_key(&self, base_stamp: &str) -> String {
        let mut baked: Vec<_> = self.env.iter().filter_map(|v| v.value.as_ref().map(|x| (&v.name, x))).collect();
        baked.sort();
        let spec = serde_json::json!([base_stamp, self.system_prompt, baked, self.script]);
        let digest = Sha256::digest(spec.to_string());
        digest[..8].iter().map(|b| format!("{b:02x}")).collect()
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

fn read_dir(dir: &Path, preset: bool) -> Vec<Entry> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut out = vec![];
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "json")
            && let (Some(id), Ok(text)) = (p.file_stem().and_then(|s| s.to_str()), std::fs::read_to_string(&p))
            && let Ok(profile) = serde_json::from_str::<Profile>(&text)
            && profile.validate().is_ok()
        {
            out.push(Entry { id: id.to_string(), preset, profile });
        }
    }
    out
}

/// Presets first, then user profiles, each sorted by name. A user profile never shadows a preset.
pub fn list(presets: &Path, user: &Path) -> Vec<Entry> {
    let mut a = read_dir(presets, true);
    let ids: Vec<_> = a.iter().map(|e| e.id.clone()).collect();
    let mut b: Vec<_> = read_dir(user, false).into_iter().filter(|e| !ids.contains(&e.id)).collect();
    for v in [&mut a, &mut b] {
        v.sort_by_key(|x| x.profile.name.to_lowercase());
    }
    a.extend(b);
    a
}

fn user_path(user: &Path, id: &str) -> Result<PathBuf, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err("bad profile id".into());
    }
    Ok(user.join(format!("{id}.json")))
}

/// Saves a user profile. `id` is `None` for a new one (derived from the name). Presets cannot be
/// overwritten. Returns the id.
pub fn save(presets: &Path, user: &Path, id: Option<&str>, profile: &Profile) -> Result<String, String> {
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
            while presets.join(format!("{id}.json")).exists() || user.join(format!("{id}.json")).exists() {
                id = format!("{base}-{n}");
                n += 1;
            }
            id
        }
    };
    if presets.join(format!("{id}.json")).exists() {
        return Err("presets cannot be edited".into());
    }
    let path = user_path(user, &id)?;
    std::fs::create_dir_all(user).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(profile).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(id)
}

pub fn delete(presets: &Path, user: &Path, id: &str) -> Result<(), String> {
    if presets.join(format!("{id}.json")).exists() {
        return Err("presets cannot be deleted".into());
    }
    std::fs::remove_file(user_path(user, id)?).map_err(|e| e.to_string())
}

fn dirs(app: &tauri::AppHandle) -> Result<(PathBuf, PathBuf), String> {
    use tauri::Manager;
    let presets = app.path().resource_dir().map_err(|e| e.to_string())?.join("profiles");
    let user = app.path().app_data_dir().map_err(|e| e.to_string())?.join("profiles");
    Ok((presets, user))
}

#[tauri::command]
pub fn profiles_list(app: tauri::AppHandle) -> Result<Vec<Entry>, String> {
    let (presets, user) = dirs(&app)?;
    Ok(list(&presets, &user))
}

/// Creates (`id` null) or updates a user profile; returns its id.
#[tauri::command]
pub fn profiles_save(app: tauri::AppHandle, id: Option<String>, profile: Profile) -> Result<String, String> {
    let (presets, user) = dirs(&app)?;
    save(&presets, &user, id.as_deref(), &profile)
}

#[tauri::command]
pub fn profiles_delete(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let (presets, user) = dirs(&app)?;
    delete(&presets, &user, &id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str) -> Profile {
        Profile { name: name.into(), description: String::new(), system_prompt: "sp".into(), env: vec![], script: "echo hi".into() }
    }

    fn dir(tag: &str) -> PathBuf {
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
    fn image_key_tracks_only_what_is_baked() {
        let a = p("a");
        let mut b = p("b");
        b.description = "other".into();
        b.env = vec![EnvVar { name: "TOKEN".into(), value: None }];
        assert_eq!(a.image_key("s1"), b.image_key("s1"), "name, description and runtime vars are not baked");
        assert_ne!(a.image_key("s1"), a.image_key("s2"), "a new base changes the key");
        let mut c = p("a");
        c.script = "echo other".into();
        assert_ne!(a.image_key("s1"), c.image_key("s1"));
        let mut d = p("a");
        d.env = vec![EnvVar { name: "X".into(), value: Some("1".into()) }];
        assert_ne!(a.image_key("s1"), d.image_key("s1"));
    }

    #[test]
    fn user_profiles_roundtrip_and_presets_are_protected() {
        let (pre, usr) = (dir("pre"), dir("usr"));
        std::fs::write(pre.join("dev.json"), serde_json::to_string(&p("Dev")).unwrap()).unwrap();
        let id = save(&pre, &usr, None, &p("My Agent!")).unwrap();
        assert_eq!(id, "my-agent");
        assert_eq!(save(&pre, &usr, None, &p("My Agent")).unwrap(), "my-agent-2");
        assert_eq!(save(&pre, &usr, None, &p("dev")).unwrap(), "dev-2", "a new profile never takes a preset's id");
        assert!(save(&pre, &usr, Some("dev"), &p("x")).is_err());
        assert!(delete(&pre, &usr, "dev").is_err());
        assert!(delete(&pre, &usr, "../x").is_err());
        let all = list(&pre, &usr);
        assert!(all[0].preset && all[0].id == "dev");
        assert_eq!(all.len(), 4);
        delete(&pre, &usr, "my-agent").unwrap();
        assert_eq!(list(&pre, &usr).len(), 3);
    }
}
