//! Claude accounts: several setup tokens (personal subscriptions) that agent runs share. Tokens
//! are in the OS credential store like the single token before them (`default` is that same
//! entry); names and the on/off switch are in `<app data>/claude-accounts.json`. Each run takes
//! the next ready account in turn (round-robin). A run that fails because Claude refused the
//! account (usage limit, rate limit, a rejected token) puts that account on cooldown, and when
//! the run had done no billed work it is started again on the next account (`runs::run_vm`).
//! Cooldowns live in memory only: a restart gives every account another chance.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use keyring::Entry;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::protocol::{Final, Status};

const SERVICE: &str = "cc-agent-flows";
/// The account that used to be the only one; its token entry is the old one.
pub const DEFAULT: &str = "default";
const MAX_ACCOUNTS: usize = 20;
/// How long an account rests after Claude said it hit a limit.
const LIMITED_SECS: u64 = 60 * 60;
/// How long after Claude refused its token (a new token clears it at once).
const REJECTED_SECS: u64 = 12 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Meta {
    id: String,
    name: String,
    #[serde(default = "yes")]
    enabled: bool,
}

fn yes() -> bool {
    true
}

#[derive(Default, Serialize, Deserialize)]
struct Index {
    accounts: Vec<Meta>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cooldown {
    Limited,
    Rejected,
}

/// `id -> (until unix seconds, why)`.
static COOLING: Mutex<Option<HashMap<String, (u64, Cooldown)>>> = Mutex::new(None);
static TURN: AtomicUsize = AtomicUsize::new(0);

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn file(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("claude-accounts.json"))
}

fn read(app: &AppHandle) -> Index {
    let mut index: Index = file(app).ok().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if !index.accounts.iter().any(|a| a.id == DEFAULT) {
        index.accounts.insert(0, Meta { id: DEFAULT.into(), name: "Default".into(), enabled: true });
    }
    index
}

