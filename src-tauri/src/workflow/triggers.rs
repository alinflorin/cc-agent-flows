//! Trigger nodes of a workflow: `cron` nodes start a session on a schedule, `webhook` nodes start
//! one when their URL is called (`webhook.rs`). The session's request comes from the node, and
//! goes to the agent the node points to. (`chat` nodes need nothing here: the user's message in
//! the app is the trigger.) What each node did last is kept in `<app data>/triggers.json`.
//! The app must be running: a minute that passes while it is closed is not made up. A cron node
//! does not start a new session while the one it started last is still going.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{Local, NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::workflow::cron::Cron;
use crate::workflow::{self, Node, NodeKind};

/// A trigger node with the project and workflow it is in and the agent it points to.
pub struct Found {
    pub project: String,
    pub workflow: String,
    pub node: Node,
    pub target: String,
}

/// The trigger nodes of the given kind in every project.
pub fn find_all(app: &AppHandle, kind: NodeKind) -> Vec<Found> {
    let mut out = vec![];
    for project in crate::config::projects::ids(app) {
        let Ok(root) = workflow::root(app, &project) else { continue };
        for w in workflow::workflows(&root) {
            let wf = workflow::workflow(&root, &w.id);
            for n in wf.nodes.iter().filter(|n| n.kind == kind) {
                if let Some(t) = wf.trigger_target(&n.id) {
                    out.push(Found { project: project.clone(), workflow: w.id.clone(), node: n.clone(), target: t.id.clone() });
                }
            }
        }
    }
    out
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct Last {
    last_run: Option<u64>,
    last_session: Option<String>,
    last_error: Option<String>,
}

fn key(project: &str, workflow: &str, node: &str) -> String {
    format!("{project}/{workflow}/{node}")
}

/// Serializes the read-modify-write of the file.
static FILE: Mutex<()> = Mutex::new(());

fn path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(crate::util::data_dir(app)?.join("triggers.json"))
}

fn read(app: &AppHandle) -> HashMap<String, Last> {
    path(app).ok().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn record(app: &AppHandle, k: &str, f: impl FnOnce(&mut Last)) {
    let _g = FILE.lock().unwrap();
    let mut all = read(app);
    f(all.entry(k.to_string()).or_default());
    if let Ok(p) = path(app)
        && let Ok(text) = serde_json::to_string_pretty(&all)
    {
        let _ = std::fs::write(p, text);
    }
}

fn now() -> u64 {
    crate::util::unix_secs()
}

/// The request of a session a trigger starts: a cron node's request as written; a webhook's body
/// as it came, after the node's own text if it has one.
pub fn request(node: &Node, payload: &str) -> String {
    match (node.prompt.trim().is_empty(), payload.is_empty()) {
        (_, true) => node.prompt.clone(),
        (true, false) => payload.to_string(),
        (false, false) => format!("{}\n\n{payload}", node.prompt.trim_end()),
    }
}

/// Starts the session of a trigger node. Returns its id.
pub fn start_session(app: &AppHandle, f: &Found, payload: &str) -> Result<String, String> {
    let root = workflow::root(app, &f.project)?;
    let task = request(&f.node, payload);
    if task.trim().is_empty() {
        return Err("there is no request to start the session with".into());
    }
    let label = if f.node.label.trim().is_empty() { format!("{:?}", f.node.kind).to_lowercase() } else { f.node.label.trim().to_string() };
    let name = format!("{label} · {}", Local::now().format("%Y-%m-%d %H:%M"));
    let source = if f.node.kind == NodeKind::Cron { "cron" } else { "webhook" };
    let sid = workflow::create_triggered_session(&root, &name, &task, &f.workflow, &f.target, source, &f.node.trigger.autonomy)?;
    let _ = app.emit("sessions-changed", ());
    if let Err(e) = crate::workflow::engine::session_start(app.clone(), f.project.clone(), sid.clone(), None) {
        let _ = workflow::delete_session(&root, &sid);
        return Err(e);
    }
    Ok(sid)
}

/// Fires a node and remembers what came of it.
pub fn fire(app: &AppHandle, f: &Found, payload: &str) -> Result<String, String> {
    let k = key(&f.project, &f.workflow, &f.node.id);
    let root = workflow::root(app, &f.project)?;
    if f.node.kind == NodeKind::Cron
        && let Some(last) = read(app).get(&k).and_then(|l| l.last_session.clone())
        && workflow::read_session(&root, &last).is_ok_and(|s| matches!(s.status.as_str(), "running" | "waiting"))
    {
        let e = "the session it started last is still going".to_string();
        record(app, &k, |l| l.last_error = Some(e.clone()));
        return Err(e);
    }
    let result = start_session(app, f, payload);
    record(app, &k, |l| match &result {
        Ok(sid) => {
            l.last_run = Some(now());
            l.last_session = Some(sid.clone());
            l.last_error = None;
        }
        Err(e) => l.last_error = Some(e.clone()),
    });
    result
}

/// Checks the cron nodes every few seconds, firing those whose minute has come (run at startup).
pub fn start(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut fired: HashMap<String, NaiveDateTime> = HashMap::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            let minute = Local::now().naive_local().with_second(0).and_then(|t| t.with_nanosecond(0)).unwrap();
            for f in find_all(&app, NodeKind::Cron) {
                let k = key(&f.project, &f.workflow, &f.node.id);
                if fired.get(&k) == Some(&minute) || !Cron::parse(&f.node.trigger.cron).is_ok_and(|c| c.matches(&minute)) {
                    continue;
                }
                fired.insert(k, minute);
                let app = app.clone();
                let _ = tauri::async_runtime::spawn_blocking(move || fire(&app, &f, "")).await;
            }
        }
    });
}

