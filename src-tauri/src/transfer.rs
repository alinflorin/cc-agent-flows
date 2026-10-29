//! Export and import of everything the app holds, as one encrypted YAML file: the global settings
//! and every project (variables, secrets with their values, injections, integrations, network,
//! agent profiles, workflows, image definitions) and the Claude accounts with their tokens. Not
//! exported: session logs, built image disks, run logs, app preferences.
//!
//! The file is YAML with a small header and one `ciphertext` field: a key from the password
//! (Argon2id, random salt) encrypts the whole bundle with XChaCha20-Poly1305, so nothing is
//! readable without it and a wrong password or a changed file is refused. The plain bundle exists
//! only in this module's memory: the frontend sends the password and gets back a summary, the file
//! is chosen and written here, through the native dialog.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri_plugin_dialog::DialogExt;

use crate::config::{accounts, projects, scope::Scope, secrets};

const FORMAT: &str = "cc-agent-flows-export";
const VERSION: u32 = 1;
const MIN_PASSWORD: usize = 8;
/// Argon2id: 64 MiB, 3 passes, 1 lane.
const M_KIB: u32 = 64 * 1024;
const T_COST: u32 = 3;
const P_COST: u32 = 1;
const MAX_FILE: u64 = 64 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Envelope {
    format: String,
    version: u32,
    kdf: Kdf,
    cipher: CipherInfo,
    ciphertext: String,
}

#[derive(Serialize, Deserialize)]
struct Kdf {
    name: String,
    m_kib: u32,
    t: u32,
    p: u32,
    salt: String,
}

