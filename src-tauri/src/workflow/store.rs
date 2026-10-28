//! Files of the workflows (`workflows/<id>.json`) and the sessions (`sessions/<sid>.json`) under a
//! root directory. No Tauri here, so it can be tested on a temp dir.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::model::*;
use crate::util::{read_json, write_json};

// ---- storage ----

fn safe_id(id: &str) -> Result<&str, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err("bad id".into());
    }
    Ok(id)
}

fn workflows_dir(root: &Path) -> PathBuf {
    root.join("workflows")
}

/// A workflow as stored: `workflows/<id>.json`.
#[derive(Serialize, Deserialize, Default)]
struct Stored {
    #[serde(default)]
    name: String,
    #[serde(default)]
    system_prompt: String,
    #[serde(default)]
    workflow: Workflow,
}

fn workflow_path(root: &Path, id: &str) -> Result<PathBuf, String> {
    Ok(workflows_dir(root).join(format!("{}.json", safe_id(id)?)))
}

fn stored(root: &Path, id: &str) -> Option<Stored> {
    read_json(&workflow_path(root, id).ok()?)
}

/// A stored workflow whole: its name, chart and instructions.
pub fn load_workflow(root: &Path, id: &str) -> Option<(String, Workflow, Instructions)> {
    let s = stored(root, id)?;
    Some((s.name, s.workflow, Instructions { system_prompt: s.system_prompt }))
}

pub fn workflow(root: &Path, id: &str) -> Workflow {
    stored(root, id).map(|s| s.workflow).unwrap_or_default()
}

pub fn instructions(root: &Path, id: &str) -> Instructions {
    Instructions { system_prompt: stored(root, id).map(|s| s.system_prompt).unwrap_or_default() }
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowEntry {
    pub id: String,
    pub name: String,
    pub agents: usize,
}

pub fn workflows(root: &Path) -> Vec<WorkflowEntry> {
    let Ok(rd) = std::fs::read_dir(workflows_dir(root)) else { return vec![] };
    let mut out: Vec<WorkflowEntry> = rd
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension()? != "json" {
                return None;
            }
            let id = p.file_stem()?.to_str()?.to_string();
            let s: Stored = read_json(&p)?;
            let agents = s.workflow.nodes.iter().filter(|n| n.kind == NodeKind::Agent).count();
            Some(WorkflowEntry { name: if s.name.is_empty() { id.clone() } else { s.name }, id, agents })
        })
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
    out
}

/// Saves a workflow; an empty `id` makes a new one. Returns its id.
pub fn save_workflow(root: &Path, id: &str, name: &str, wf: &Workflow, instructions: &Instructions) -> Result<String, String> {
    let mut wf = wf.clone();
    wf.validate()?;
    // a webhook gets its secret URL the first time it is saved
    for n in wf.nodes.iter_mut().filter(|n| n.kind == NodeKind::Webhook && n.trigger.token.is_empty()) {
        n.trigger.token = new_token();
    }
    let wf = &wf;
    if instructions.system_prompt.len() > 64 * 1024 {
        return Err("the instructions are too large".into());
    }
    if name.trim().is_empty() || name.len() > 120 {
        return Err("the name is required, up to 120 characters".into());
    }
    let id = if id.is_empty() {
        let base = format!("w-{:x}", crate::util::unix_millis());
        let (mut id, mut n) = (base.clone(), 2);
        while workflow_path(root, &id)?.exists() {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    } else {
        id.to_string()
    };
    write_json(&workflow_path(root, &id)?, &Stored { name: name.trim().into(), system_prompt: instructions.system_prompt.clone(), workflow: wf.clone() })?;
    Ok(id)
}

/// A new unguessable webhook token (128 random bits, hex).
pub fn new_token() -> String {
    let b: [u8; 16] = rand::random();
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Deletes a workflow no session uses.
pub fn delete_workflow(root: &Path, id: &str) -> Result<(), String> {
    let path = workflow_path(root, id)?;
    if sessions(root).iter().any(|s| s.session.workflow == id) {
        return Err("a session uses this workflow".into());
    }
    std::fs::remove_file(path).map_err(|e| e.to_string())
}

fn sessions_dir(root: &Path) -> PathBuf {
    root.join("sessions")
}

fn session_path(root: &Path, sid: &str) -> Result<PathBuf, String> {
    Ok(sessions_dir(root).join(format!("{}.json", safe_id(sid)?)))
}

/// A file next to a session's own: `<sid>.<ext>` in the `sessions/` folder (`state.json` and
/// `log.jsonl` belong to the engine).
pub fn session_file(root: &Path, sid: &str, ext: &str) -> Result<PathBuf, String> {
    Ok(sessions_dir(root).join(format!("{}.{ext}", safe_id(sid)?)))
}

pub fn read_session(root: &Path, sid: &str) -> Result<Session, String> {
    read_json(&session_path(root, sid)?).ok_or_else(|| "no such session".to_string())
}

pub fn set_session_status(root: &Path, sid: &str, status: &str) -> Result<(), String> {
    let mut session = read_session(root, sid)?;
    session.status = status.into();
    write_json(&session_path(root, sid)?, &session)
}

pub fn sessions(root: &Path) -> Vec<SessionEntry> {
    let Ok(rd) = std::fs::read_dir(sessions_dir(root)) else { return vec![] };
    let mut out: Vec<SessionEntry> = rd
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let id = p.file_stem()?.to_str()?.to_string();
            // `<sid>.state.json` and the like are the engine's files, not sessions
            if id.contains('.') || p.extension()? != "json" {
                return None;
            }
            Some(SessionEntry { id, session: read_json(&p)? })
        })
        .collect();
    out.sort_by(|a, b| b.session.created.cmp(&a.session.created).then(b.id.cmp(&a.id)));
    out
}