fn write(app: &AppHandle, index: &Index) -> Result<(), String> {
    let p = file(app)?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(p, serde_json::to_vec_pretty(index).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

fn entry(id: &str) -> Result<Entry, String> {
    let user = if id == DEFAULT { "claude-setup-token".to_string() } else { format!("claude-account:{id}") };
    Entry::new(SERVICE, &user).map_err(|e| e.to_string())
}

fn token(id: &str) -> Option<String> {
    entry(id).ok()?.get_password().ok()
}

/// What `choose` looks at.
struct Candidate {
    cooling_until: Option<u64>,
}

/// Which candidate runs next: round-robin over those that are not resting (`turn` counts the
/// picks so far); when all are resting, the one that rests the shortest.
fn choose(c: &[Candidate], now: u64, turn: usize) -> Option<usize> {
    let ready: Vec<usize> = (0..c.len()).filter(|i| c[*i].cooling_until.is_none_or(|u| u <= now)).collect();
    if !ready.is_empty() {
        return Some(ready[turn % ready.len()]);
    }
    (0..c.len()).min_by_key(|i| c[*i].cooling_until)
}

fn cooling_until(id: &str) -> Option<(u64, Cooldown)> {
    COOLING.lock().unwrap().as_ref().and_then(|m| m.get(id).copied()).filter(|(u, _)| *u > now())
}

/// The account the next run uses, not one of `tried`: `(id, name, token)`.
pub fn pick(app: &AppHandle, tried: &[String]) -> Result<(String, String, String), String> {
    let usable: Vec<(Meta, String)> = read(app).accounts.into_iter().filter(|a| a.enabled && !tried.contains(&a.id)).filter_map(|a| token(&a.id).map(|t| (a, t))).collect();
    if usable.is_empty() {
        return Err(if tried.is_empty() { "no Claude account is ready: add a setup token in Settings > LLM".into() } else { "every Claude account was refused (usage limit or rejected token)".to_string() });
    }
    let cands: Vec<Candidate> = usable.iter().map(|(a, _)| Candidate { cooling_until: cooling_until(&a.id).map(|c| c.0) }).collect();
    let i = choose(&cands, now(), TURN.fetch_add(1, Ordering::Relaxed)).ok_or("no Claude account")?;
    let (a, t) = usable.into_iter().nth(i).ok_or("no Claude account")?;
    Ok((a.id, a.name, t))
}

/// Whether some enabled account has a token (the check before a session starts).
pub fn usable(app: &AppHandle) -> bool {
    read(app).accounts.iter().any(|a| a.enabled && token(&a.id).is_some())
}

/// Whether a failed run was Claude refusing the account, and how. Looks at the text the run
/// failed with (and its structured answer).
pub fn classify(fin: &Final) -> Option<Cooldown> {
    if fin.status != Status::Failed {
        return None;
    }
    let text = format!("{} {}", fin.result, fin.output.as_ref().map(|o| o.to_string()).unwrap_or_default()).to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
    if has(&["usage limit", "rate limit", "rate_limit", "too many requests", "quota", "limit reached", "429"]) {
        Some(Cooldown::Limited)
    } else if has(&["authentication_error", "invalid api key", "invalid bearer", "oauth token has expired", "token has expired", "unauthorized", "/login", "401"]) {
        Some(Cooldown::Rejected)
    } else {
        None
    }
}

/// Puts an account to rest after Claude refused it.
pub fn rest(id: &str, why: Cooldown) {
    let secs = if why == Cooldown::Limited { LIMITED_SECS } else { REJECTED_SECS };
    COOLING.lock().unwrap().get_or_insert_with(HashMap::new).insert(id.to_string(), (now() + secs, why));
}

fn clear_rest(id: &str) {
    if let Some(m) = COOLING.lock().unwrap().as_mut() {
        m.remove(id);
    }
}

// ---- commands ----

#[derive(Serialize)]
pub struct Info {
    id: String,
    name: String,
    enabled: bool,
    has_token: bool,
    /// Unix seconds until which it rests, and why (`limited | rejected`).
    rest_until: Option<u64>,
    rest_why: Option<&'static str>,
}

#[tauri::command]
pub fn accounts_list(app: AppHandle) -> Vec<Info> {
    read(&app)
        .accounts
        .into_iter()
        .map(|a| {
            let rest = cooling_until(&a.id);
            Info { has_token: token(&a.id).is_some(), rest_until: rest.map(|r| r.0), rest_why: rest.map(|r| if r.1 == Cooldown::Limited { "limited" } else { "rejected" }), id: a.id, name: a.name, enabled: a.enabled }
        })
        .collect()
}

fn clean_token(t: &str) -> Result<String, String> {
    let t = t.trim();
    if t.is_empty() || t.len() > 4096 || t.chars().any(char::is_whitespace) {
        return Err("that is not a setup token".into());
    }
    Ok(t.to_string())
}

fn clean_name(n: &str) -> Result<String, String> {
    let n = n.trim();
    if n.is_empty() || n.len() > 60 {
        return Err("the name is 1 to 60 characters".into());
    }
    Ok(n.to_string())
}

/// Adds an account with its token; returns its id.
#[tauri::command]
pub fn accounts_add(app: AppHandle, name: String, token: String) -> Result<String, String> {
    let (name, token) = (clean_name(&name)?, clean_token(&token)?);
    let mut index = read(&app);
    if index.accounts.len() >= MAX_ACCOUNTS {
        return Err("too many accounts".into());
    }
    let id = format!("a-{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis()));
    entry(&id)?.set_password(&token).map_err(|e| e.to_string())?;
    index.accounts.push(Meta { id: id.clone(), name, enabled: true });
    write(&app, &index)?;
    Ok(id)
}

/// Replaces an account's token (this also lifts its cooldown).
#[tauri::command]
pub fn accounts_set_token(app: AppHandle, id: String, token: String) -> Result<(), String> {
    let token = clean_token(&token)?;
    if !read(&app).accounts.iter().any(|a| a.id == id) {
        return Err("no such account".into());
    }
    entry(&id)?.set_password(&token).map_err(|e| e.to_string())?;
    clear_rest(&id);
    Ok(())
}

#[tauri::command]
pub fn accounts_update(app: AppHandle, id: String, name: String, enabled: bool) -> Result<(), String> {
    let name = clean_name(&name)?;
    let mut index = read(&app);
    let a = index.accounts.iter_mut().find(|a| a.id == id).ok_or("no such account")?;
    (a.name, a.enabled) = (name, enabled);
    write(&app, &index)
}

/// Lets an account that is resting be used again.
#[tauri::command]
pub fn accounts_wake(id: String) {
    clear_rest(&id);
}

/// Removes an account and its token. The default account stays, without a token.
#[tauri::command]
pub fn accounts_remove(app: AppHandle, id: String) -> Result<(), String> {
    match entry(&id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(e) => return Err(e.to_string()),
    }
    clear_rest(&id);
    let mut index = read(&app);
    index.accounts.retain(|a| a.id != id || a.id == DEFAULT);
    write(&app, &index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cand(until: Option<u64>) -> Candidate {
        Candidate { cooling_until: until }
    }

    fn failed(text: &str) -> Final {
        crate::runs::failed(text)
    }

    #[test]
    fn takes_the_ready_accounts_in_turn() {
        let c = [cand(None), cand(Some(500)), cand(None)];
        let picks: Vec<_> = (0..4).map(|t| choose(&c, 100, t).unwrap()).collect();
        assert_eq!(picks, [0, 2, 0, 2], "the resting one is skipped");
        assert_eq!(choose(&c, 600, 1), Some(1), "its rest is over");
        assert_eq!(choose(&[cand(Some(900)), cand(Some(700))], 100, 0), Some(1), "all resting: the one that wakes first");
        assert_eq!(choose(&[], 0, 0), None);
    }

    #[test]
    fn tells_a_refused_account_from_other_failures() {
        assert_eq!(classify(&failed("Claude AI usage limit reached|1760000000")), Some(Cooldown::Limited));
        assert_eq!(classify(&failed("API Error: 429 rate_limit_error")), Some(Cooldown::Limited));
        assert_eq!(classify(&failed("OAuth token has expired. Please run /login")), Some(Cooldown::Rejected));
        assert_eq!(classify(&failed("tests failed: 3 of 40")), None);
        assert_eq!(classify(&failed("timed out")), None);
        let mut done = failed("usage limit");
        done.status = Status::Done;
        assert_eq!(classify(&done), None, "only a failed run counts");
        let mut hidden = failed("exit 1");
        hidden.output = Some(json!({"error": "Too Many Requests"}));
        assert_eq!(classify(&hidden), Some(Cooldown::Limited));
    }

    #[test]
    fn a_rest_ends_and_can_be_lifted() {
        rest("t-1", Cooldown::Limited);
        assert!(cooling_until("t-1").is_some());
        clear_rest("t-1");
        assert!(cooling_until("t-1").is_none());
    }
}