#[derive(Serialize, Deserialize)]
struct CipherInfo {
    name: String,
    nonce: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Bundle {
    app_version: String,
    created: u64,
    /// Path relative to the app data dir (`/` separated) -> the file's text.
    files: BTreeMap<String, String>,
    secrets: Vec<SecretItem>,
    /// Claude account id -> setup token.
    claude_tokens: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct SecretItem {
    project: Option<String>,
    name: String,
    value: String,
}

#[derive(Serialize, Default, Debug)]
pub struct Summary {
    /// Files written or merged into.
    pub written: usize,
    /// Files and items left as they were (they exist and `overwrite` was off).
    pub skipped: usize,
    pub secrets: usize,
    pub tokens: usize,
}

// ---- encryption ----

fn derive(password: &str, salt: &[u8], m: u32, t: u32, p: u32) -> Result<[u8; 32], String> {
    let params = argon2::Params::new(m, t, p, Some(32)).map_err(|e| e.to_string())?;
    let mut key = [0u8; 32];
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| e.to_string())?;
    Ok(key)
}

fn aad() -> String {
    format!("{FORMAT}:{VERSION}")
}

fn encrypt(password: &str, plain: &[u8]) -> Result<String, String> {
    let salt: [u8; 16] = rand::random();
    let nonce: [u8; 24] = rand::random();
    let key = derive(password, &salt, M_KIB, T_COST, P_COST)?;
    let cipher = XChaCha20Poly1305::new_from_slice(&key).map_err(|e| e.to_string())?;
    let ciphertext = cipher
        .encrypt(&XNonce::from(nonce), Payload { msg: plain, aad: aad().as_bytes() })
        .map_err(|_| "encryption failed".to_string())?;
    let envelope = Envelope {
        format: FORMAT.into(),
        version: VERSION,
        kdf: Kdf { name: "argon2id".into(), m_kib: M_KIB, t: T_COST, p: P_COST, salt: B64.encode(salt) },
        cipher: CipherInfo { name: "xchacha20poly1305".into(), nonce: B64.encode(nonce) },
        ciphertext: B64.encode(ciphertext),
    };
    serde_yaml_ng::to_string(&envelope).map_err(|e| e.to_string())
}

fn decrypt(password: &str, text: &str) -> Result<Vec<u8>, String> {
    let bad = || "this is not a CC Agent Flows export".to_string();
    let envelope: Envelope = serde_yaml_ng::from_str(text).map_err(|_| bad())?;
    if envelope.format != FORMAT {
        return Err(bad());
    }
    if envelope.version != VERSION || envelope.kdf.name != "argon2id" || envelope.cipher.name != "xchacha20poly1305" {
        return Err("this export was made by a newer version of the app".into());
    }
    // the cost comes from the file: cap it so a crafted file cannot exhaust memory
    if envelope.kdf.m_kib > 1024 * 1024 || envelope.kdf.t > 20 || envelope.kdf.p > 8 {
        return Err("the export asks for too much work to open".into());
    }
    let salt = B64.decode(&envelope.kdf.salt).map_err(|_| bad())?;
    let nonce = B64.decode(&envelope.cipher.nonce).map_err(|_| bad())?;
    let data = B64.decode(&envelope.ciphertext).map_err(|_| bad())?;
    let nonce: [u8; 24] = nonce.try_into().map_err(|_| bad())?;
    if salt.len() < 8 {
        return Err(bad());
    }
    let key = derive(password, &salt, envelope.kdf.m_kib, envelope.kdf.t, envelope.kdf.p)?;
    let cipher = XChaCha20Poly1305::new_from_slice(&key).map_err(|e| e.to_string())?;
    cipher
        .decrypt(&XNonce::from(nonce), Payload { msg: &data, aad: aad().as_bytes() })
        .map_err(|_| "wrong password, or the file was changed".to_string())
}

// ---- what is in a bundle ----

const SCOPE_FILES: [&str; 5] = ["variables.json", "secrets.json", "injections.json", "integrations.json", "network.json"];
const SCOPE_DIRS: [&str; 3] = ["profiles", "workflows", "images/specs"];

fn plain_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 80 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Splits a bundle path into the project it belongs to (`None` = global) and the path inside the
/// scope, if it is one of the files an export holds. Anything else is refused.
fn classify(rel: &str) -> Option<(Option<&str>, &str)> {
    let (project, inner) = match rel.strip_prefix("projects/") {
        Some(rest) => {
            let (id, inner) = rest.split_once('/')?;
            if !projects::valid_id(id) {
                return None;
            }
            (Some(id), inner)
        }
        None => (None, rel),
    };
    let top = SCOPE_FILES.contains(&inner)
        || (project.is_none() && (inner == "webhook.json" || inner == "claude-accounts.json" || inner == "vm.json" || inner == "system_prompt.json"))
        || (project.is_some() && inner == projects::FILE);
    let in_dir = SCOPE_DIRS.iter().any(|d| {
        inner.strip_prefix(d).and_then(|r| r.strip_prefix('/')).and_then(|f| f.strip_suffix(".json")).is_some_and(plain_id)
    });
    (top || in_dir).then_some((project, inner))
}

fn read_scope(dir: &Path, prefix: &str, global: bool, out: &mut BTreeMap<String, String>) {
    let mut names: Vec<String> = SCOPE_FILES.iter().map(|s| s.to_string()).collect();
    if global {
        names.extend(["webhook.json".to_string(), "claude-accounts.json".to_string(), "system_prompt.json".to_string()]);
    } else {
        names.push(projects::FILE.to_string());
    }
    for n in names {
        if let Ok(text) = std::fs::read_to_string(dir.join(&n)) {
            out.insert(format!("{prefix}{n}"), text);
        }
    }
    for d in SCOPE_DIRS {
        let Ok(rd) = std::fs::read_dir(dir.join(d)) else { continue };
        for f in rd.flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            if name.strip_suffix(".json").is_some_and(plain_id) && let Ok(text) = std::fs::read_to_string(f.path()) {
                out.insert(format!("{prefix}{d}/{name}"), text);
            }
        }
    }
}

