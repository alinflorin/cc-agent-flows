//! Numbers over all the sessions, from their logs: the tokens they used, how long they took, how often
//! each kind of agent ran, failed and how long it needed. Computed when asked.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::Serialize;
use serde_json::Value;
use tauri::AppHandle;

use crate::workflow;

/// Every entry of a session's log of one of `kinds` (all when empty), in order. Lines are
/// filtered by text before they are parsed: agent events are most of the file.
fn log_entries(root: &Path, sid: &str, kinds: &[&str]) -> Vec<Value> {
    let Ok(path) = workflow::session_file(root, sid, "log.jsonl") else { return vec![] };
    let Ok(text) = std::fs::read_to_string(path) else { return vec![] };
    let wanted: Vec<String> = kinds.iter().map(|k| format!("\"kind\":\"{k}\"")).collect();
    text.lines()
        .filter(|l| wanted.is_empty() || wanted.iter().any(|w| l.contains(w.as_str())))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| kinds.is_empty() || v.get("kind").and_then(Value::as_str).is_some_and(|k| kinds.contains(&k)))
        .collect()
}

#[derive(Serialize, Debug, PartialEq)]
pub struct SessionStat {
    id: String,
    name: String,
    task: String,
    status: String,
    created: u64,
    duration_secs: u64,
    tokens: u64,
    runs: u32,
    failed_runs: u32,
    turns: u32,
}

/// One agent profile over all sessions.
#[derive(Serialize, Default, Debug, PartialEq)]
pub struct AgentStat {
    profile: String,
    runs: u32,
    failed: u32,
    tokens: u64,
    total_secs: u64,
    avg_secs: u64,
    longest_secs: u64,
}

#[derive(Serialize, Default, Debug, PartialEq)]
pub struct DayStat {
    /// Local date, `YYYY-MM-DD`.
    day: String,
    sessions: u32,
    runs: u32,
    tokens: u64,
}

#[derive(Serialize, Default, Debug, PartialEq)]
pub struct Analytics {
    sessions: Vec<SessionStat>,
    agents: Vec<AgentStat>,
    days: Vec<DayStat>,
    total_tokens: u64,
    total_runs: u32,
    failed_runs: u32,
}

fn day(t: u64) -> String {
    chrono::DateTime::from_timestamp(t as i64, 0).map_or_else(String::new, |d| d.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
}

/// Folds one session's log entries into its stat and the per-agent and per-day tables.
fn fold(sid: &str, session: &workflow::Session, entries: &[Value], agents: &mut HashMap<String, AgentStat>, days: &mut BTreeMap<String, DayStat>) -> SessionStat {
    let mut stat = SessionStat {
        id: sid.into(),
        name: session.name.clone(),
        task: session.task.chars().take(200).collect(),
        status: session.status.clone(),
        created: session.created,
        duration_secs: 0,
        tokens: 0,
        runs: 0,
        failed_runs: 0,
        turns: 1 + session.messages.len() as u32,
    };
    let mut started: HashMap<&str, (u64, &str)> = HashMap::new();
    let (mut first, mut last) = (u64::MAX, 0);
    for e in entries {
        let t = e.get("t").and_then(Value::as_u64).unwrap_or(0);
        first = first.min(t);
        last = last.max(t);
        let body = e.get("body").unwrap_or(&Value::Null);
        let text = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or("");
        match e.get("kind").and_then(Value::as_str) {
            Some("run_start") => {
                started.insert(text("call"), (t, text("profile")));
            }
            Some("run_end") => {
                let (t0, profile) = started.remove(text("call")).unwrap_or((t, ""));
                let secs = t.saturating_sub(t0);
                let tokens = body.get("tokens").and_then(Value::as_u64).unwrap_or(0);
                let failed = text("status") == "failed";
                stat.runs += 1;
                stat.tokens += tokens;
                stat.failed_runs += u32::from(failed);
                let a = agents.entry(profile.to_string()).or_insert_with(|| AgentStat { profile: profile.to_string(), ..Default::default() });
                a.runs += 1;
                a.failed += u32::from(failed);
                a.tokens += tokens;
                a.total_secs += secs;
                a.longest_secs = a.longest_secs.max(secs);
                let d = days.entry(day(t)).or_insert_with(|| DayStat { day: day(t), ..Default::default() });
                d.runs += 1;
                d.tokens += tokens;
            }
            _ => {}
        }
    }
    if first != u64::MAX {
        stat.duration_secs = last - first;
    }
    days.entry(day(session.created)).or_insert_with(|| DayStat { day: day(session.created), ..Default::default() }).sessions += 1;
    stat
}

pub fn compute(root: &std::path::Path) -> Analytics {
    let mut out = Analytics::default();
    let (mut agents, mut days) = (HashMap::new(), BTreeMap::new());
    for s in workflow::sessions(root).into_iter().filter(|s| s.session.status != "new") {
        let entries = log_entries(root, &s.id, &["started", "status", "run_start", "run_end"]);
        let stat = fold(&s.id, &s.session, &entries, &mut agents, &mut days);
        out.total_tokens += stat.tokens;
        out.total_runs += stat.runs;
        out.failed_runs += stat.failed_runs;
        out.sessions.push(stat);
    }
    let mut agents: Vec<AgentStat> = agents.into_values().collect();
    for a in &mut agents {
        a.avg_secs = a.total_secs.checked_div(u64::from(a.runs)).unwrap_or(0);
    }
    agents.sort_by(|a, b| b.tokens.cmp(&a.tokens).then(a.profile.cmp(&b.profile)));
    out.agents = agents;
    out.days = days.into_values().collect();
    out
}

#[tauri::command]
pub async fn analytics_get(app: AppHandle) -> Result<Analytics, String> {
    let root = workflow::root(&app)?;
    tauri::async_runtime::spawn_blocking(move || compute(&root)).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn folds_runs_into_session_agent_and_day_numbers() {
        let session = workflow::Session { name: "n".into(), task: "t".into(), created: 1_000_000, status: "done".into(), autonomy: "ask".into(), workflow: "w".into(), messages: vec!["x".into()], entry: None, source: "chat".into() };
        let e = |t: u64, kind: &str, body: Value| json!({"t": t, "kind": kind, "body": body});
        let entries = vec![
            e(100, "started", json!({})),
            e(110, "run_start", json!({"call": "r", "profile": "dev"})),
            e(150, "run_end", json!({"call": "r", "status": "done", "tokens": 500})),
            e(150, "run_start", json!({"call": "r/1.1", "profile": "dev"})),
            e(160, "run_end", json!({"call": "r/1.1", "status": "failed", "tokens": 250})),
            e(170, "status", json!({"status": "done"})),
        ];
        let (mut agents, mut days) = (HashMap::new(), BTreeMap::new());
        let s = fold("s-1", &session, &entries, &mut agents, &mut days);
        assert_eq!((s.runs, s.failed_runs, s.tokens, s.duration_secs, s.turns), (2, 1, 750, 70, 2));
        let a = &agents["dev"];
        assert_eq!((a.runs, a.failed, a.total_secs, a.longest_secs), (2, 1, 50, 40));
        assert_eq!(days.values().map(|d| d.runs).sum::<u32>(), 2);
    }
}
