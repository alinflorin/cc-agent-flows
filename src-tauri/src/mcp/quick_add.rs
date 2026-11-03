//! "Quick add" (Settings > MCP): writes this server's address and token into the configuration of
//! an MCP client on this computer, so the user does not copy them by hand. Adding again replaces
//! the entry (after a port or token change). Files the user may have edited are kept: a JSON or TOML
//! that does not parse is left alone with an error, other entries stay, and the first change makes
//! a copy next to the file (`<name>.ccaf-backup`).
//!
//! - Claude Code: through its own CLI (`claude mcp add-json`, user scope), not by editing `~/.claude.json`, which it writes itself.
//! - Claude Desktop: `claude_desktop_config.json` only starts local (stdio) servers, so the entry runs the `mcp-remote` bridge with `npx` (needs Node.js); Claude Desktop must be restarted.
//! - Codex: a `[mcp_servers.ccaf]` table in `config.toml` of `$CODEX_HOME` (default `~/.codex`) with the URL and the token as a static header.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Manager};

const NAME: &str = "ccaf";

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    ClaudeCode,
    ClaudeDesktop,
    Codex,
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// The `claude_desktop_config.json` text with our entry in `mcpServers`; other content is kept.
fn desktop_merge(existing: &str, url: &str, token: &str) -> Result<String, String> {
    let mut root: Value = if existing.trim().is_empty() { json!({}) } else { serde_json::from_str(existing).map_err(|e| format!("claude_desktop_config.json is not valid JSON ({e}); fix or remove it first"))? };
    let obj = root.as_object_mut().ok_or("claude_desktop_config.json is not a JSON object")?;
    let servers = obj.entry("mcpServers").or_insert_with(|| json!({}));
    let servers = servers.as_object_mut().ok_or("`mcpServers` in claude_desktop_config.json is not an object")?;
    // the token goes through `env`: a space in an argument is mangled on Windows
    servers.insert(
        NAME.into(),
        json!({
            "command": "npx",
            "args": ["-y", "mcp-remote", url, "--header", "Authorization:${CCAF_AUTH}"],
            "env": { "CCAF_AUTH": bearer(token) }
        }),
    );
    serde_json::to_string_pretty(&root).map(|s| s + "\n").map_err(|e| e.to_string())
}

/// The `config.toml` text with our `[mcp_servers.ccaf]` table; comments and other tables are kept.
fn codex_merge(existing: &str, url: &str, token: &str) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = existing.parse().map_err(|e| format!("config.toml is not valid TOML ({e}); fix or remove it first"))?;
    let servers = doc.entry("mcp_servers").or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    let servers = servers.as_table_like_mut().ok_or("`mcp_servers` in config.toml is not a table")?;
    let mut entry = toml_edit::Table::new();
    entry["url"] = toml_edit::value(url);
    let mut headers = toml_edit::InlineTable::new();
    headers.insert("Authorization", bearer(token).into());
    entry["http_headers"] = toml_edit::value(headers);
    servers.insert(NAME, toml_edit::Item::Table(entry));
    Ok(doc.to_string())
}

/// Writes `text` to `path` (creating the folder), after a one-time copy of what was there.
fn write_with_backup(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    if path.is_file() {
        let backup = PathBuf::from(format!("{}.ccaf-backup", path.display()));
        if !backup.exists() {
            std::fs::copy(path, &backup).map_err(|e| e.to_string())?;
        }
    }
    let tmp = PathBuf::from(format!("{}.ccaf-tmp", path.display()));
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    // the token is in it
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).map_or(0o600, |m| m.permissions().mode() & 0o777);
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode));
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn edit_file(path: &Path, merge: impl Fn(&str) -> Result<String, String>) -> Result<(), String> {
    let existing = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    write_with_backup(path, &merge(&existing)?)
}