/// The portable part of a `vm.json` text (`None` if it is not an object or has none of them).
fn portable_vm(text: &str) -> Option<String> {
    let Value::Object(map) = serde_json::from_str::<Value>(text).ok()? else { return None };
    let kept: serde_json::Map<String, Value> = map.into_iter().filter(|(k, _)| crate::config::vm_settings::PORTABLE.contains(&k.as_str())).collect();
    (!kept.is_empty()).then(|| Value::Object(kept).to_string())
}

fn secret_names(files: &BTreeMap<String, String>, key: &str) -> Vec<String> {
    files
        .get(key)
        .and_then(|t| serde_json::from_str::<Value>(t).ok())
        .and_then(|v| v.get("names").cloned())
        .and_then(|n| serde_json::from_value::<Vec<String>>(n).ok())
        .unwrap_or_default()
}

fn collect(app: &tauri::AppHandle) -> Result<Bundle, String> {
    let data = crate::util::data_dir(app)?;
    let mut bundle = Bundle { app_version: env!("CARGO_PKG_VERSION").into(), created: crate::util::unix_secs(), ..Default::default() };
    read_scope(&data, "", true, &mut bundle.files);
    let mut project_ids = vec![];
    if let Ok(rd) = std::fs::read_dir(data.join("projects")) {
        for e in rd.flatten() {
            let id = e.file_name().to_string_lossy().to_string();
            if projects::valid_id(&id) && e.path().join(projects::FILE).is_file() {
                project_ids.push(id);
            }
        }
    }
    project_ids.sort();
    for id in &project_ids {
        read_scope(&projects::dir_in(&data, id), &format!("projects/{id}/"), false, &mut bundle.files);
    }
    // only the VM settings that mean the same on another computer
    if let Some(text) = std::fs::read_to_string(data.join("vm.json")).ok().and_then(|t| portable_vm(&t)) {
        bundle.files.insert("vm.json".into(), text);
    }
    for name in secret_names(&bundle.files, "secrets.json") {
        if let Some(value) = secrets::raw_value(&Scope::global(), &name) {
            bundle.secrets.push(SecretItem { project: None, name, value });
        }
    }
    for id in &project_ids {
        for name in secret_names(&bundle.files, &format!("projects/{id}/secrets.json")) {
            if let Some(value) = secrets::raw_value(&Scope::project(id), &name) {
                bundle.secrets.push(SecretItem { project: Some(id.clone()), name, value });
            }
        }
    }
    let mut ids = vec![accounts::DEFAULT.to_string()];
    if let Some(v) = bundle.files.get("claude-accounts.json").and_then(|t| serde_json::from_str::<Value>(t).ok()) {
        for a in v.get("accounts").and_then(Value::as_array).into_iter().flatten() {
            if let Some(id) = a.get("id").and_then(Value::as_str) {
                ids.push(id.to_string());
            }
        }
    }
    ids.sort();
    ids.dedup();
    for id in ids {
        if let Some(t) = accounts::raw_token(&id) {
            bundle.claude_tokens.insert(id, t);
        }
    }
    Ok(bundle)
}

// ---- import ----