pub fn create_session(root: &Path, name: &str, task: &str, workflow: &str) -> Result<String, String> {
    if name.len() > 120 || task.len() > 64 * 1024 {
        return Err("the name is up to 120 characters and the task up to 64 KB".into());
    }
    safe_id(workflow)?;
    let base = format!("s-{:x}", crate::util::unix_millis());
    let (mut id, mut n) = (base.clone(), 2);
    while session_path(root, &id)?.exists() {
        id = format!("{base}-{n}");
        n += 1;
    }
    let session = Session { name: name.trim().into(), task: task.into(), created: crate::util::unix_secs(), status: new_status(), autonomy: ask(), workflow: workflow.into(), messages: vec![], entry: None, source: chat() };
    write_json(&session_path(root, &id)?, &session)?;
    Ok(id)
}

/// A session started by a trigger node: its request is already set, and the agent that gets it.
pub fn create_triggered_session(root: &Path, name: &str, task: &str, workflow: &str, entry: &str, source: &str, autonomy: &str) -> Result<String, String> {
    let sid = create_session(root, name, task, workflow)?;
    let mut session = read_session(root, &sid)?;
    session.entry = Some(entry.into());
    session.source = source.into();
    session.autonomy = if autonomy == "ask" { "ask".into() } else { "never".into() };
    write_json(&session_path(root, &sid)?, &session)?;
    Ok(sid)
}

/// Sets a session's name (empty = untitled) and/or its task (only before it starts).
pub fn edit_session(root: &Path, sid: &str, name: Option<&str>, task: Option<&str>) -> Result<(), String> {
    let mut session = read_session(root, sid)?;
    if let Some(name) = name {
        if name.len() > 120 {
            return Err("the name is up to 120 characters".into());
        }
        session.name = name.trim().into();
    }
    if let Some(task) = task {
        if session.status != "new" {
            return Err("the task cannot change once the session has started".into());
        }
        if task.len() > 64 * 1024 {
            return Err("the task is too large".into());
        }
        session.task = task.into();
    }
    write_json(&session_path(root, sid)?, &session)
}

pub fn set_session_autonomy(root: &Path, sid: &str, autonomy: &str) -> Result<(), String> {
    if !matches!(autonomy, "ask" | "never") {
        return Err("autonomy is `ask` or `never`".into());
    }
    let mut session = read_session(root, sid)?;
    session.autonomy = autonomy.into();
    write_json(&session_path(root, sid)?, &session)
}

/// Most messages a session takes after the first.
const MAX_MESSAGES: usize = 200;

/// Adds a message to a session that has started (it is run after the turns before it).
pub fn add_session_message(root: &Path, sid: &str, text: &str) -> Result<usize, String> {
    let text = text.trim();
    if text.is_empty() || text.len() > 64 * 1024 {
        return Err("a message is 1 byte to 64 KB".into());
    }
    let mut session = read_session(root, sid)?;
    if session.status == "new" {
        return Err("the session has not started: send the first message to start it".into());
    }
    if session.messages.len() >= MAX_MESSAGES {
        return Err("the session has reached its limit of messages".into());
    }
    session.messages.push(text.into());
    write_json(&session_path(root, sid)?, &session)?;
    Ok(session.messages.len())
}