fn claude_code(url: &str, token: &str) -> Result<String, String> {
    let body = json!({ "type": "http", "url": url, "headers": { "Authorization": bearer(token) } }).to_string();
    let run = |args: &[&str]| -> std::io::Result<std::process::Output> {
        match Command::new("claude").args(args).output() {
            // on Windows the npm install is a .cmd shim
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && cfg!(windows) => Command::new("claude.cmd").args(args).output(),
            r => r,
        }
    };
    // replace an older entry (port or token changed); it is fine if there is none
    let _ = run(&["mcp", "remove", NAME, "--scope", "user"]);
    match run(&["mcp", "add-json", NAME, &body, "--scope", "user"]) {
        Ok(o) if o.status.success() => Ok("Added to Claude Code (all projects). Start a new Claude Code session; `/mcp` shows it.".into()),
        Ok(o) => Err(format!("claude mcp add-json failed: {}", String::from_utf8_lossy(&o.stderr).trim())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err("the `claude` command was not found; install Claude Code or copy the command below".into()),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub async fn mcp_quick_add(app: AppHandle, target: Target) -> Result<String, String> {
    let cfg = super::current(&app);
    if !cfg.enabled || cfg.token.is_empty() {
        return Err("turn the server on and save first".into());
    }
    let url = format!("http://127.0.0.1:{}/mcp", cfg.port);
    let token = cfg.token;
    let home = app.path().home_dir().map_err(|e| e.to_string())?;
    // Claude Desktop keeps its file in the user's config folder (Application Support, %APPDATA%, ~/.config)
    let config_dir = app.path().config_dir().map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || match target {
        Target::ClaudeCode => claude_code(&url, &token),
        Target::ClaudeDesktop => {
            let path = config_dir.join("Claude").join("claude_desktop_config.json");
            edit_file(&path, |t| desktop_merge(t, &url, &token))?;
            Ok(format!("Written to {}. It needs Node.js (npx). Quit and start Claude Desktop again.", path.display()))
        }
        Target::Codex => {
            let dir = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".codex"));
            let path = dir.join("config.toml");
            edit_file(&path, |t| codex_merge(t, &url, &token))?;
            Ok(format!("Written to {}. Start a new Codex session.", path.display()))
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_keeps_other_servers_and_replaces_ours() {
        let old = r#"{"theme":"x","mcpServers":{"fs":{"command":"npx"},"ccaf":{"command":"old"}}}"#;
        let v: Value = serde_json::from_str(&desktop_merge(old, "http://127.0.0.1:1/mcp", "tok").unwrap()).unwrap();
        assert_eq!(v["theme"], "x");
        assert_eq!(v["mcpServers"]["fs"]["command"], "npx");
        assert_eq!(v["mcpServers"]["ccaf"]["command"], "npx");
        assert_eq!(v["mcpServers"]["ccaf"]["env"]["CCAF_AUTH"], "Bearer tok");
        assert!(desktop_merge("", "u", "t").is_ok());
        assert!(desktop_merge("{broken", "u", "t").is_err());
        assert!(desktop_merge("[]", "u", "t").is_err());
        assert!(desktop_merge(r#"{"mcpServers":3}"#, "u", "t").is_err());
    }

    #[test]
    fn codex_keeps_comments_and_other_tables() {
        let old = "# my config\nmodel = \"o3\"\n\n[mcp_servers.other]\nurl = \"https://x\"\n\n[mcp_servers.ccaf]\nurl = \"old\"\n";
        let out = codex_merge(old, "http://127.0.0.1:7879/mcp", "tok").unwrap();
        assert!(out.contains("# my config") && out.contains("model = \"o3\"") && out.contains("[mcp_servers.other]"));
        let doc: toml_edit::DocumentMut = out.parse().unwrap();
        assert_eq!(doc["mcp_servers"]["ccaf"]["url"].as_str(), Some("http://127.0.0.1:7879/mcp"));
        assert_eq!(doc["mcp_servers"]["ccaf"]["http_headers"]["Authorization"].as_str(), Some("Bearer tok"));
        assert!(!out.contains("\"old\""));
        assert!(codex_merge("", "u", "t").is_ok());
        assert!(codex_merge("= broken", "u", "t").is_err());
    }

    #[test]
    fn backup_is_made_once() {
        let dir = std::env::temp_dir().join(format!("ccaf-qa-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let f = dir.join("sub").join("c.json");
        write_with_backup(&f, "one").unwrap();
        write_with_backup(&f, "two").unwrap();
        write_with_backup(&f, "three").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "three");
        assert_eq!(std::fs::read_to_string(format!("{}.ccaf-backup", f.display())).unwrap(), "one");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