/// Adds what `incoming` has to `existing`: items of a list or map that are not there yet, and, with
/// `overwrite`, replaces the ones that are. Nothing is ever removed. `None`: the file has no item
/// structure (it is replaced as a whole or left alone).
fn merge(file: &str, existing: Value, incoming: Value, overwrite: bool, skipped: &mut usize) -> Option<Value> {
    fn by_key(existing: Vec<Value>, incoming: Vec<Value>, key: &str, overwrite: bool, skipped: &mut usize) -> Vec<Value> {
        let mut out = existing;
        for item in incoming {
            let k = item.get(key).cloned();
            match out.iter().position(|x| x.get(key) == k.as_ref()) {
                Some(i) if overwrite => out[i] = item,
                Some(_) => *skipped += 1,
                None => out.push(item),
            }
        }
        out
    }
    match (file, existing, incoming) {
        ("variables.json", Value::Object(mut e), Value::Object(i)) => {
            for (k, v) in i {
                if overwrite || !e.contains_key(&k) {
                    e.insert(k, v);
                } else {
                    *skipped += 1;
                }
            }
            Some(Value::Object(e))
        }
        ("secrets.json", Value::Object(mut e), Value::Object(i)) => {
            let mut names: Vec<Value> = e.get("names").and_then(Value::as_array).cloned().unwrap_or_default();
            for n in i.get("names").and_then(Value::as_array).into_iter().flatten() {
                if !names.contains(n) {
                    names.push(n.clone());
                }
            }
            e.insert("names".into(), Value::Array(names));
            Some(Value::Object(e))
        }
        ("injections.json" | "integrations.json", Value::Array(e), Value::Array(i)) => Some(Value::Array(by_key(e, i, "name", overwrite, skipped))),
        ("system_prompt.json", Value::Object(mut e), Value::Object(i)) => {
            // one text: the file's replaces ours only when asked to, or when ours is empty
            let empty = e.get("system_prompt").and_then(Value::as_str).is_none_or(|t| t.trim().is_empty());
            match i.get("system_prompt") {
                Some(v) if overwrite || empty => {
                    e.insert("system_prompt".into(), v.clone());
                }
                _ => *skipped += 1,
            }
            Some(Value::Object(e))
        }
        ("vm.json", Value::Object(mut e), Value::Object(i)) => {
            for (k, v) in i {
                if overwrite || !e.contains_key(&k) {
                    e.insert(k, v);
                } else {
                    *skipped += 1;
                }
            }
            Some(Value::Object(e))
        }
        ("network.json", Value::Object(mut e), Value::Object(i)) => {
            let mut hosts: Vec<Value> = e.get("allow_hosts").and_then(Value::as_array).cloned().unwrap_or_default();
            for h in i.get("allow_hosts").and_then(Value::as_array).into_iter().flatten() {
                if !hosts.contains(h) {
                    hosts.push(h.clone());
                }
            }
            e.insert("allow_hosts".into(), Value::Array(hosts));
            // the limits and the leak switch of the import win only when asked to replace
            for k in ["max_connections", "leak_detection"] {
                if let Some(v) = i.get(k)
                    && (overwrite || !e.contains_key(k))
                {
                    e.insert(k.into(), v.clone());
                }
            }
            Some(Value::Object(e))
        }
        ("claude-accounts.json", Value::Object(mut e), Value::Object(i)) => {
            let own = e.get("accounts").and_then(Value::as_array).cloned().unwrap_or_default();
            let theirs = i.get("accounts").and_then(Value::as_array).cloned().unwrap_or_default();
            e.insert("accounts".into(), Value::Array(by_key(own, theirs, "id", overwrite, skipped)));
            if overwrite && let Some(m) = i.get("mode") {
                e.insert("mode".into(), m.clone());
            }
            Some(Value::Object(e))
        }
        _ => None,
    }
}

fn write_text(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| e.to_string())
}