/// Takes back the `n`-th message (1-based) when it could not be started.
pub fn drop_session_message(root: &Path, sid: &str, n: usize) -> Result<(), String> {
    let mut session = read_session(root, sid)?;
    if session.messages.len() == n {
        session.messages.pop();
        write_json(&session_path(root, sid)?, &session)?;
    }
    Ok(())
}

/// Picks the workflow of a session that has not started.
pub fn set_session_workflow(root: &Path, sid: &str, workflow: &str) -> Result<(), String> {
    let mut session = read_session(root, sid)?;
    if session.status != "new" {
        return Err("the workflow cannot change once the session has started".into());
    }
    if stored(root, workflow).is_none() {
        return Err("no such workflow".into());
    }
    session.workflow = workflow.into();
    write_json(&session_path(root, sid)?, &session)
}

pub fn delete_session(root: &Path, sid: &str) -> Result<(), String> {
    for ext in ["state.json", "log.jsonl"] {
        let _ = std::fs::remove_file(session_file(root, sid, ext)?);
    }
    std::fs::remove_file(session_path(root, sid)?).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::model::tests::n;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ccaf-proj-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_webhook_gets_a_secret_url_when_saved_and_keeps_it() {
        let root = tmp("hook");
        let mut wf = standard_workflow();
        wf.nodes.push(Node { kind: NodeKind::Webhook, profile: String::new(), ..n("hook") });
        wf.edges.push(Edge { id: "h".into(), from: "hook".into(), to: "orch".into(), kind: EdgeKind::Delegate });
        let id = save_workflow(&root, "", "W", &wf, &Instructions::default()).unwrap();
        let token = workflow(&root, &id).nodes.iter().find(|n| n.id == "hook").unwrap().trigger.token.clone();
        assert_eq!(token.len(), 32);
        let saved = workflow(&root, &id);
        save_workflow(&root, &id, "W", &saved, &Instructions::default()).unwrap();
        assert_eq!(workflow(&root, &id).nodes.iter().find(|n| n.id == "hook").unwrap().trigger.token, token);
        assert_ne!(new_token(), new_token());
    }

    #[test]
    fn workflow_and_sessions_roundtrip() {
        let root = tmp("rt");
        assert!(workflow(&root, "main").nodes.is_empty());
        let ins = Instructions { system_prompt: "be brief".into() };
        let id = save_workflow(&root, "", "Main", &standard_workflow(), &ins).unwrap();
        assert_eq!(workflow(&root, &id), standard_workflow());
        assert_eq!(instructions(&root, &id), ins);
        assert!(save_workflow(&root, &id, "Main", &Workflow { nodes: vec![n("a"), n("a")], edges: vec![] }, &ins).is_err());
        assert_eq!(workflow(&root, &id), standard_workflow(), "a refused save changes nothing");
        let other = save_workflow(&root, "", "Other", &standard_workflow(), &ins).unwrap();
        assert_ne!(id, other);
        assert_eq!(workflows(&root).iter().map(|w| w.name.as_str()).collect::<Vec<_>>(), ["Main", "Other"]);
        assert!(save_workflow(&root, &id, " ", &standard_workflow(), &ins).is_err());

        let s1 = create_session(&root, "first", "do it", &id).unwrap();
        let s2 = create_session(&root, "", "", &id).unwrap();
        assert_ne!(s1, s2);
        assert_eq!(sessions(&root).len(), 2);
        assert!(create_session(&root, &"x".repeat(121), "", &id).is_err());
        set_session_autonomy(&root, &s2, "never").unwrap();
        assert_eq!(read_session(&root, &s2).unwrap().autonomy, "never");
        assert!(set_session_autonomy(&root, &s2, "sometimes").is_err());
        edit_session(&root, &s2, Some("  renamed "), Some("a task")).unwrap();
        set_session_status(&root, &s2, "running").unwrap();
        assert!(edit_session(&root, &s2, None, Some("another")).is_err(), "the task is fixed once started");
        assert_eq!(read_session(&root, &s2).unwrap().name, "renamed");
        assert!(delete_workflow(&root, &id).is_err(), "in use");
        set_session_workflow(&root, &s1, &other).unwrap();
        assert!(set_session_workflow(&root, &s1, "nope").is_err());
        assert!(set_session_workflow(&root, &s2, &other).is_err(), "started");
        delete_workflow(&root, &id).unwrap_err();
        delete_session(&root, &s1).unwrap();
        delete_workflow(&root, &other).unwrap();
        assert_eq!(sessions(&root).len(), 1);
        assert!(read_session(&root, "../x").is_err());
    }
}
