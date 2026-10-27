//! Secrets live in the OS credential store, never in `settings.json`. The token is write-only
//! from the frontend: commands report whether one is set but never return it.

use keyring::Entry;

const SERVICE: &str = "cc-agent-flows";
const CLAUDE_TOKEN: &str = "claude-setup-token";

fn entry() -> Result<Entry, String> {
    Entry::new(SERVICE, CLAUDE_TOKEN).map_err(|e| e.to_string())
}

/// The stored Claude setup token, for the backend to hand to an agent VM.
#[allow(dead_code)]
pub fn claude_token() -> Option<String> {
    entry().ok()?.get_password().ok()
}

#[tauri::command]
pub fn claude_token_set(token: String) -> Result<(), String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("empty token".into());
    }
    entry()?.set_password(token).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn claude_token_status() -> Result<bool, String> {
    match entry()?.get_password() {
        Ok(_) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub fn claude_token_clear() -> Result<(), String> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