fn apply(app: &tauri::AppHandle, bundle: Bundle, overwrite: bool) -> Result<Summary, String> {
    let data = crate::util::data_dir(app)?;
    let mut sum = Summary::default();
    // validate everything before the first write
    let mut entries = vec![];
    for (rel, text) in &bundle.files {
        let (project, inner) = classify(rel).ok_or_else(|| format!("the file contains an entry the app does not know ({rel})"))?;
        let value: Value = serde_json::from_str(text).map_err(|_| format!("{rel} in the file is damaged"))?;
        // a file cannot set the VM settings that belong to this computer
        let (value, text) = if inner == "vm.json" {
            match portable_vm(text) {
                Some(t) => (serde_json::from_str(&t).unwrap_or(Value::Null), t),
                None => continue,
            }
        } else {
            (value, text.clone())
        };
        entries.push((project.map(str::to_string), inner.to_string(), value, text));
    }
    // the global files first, then each project's
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let global_specs = data.join("images").join("specs");
    for (project, inner, incoming, text) in entries {
        let dir = match &project {
            Some(id) => projects::dir_in(&data, id),
            None => data.clone(),
        };
        let path = dir.join(&inner);
        // a project cannot have an image id that a global image has
        if project.is_some() && inner.starts_with("images/specs/") && global_specs.join(inner.rsplit('/').next().unwrap_or("")).exists() {
            sum.skipped += 1;
            continue;
        }
        // a project's files only make sense with its project.json
        if project.is_some() && !dir.join(projects::FILE).is_file() && inner != projects::FILE {
            return Err("the file has a project's content without the project itself".into());
        }
        if !path.exists() {
            write_text(&path, &text)?;
            sum.written += 1;
            continue;
        }
        let existing = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
        match existing.and_then(|e| merge(&inner, e, incoming, overwrite, &mut sum.skipped)) {
            Some(merged) => {
                write_text(&path, &serde_json::to_string_pretty(&merged).map_err(|e| e.to_string())?)?;
                sum.written += 1;
            }
            None if overwrite => {
                write_text(&path, &text)?;
                sum.written += 1;
            }
            None => sum.skipped += 1,
        }
    }
    for s in &bundle.secrets {
        let scope = match &s.project {
            Some(id) if projects::valid_id(id) => Scope::project(id),
            Some(_) => return Err("a secret in the file belongs to an invalid project".into()),
            None => Scope::global(),
        };
        if secrets::restore_value(&scope, &s.name, &s.value, overwrite)? {
            sum.secrets += 1;
        } else {
            sum.skipped += 1;
        }
    }
    for (id, token) in &bundle.claude_tokens {
        if accounts::restore_token(id, token, overwrite)? {
            sum.tokens += 1;
        } else {
            sum.skipped += 1;
        }
    }
    Ok(sum)
}

// ---- commands ----

fn check_password(password: &str) -> Result<(), String> {
    if password.chars().count() < MIN_PASSWORD {
        return Err(format!("the password must have at least {MIN_PASSWORD} characters"));
    }
    Ok(())
}

fn pick_path(app: &tauri::AppHandle, save: bool) -> Result<Option<PathBuf>, String> {
    let dialog = app.dialog().file().add_filter("YAML", &["yaml", "yml"]);
    let chosen = if save { dialog.set_file_name("cc-agent-flows-export.yaml").blocking_save_file() } else { dialog.blocking_pick_file() };
    chosen.map(|p| p.into_path().map_err(|e| e.to_string())).transpose()
}

/// Asks where to save, then writes the encrypted export. The saved path; `None` if the dialog was cancelled.
#[tauri::command]
pub async fn transfer_export(app: tauri::AppHandle, password: String) -> Result<Option<String>, String> {
    check_password(&password)?;
    let dialog_app = app.clone();
    let Some(path) = tauri::async_runtime::spawn_blocking(move || pick_path(&dialog_app, true)).await.map_err(|e| e.to_string())?? else {
        return Ok(None);
    };
    let text = tauri::async_runtime::spawn_blocking(move || {
        let bundle = collect(&app)?;
        let plain = serde_json::to_vec(&bundle).map_err(|e| e.to_string())?;
        encrypt(&password, &plain)
    })
    .await
    .map_err(|e| e.to_string())??;
    write_private(&path, &text)?;
    Ok(Some(path.to_string_lossy().to_string()))
}

/// A file only its owner can read (on Unix): it holds every token.
fn write_private(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).and_then(|mut f| f.write_all(text.as_bytes())).map_err(|e| e.to_string())
}