#[derive(Serialize)]
pub struct Info {
    project: String,
    workflow: String,
    node: String,
    kind: &'static str,
    /// Cron nodes: the next time it fires, local.
    next: Option<String>,
    /// Webhook nodes: the URL to call.
    url: Option<String>,
    last_run: Option<u64>,
    last_session: Option<String>,
    last_error: Option<String>,
}

/// Every cron and webhook node of a project with what it did last, for the editor.
#[tauri::command]
pub fn triggers_status(app: AppHandle, project: String) -> Result<Vec<Info>, String> {
    let last = read(&app);
    let port = crate::workflow::webhook::port(&app);
    let mut out = vec![];
    for (kind, name) in [(NodeKind::Cron, "cron"), (NodeKind::Webhook, "webhook")] {
        for f in find_all(&app, kind).into_iter().filter(|f| f.project == project) {
            let l = last.get(&key(&f.project, &f.workflow, &f.node.id)).cloned().unwrap_or_default();
            let next = (kind == NodeKind::Cron).then(|| Cron::parse(&f.node.trigger.cron).ok().and_then(|c| c.next(Local::now().naive_local(), 1).first().map(|t| t.format("%Y-%m-%d %H:%M").to_string()))).flatten();
            let url = (kind == NodeKind::Webhook).then(|| format!("http://127.0.0.1:{port}/hook/{}", f.node.trigger.token));
            out.push(Info { project: f.project, workflow: f.workflow, node: f.node.id, kind: name, next, url, last_run: l.last_run, last_session: l.last_session, last_error: l.last_error });
        }
    }
    Ok(out)
}

/// Starts the session of a cron or webhook node now (a webhook with `payload` as the body), to
/// try it. Returns the session id.
#[tauri::command]
pub async fn trigger_fire(app: AppHandle, project: String, workflow: String, node: String, payload: Option<String>) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let f = [NodeKind::Cron, NodeKind::Webhook]
            .into_iter()
            .flat_map(|k| find_all(&app, k))
            .find(|f| f.project == project && f.workflow == workflow && f.node.id == node)
            .ok_or("no such trigger, or it points to no agent (save the workflow first)")?;
        fire(&app, &f, payload.as_deref().unwrap_or(""))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The next times a cron expression fires, local (for the editor), or why it is not valid.
#[tauri::command]
pub fn cron_preview(cron: String) -> Result<Vec<String>, String> {
    let c = Cron::parse(&cron)?;
    Ok(c.next(Local::now().naive_local(), 5).iter().map(|t| t.format("%a %Y-%m-%d %H:%M").to_string()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::{Node, NodeKind};

    fn node(prompt: &str) -> Node {
        Node { id: "w".into(), label: String::new(), x: 0, y: 0, kind: NodeKind::Webhook, profile: String::new(), prompt: prompt.into(), max_calls: 20, timeout_mins: 30, trigger: Default::default() }
    }

    #[test]
    fn a_webhook_body_goes_in_as_it_came() {
        let body = "{\"a\": 1,\n  \"b\":   \"  x \"}\n";
        assert_eq!(request(&node(""), body), body);
        assert_eq!(request(&node("Handle this event."), body), format!("Handle this event.\n\n{body}"));
        assert_eq!(request(&node("Every morning, check the board."), ""), "Every morning, check the board.");
        assert_eq!(request(&node(" "), ""), " ");
    }
}