/// Asks for an export file, decrypts it and merges it in. `overwrite`: items that already exist
/// are replaced (otherwise they stay). `None` if the dialog was cancelled.
#[tauri::command]
pub async fn transfer_import(app: tauri::AppHandle, password: String, overwrite: bool) -> Result<Option<Summary>, String> {
    let dialog_app = app.clone();
    let Some(path) = tauri::async_runtime::spawn_blocking(move || pick_path(&dialog_app, false)).await.map_err(|e| e.to_string())?? else {
        return Ok(None);
    };
    if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > MAX_FILE {
        return Err("the file is too large".into());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let plain = decrypt(&password, &text)?;
        let bundle: Bundle = serde_json::from_slice(&plain).map_err(|_| "the file's content is damaged".to_string())?;
        apply(&app, bundle, overwrite).map(Some)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_right_password_opens_it_and_a_wrong_one_does_not() {
        let text = encrypt("correct horse", b"{\"secret\":1}").unwrap();
        assert!(!text.contains("secret"));
        assert_eq!(decrypt("correct horse", &text).unwrap(), b"{\"secret\":1}");
        assert!(decrypt("wrong password", &text).is_err());
        assert!(decrypt("correct horse", "format: other\n").is_err());
    }

    #[test]
    fn only_known_files_are_accepted() {
        assert!(classify("variables.json").is_some());
        assert!(classify("profiles/dev.json").is_some());
        assert!(classify("images/specs/qa.json").is_some());
        assert!(classify("projects/demo/workflows/main.json").is_some());
        assert!(classify("projects/demo/project.json").is_some());
        assert!(classify("projects/demo/webhook.json").is_none());
        assert!(classify("projects/../x/project.json").is_none());
        assert!(classify("profiles/../../evil.json").is_none());
        assert!(classify("sessions/a.json").is_none());
        assert!(classify("/etc/passwd").is_none());
    }

    #[test]
    fn only_the_portable_vm_settings_travel() {
        let text = r#"{"default_memory_mb":4096,"max_vms":2,"accel":"software","run_grace_secs":600}"#;
        let v: Value = serde_json::from_str(&portable_vm(text).unwrap()).unwrap();
        assert_eq!(v, serde_json::json!({"default_memory_mb": 4096, "run_grace_secs": 600}));
        assert!(portable_vm(r#"{"max_vms":2}"#).is_none());
        assert!(classify("vm.json").is_some());
        assert!(classify("projects/demo/vm.json").is_none());
    }

    #[test]
    fn the_global_system_prompt_travels_and_keeps_ours_unless_asked() {
        assert!(classify("system_prompt.json").is_some());
        assert!(classify("projects/demo/system_prompt.json").is_none());
        let mut skipped = 0;
        let ours = serde_json::json!({"system_prompt": "mine"});
        let theirs = serde_json::json!({"system_prompt": "theirs"});
        assert_eq!(merge("system_prompt.json", ours.clone(), theirs.clone(), false, &mut skipped).unwrap(), ours);
        assert_eq!(skipped, 1);
        assert_eq!(merge("system_prompt.json", ours, theirs.clone(), true, &mut skipped).unwrap(), theirs);
        let empty = serde_json::json!({"system_prompt": " "});
        assert_eq!(merge("system_prompt.json", empty, theirs.clone(), false, &mut skipped).unwrap(), theirs);
    }

    #[test]
    fn merging_keeps_what_exists_unless_told_to_overwrite() {
        let mut skipped = 0;
        let existing: Value = serde_json::json!({"A": "1"});
        let incoming: Value = serde_json::json!({"A": "2", "B": "3"});
        let kept = merge("variables.json", existing.clone(), incoming.clone(), false, &mut skipped).unwrap();
        assert_eq!(kept, serde_json::json!({"A": "1", "B": "3"}));
        assert_eq!(skipped, 1);
        let replaced = merge("variables.json", existing, incoming, true, &mut skipped).unwrap();
        assert_eq!(replaced, serde_json::json!({"A": "2", "B": "3"}));
        let list = merge("injections.json", serde_json::json!([{"name": "X", "v": 1}]), serde_json::json!([{"name": "X", "v": 2}, {"name": "Y"}]), false, &mut skipped).unwrap();
        assert_eq!(list, serde_json::json!([{"name": "X", "v": 1}, {"name": "Y"}]));
    }
}
