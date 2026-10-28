//! Things that leave the app as files: the shareable bundle of workflows, agent profiles and
//! images (and its import), and a session's audit record. Files are written to the user's
//! Downloads folder. Bundles never hold secrets, variables' values or injections: only names the
//! items refer to (`${secret:NAME}`), so whoever imports one sets those up on their own machine.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Manager};

use crate::images::{self, Spec};
use crate::profiles::{self, Profile};
use crate::workflow::{self, Instructions, Workflow};

const BUNDLE_FORMAT: &str = "ccaf-bundle";
const BUNDLE_VERSION: u32 = 1;
/// Largest bundle the import takes.
const MAX_BUNDLE: usize = 16 << 20;

#[derive(Serialize, Deserialize)]
struct BWorkflow {
    id: String,
    name: String,
    #[serde(default)]
    system_prompt: String,
    workflow: Workflow,
}

#[derive(Serialize, Deserialize)]
struct BProfile {
    id: String,
    profile: Profile,
}

#[derive(Serialize, Deserialize)]
struct BImage {
    id: String,
    spec: Spec,
}

#[derive(Serialize, Deserialize, Default)]
struct Bundle {
    format: String,
    version: u32,
    #[serde(default)]
    workflows: Vec<BWorkflow>,
    #[serde(default)]
    profiles: Vec<BProfile>,
    #[serde(default)]
    images: Vec<BImage>,
}

/// Writes `text` to a new file in the Downloads folder (the data dir when there is none) and
/// returns its path. Never overwrites: a taken name gets a number.
fn save_download(app: &AppHandle, name: &str, text: &str) -> Result<String, String> {
    let dir = app.path().download_dir().or_else(|_| app.path().app_data_dir()).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, "txt"));
    let mut path = dir.join(name);
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem}-{n}.{ext}"));
        n += 1;
    }
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

fn stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

fn profiles_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("profiles"))
}

// ---- bundles ----

/// What to export. With `with_dependencies`, a workflow brings the profiles its agents use, a
/// profile its image, and an image the chain of images under it (the base is built in).
#[derive(Deserialize)]
pub struct Selection {
    #[serde(default)]
    workflows: Vec<String>,
    #[serde(default)]
    profiles: Vec<String>,
    #[serde(default)]
    images: Vec<String>,
    #[serde(default)]
    with_dependencies: bool,
}

fn build_bundle(app: &AppHandle, sel: &Selection) -> Result<Bundle, String> {
    let root = workflow::root(app)?;
    let all_profiles = profiles::list(&profiles_dir(app)?);
    let all_images = images::read_specs(app);
    let (mut wf_ids, mut pr_ids, mut im_ids) = (sel.workflows.clone(), sel.profiles.clone(), sel.images.clone());

    let mut bundle = Bundle { format: BUNDLE_FORMAT.into(), version: BUNDLE_VERSION, ..Default::default() };
    let names = workflow::workflows(&root);
    for id in std::mem::take(&mut wf_ids) {
        let entry = names.iter().find(|w| w.id == id).ok_or(format!("no such workflow: {id}"))?;
        let mut wf = workflow::workflow(&root, &id);
        // a webhook's URL is a secret: whoever imports the bundle gets a new one
        wf.nodes.iter_mut().for_each(|n| n.trigger.token.clear());
        if sel.with_dependencies {
            for n in wf.nodes.iter().filter(|n| n.kind == workflow::NodeKind::Agent) {
                pr_ids.push(n.profile.clone());
            }
        }
        bundle.workflows.push(BWorkflow { id, name: entry.name.clone(), system_prompt: workflow::instructions(&root, &entry.id).system_prompt, workflow: wf });
    }
    let mut seen = HashSet::new();
    for id in pr_ids {
        if !seen.insert(id.clone()) {
            continue;
        }
        let entry = all_profiles.iter().find(|e| e.id == id);
        let Some(entry) = entry else {
            if sel.with_dependencies {
                continue; // a workflow may name a profile that is gone; the importer sees it as a problem
            }
            return Err(format!("no such profile: {id}"));
        };
        if sel.with_dependencies {
            im_ids.push(entry.profile.image.clone());
        }
        bundle.profiles.push(BProfile { id, profile: entry.profile.clone() });
    }
    let mut seen = HashSet::new();
    let mut queue = im_ids;
    while let Some(id) = queue.pop() {
        if id == images::BASE || !seen.insert(id.clone()) {
            continue;
        }
        let (_, spec) = all_images.iter().find(|(i, _)| *i == id).ok_or(format!("no such image: {id}"))?;
        if sel.with_dependencies {
            queue.push(spec.parent.clone());
        }
        bundle.images.push(BImage { id, spec: spec.clone() });
    }
    if bundle.workflows.is_empty() && bundle.profiles.is_empty() && bundle.images.is_empty() {
        return Err("nothing selected".into());
    }
    Ok(bundle)
}

/// Writes the selection to a bundle file in Downloads; returns its path.
#[tauri::command]
pub fn bundle_export(app: AppHandle, selection: Selection) -> Result<String, String> {
    let bundle = build_bundle(&app, &selection)?;
    let text = serde_json::to_string_pretty(&bundle).map_err(|e| e.to_string())?;
    save_download(&app, &format!("ccaf-bundle-{}.json", stamp()), &text)
}

#[derive(Serialize, Default, Debug, PartialEq)]
pub struct ImportReport {
    pub added: Vec<String>,
    pub replaced: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
}

fn parse_bundle(text: &str) -> Result<Bundle, String> {
    if text.len() > MAX_BUNDLE {
        return Err("the file is too large to be a bundle".into());
    }
    let b: Bundle = serde_json::from_str(text).map_err(|e| format!("not a bundle: {e}"))?;
    if b.format != BUNDLE_FORMAT {
        return Err("not a CC Agent Flows bundle".into());
    }
    if b.version > BUNDLE_VERSION {
        return Err("the bundle comes from a newer version of the app".into());
    }
    Ok(b)
}

/// Adds the bundle's items. An id that exists is left alone, or replaced with `overwrite`.
/// Imported images are not built (their scripts run as root in a VM with open internet: that
/// starts only when the user asks).
#[tauri::command]
pub fn bundle_import(app: AppHandle, text: String, overwrite: bool) -> Result<ImportReport, String> {
    let bundle = parse_bundle(&text)?;
    let root = workflow::root(&app)?;
    let pdir = profiles_dir(&app)?;
    let mut report = ImportReport::default();

    // Images first, parents before the images built on them.
    let mut todo: Vec<BImage> = bundle.images;
    loop {
        let before = todo.len();
        let mut later = vec![];
        for img in todo {
            let existing = images::read_specs(&app);
            let known = |id: &str| id == images::BASE || existing.iter().any(|(i, _)| i == id);
            let label = format!("image {}", img.id);
            if !images::valid_id(&img.id) {
                report.errors.push(format!("{label}: bad id"));
            } else if !known(&img.spec.parent) {
                later.push(img);
            } else if existing.iter().any(|(i, _)| *i == img.id) && !overwrite {
                report.skipped.push(label);
            } else {
                let replaced = existing.iter().any(|(i, _)| *i == img.id);
                match images::save_spec(&app, Some(img.id), img.spec, false) {
                    Ok(_) => (if replaced { &mut report.replaced } else { &mut report.added }).push(label),
                    Err(e) => report.errors.push(format!("{label}: {e}")),
                }
            }
        }
        todo = later;
        if todo.is_empty() || todo.len() == before {
            break;
        }
    }
    for img in todo {
        report.errors.push(format!("image {}: the image it is built on is not here", img.id));
    }

    for p in bundle.profiles {
        let label = format!("profile {}", p.id);
        let exists = profiles::list(&pdir).iter().any(|e| e.id == p.id);
        if exists && !overwrite {
            report.skipped.push(label);
            continue;
        }
        match profiles::save(&pdir, Some(&p.id), &p.profile) {
            Ok(_) => (if exists { &mut report.replaced } else { &mut report.added }).push(label),
            Err(e) => report.errors.push(format!("{label}: {e}")),
        }
    }

    for w in bundle.workflows {
        let label = format!("workflow {}", w.id);
        let exists = workflow::workflows(&root).iter().any(|e| e.id == w.id);
        if exists && !overwrite {
            report.skipped.push(label);
            continue;
        }
        match workflow::save_workflow(&root, &w.id, &w.name, &w.workflow, &Instructions { system_prompt: w.system_prompt }) {
            Ok(_) => (if exists { &mut report.replaced } else { &mut report.added }).push(label),
            Err(e) => report.errors.push(format!("{label}: {e}")),
        }
    }
    Ok(report)
}

// ---- audit ----

/// Every entry of a session's log of one of `kinds` (all when empty), in order. Lines are
/// filtered by text before they are parsed: agent events are most of the file.
pub fn log_entries(root: &Path, sid: &str, kinds: &[&str]) -> Vec<Value> {
    let Ok(path) = workflow::session_file(root, sid, "log.jsonl") else { return vec![] };
    let Ok(text) = std::fs::read_to_string(path) else { return vec![] };
    let wanted: Vec<String> = kinds.iter().map(|k| format!("\"kind\":\"{k}\"")).collect();
    text.lines()
        .filter(|l| wanted.is_empty() || wanted.iter().any(|w| l.contains(w.as_str())))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| kinds.is_empty() || v.get("kind").and_then(Value::as_str).is_some_and(|k| kinds.contains(&k)))
        .collect()
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn time(t: u64) -> String {
    chrono::DateTime::from_timestamp(t as i64, 0).map_or_else(String::new, |d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
}

/// The record of a session, from its log: who ran (profile, image), what they were asked and what
/// they answered, the decisions, what the user was asked and answered, what each run reached on
/// the network (never a credential value) and what it cost.
pub fn audit(app: &AppHandle, sid: &str) -> Result<Value, String> {
    let root = workflow::root(app)?;
    let session = workflow::read_session(&root, sid)?;
    let wf = workflow::workflow(&root, &session.workflow);
    let profile_images: std::collections::HashMap<String, String> = profiles::list(&profiles_dir(app)?).into_iter().map(|e| (e.id, e.profile.image)).collect();
    let state = crate::engine::read_state(&root, sid).unwrap_or_default();

    let kinds = ["started", "status", "turn", "call_start", "call_end", "run_start", "run_end", "decision", "decision_rejected", "wait", "reply", "error", "agent"];
    let mut runs: Vec<Value> = vec![];
    let mut timeline: Vec<Value> = vec![];
    let mut network: Vec<Value> = vec![];
    let mut accounts: Vec<Value> = vec![];
    for e in log_entries(&root, sid, &kinds) {
        let (t, kind) = (e.get("t").and_then(Value::as_u64).unwrap_or(0), s(&e, "kind").to_string());
        let body = e.get("body").cloned().unwrap_or(Value::Null);
        match kind.as_str() {
            "agent" => {
                let ev = body.get("event").cloned().unwrap_or(Value::Null);
                if s(&ev, "type") == "egress" {
                    network.push(json!({"time": time(t), "agent": s(&body, "node"), "call": s(&body, "call"), "line": s(&ev, "text")}));
                } else if s(&ev, "type") == "account" {
                    // which Claude account a run used (the name, never the token) and when one was refused
                    accounts.push(json!({"time": time(t), "agent": s(&body, "node"), "call": s(&body, "call"), "account": s(&ev, "account"), "refused": ev.get("refused").and_then(Value::as_bool).unwrap_or(false)}));
                }
            }
            "run_start" => {
                let profile = s(&body, "profile").to_string();
                runs.push(json!({"call": s(&body, "call"), "agent": s(&body, "node"), "profile": profile, "image": profile_images.get(&profile), "started": time(t), "t0": t}));
            }
            "run_end" => {
                if let Some(r) = runs.iter_mut().rev().find(|r| s(r, "call") == s(&body, "call")) {
                    let t0 = r.get("t0").and_then(Value::as_u64).unwrap_or(t);
                    r["ended"] = json!(time(t));
                    r["seconds"] = json!(t.saturating_sub(t0));
                    r["status"] = body.get("status").cloned().unwrap_or(Value::Null);
                    r["cost_usd"] = body.get("cost_usd").cloned().unwrap_or(Value::Null);
                    r["summary"] = body.get("summary").cloned().unwrap_or(Value::Null);
                    r["assumptions"] = body.get("assumptions").cloned().unwrap_or(Value::Null);
                }
                timeline.push(json!({"time": time(t), "kind": kind, "detail": body}));
            }
            _ => timeline.push(json!({"time": time(t), "kind": kind, "detail": body})),
        }
    }
    for r in &mut runs {
        r.as_object_mut().map(|o| o.remove("t0"));
    }
    let agents: Vec<Value> = wf.nodes.iter().filter(|n| n.kind == workflow::NodeKind::Agent).map(|n| json!({"id": n.id, "label": n.label, "profile": n.profile, "image": profile_images.get(&n.profile), "max_calls": n.max_calls, "timeout_mins": n.timeout_mins})).collect();
    Ok(json!({
        "format": "ccaf-audit",
        "version": 1,
        "exported": time(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())),
        "session": {"id": sid, "name": session.name, "status": session.status, "created": time(session.created), "autonomy": session.autonomy, "workflow": session.workflow, "task": session.task, "messages": session.messages},
        "cost_usd": state.cost_usd,
        "agent_runs": state.runs,
        "result": state.result,
        "agents": agents,
        "runs": runs,
        "network": network,
        "claude_accounts": accounts,
        "timeline": timeline,
    }))
}

fn markdown(a: &Value) -> String {
    let mut m = String::new();
    let ses = &a["session"];
    m.push_str(&format!("# Audit record: {}\n\n", if s(ses, "name").is_empty() { s(ses, "id") } else { s(ses, "name") }));
    m.push_str(&format!("- Session: `{}` ({}), created {}, autonomy {}\n- Workflow: `{}`\n- Cost: ${:.4} over {} agent runs\n- Exported: {}\n\n", s(ses, "id"), s(ses, "status"), s(ses, "created"), s(ses, "autonomy"), s(ses, "workflow"), a["cost_usd"].as_f64().unwrap_or(0.0), a["agent_runs"], s(a, "exported")));
    m.push_str(&format!("## Request\n\n{}\n\n", s(ses, "task")));
    for (i, msg) in ses["messages"].as_array().into_iter().flatten().enumerate() {
        m.push_str(&format!("### Follow-up {}\n\n{}\n\n", i + 1, msg.as_str().unwrap_or("")));
    }
    if let Some(r) = a["result"].as_str() {
        m.push_str(&format!("## Result\n\n{r}\n\n"));
    }
    m.push_str("## Agent runs\n\n| Started | Agent | Profile | Image | Status | Seconds | Cost |\n|---|---|---|---|---|---|---|\n");
    for r in a["runs"].as_array().into_iter().flatten() {
        m.push_str(&format!("| {} | {} | {} | {} | {} | {} | {} |\n", s(r, "started"), s(r, "agent"), s(r, "profile"), r["image"].as_str().unwrap_or(""), s(r, "status"), r["seconds"], r["cost_usd"].as_f64().map_or(String::new(), |c| format!("${c:.4}"))));
    }
    m.push_str("\n## User interaction\n\n");
    for e in a["timeline"].as_array().into_iter().flatten().filter(|e| matches!(s(e, "kind"), "wait" | "reply" | "turn")) {
        m.push_str(&format!("- {} **{}**: {}\n", s(e, "time"), s(e, "kind"), e["detail"]));
    }
    m.push_str("\n## Decisions\n\n");
    for e in a["timeline"].as_array().into_iter().flatten().filter(|e| matches!(s(e, "kind"), "decision" | "decision_rejected")) {
        let d = &e["detail"];
        m.push_str(&format!("- {} {} ({}): {} {}\n", s(e, "time"), s(d, "label"), s(e, "kind"), s(d, "action"), d.get("detail").or(d.get("reason")).map_or(String::new(), |x| x.to_string())));
    }
    m.push_str("\n## Claude accounts\n\n");
    for n in a["claude_accounts"].as_array().into_iter().flatten() {
        m.push_str(&format!("- {} {}: {}{}\n", s(n, "time"), s(n, "agent"), s(n, "account"), if n["refused"].as_bool() == Some(true) { " (refused by Claude)" } else { "" }));
    }
    m.push_str("\n## Network\n\n");
    for n in a["network"].as_array().into_iter().flatten() {
        m.push_str(&format!("- {} {}: {}\n", s(n, "time"), s(n, "agent"), s(n, "line")));
    }
    m
}

/// Writes the session's audit record to Downloads, as `json` or `md`; returns the path.
#[tauri::command]
pub async fn session_audit(app: AppHandle, id: String, format: String) -> Result<String, String> {
    let app2 = app.clone();
    let a = tauri::async_runtime::spawn_blocking(move || audit(&app2, &id)).await.map_err(|e| e.to_string())??;
    let id = a["session"]["id"].as_str().unwrap_or("session").to_string();
    let (ext, text) = match format.as_str() {
        "json" => ("json", serde_json::to_string_pretty(&a).map_err(|e| e.to_string())?),
        "md" => ("md", markdown(&a)),
        _ => return Err("the format is json or md".into()),
    };
    save_download(&app, &format!("ccaf-audit-{id}-{}.{ext}", stamp()), &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundles_are_checked() {
        assert!(parse_bundle("{}").is_err());
        assert!(parse_bundle(r#"{"format":"other","version":1}"#).is_err());
        assert!(parse_bundle(r#"{"format":"ccaf-bundle","version":99}"#).is_err());
        let b = parse_bundle(r#"{"format":"ccaf-bundle","version":1}"#).unwrap();
        assert!(b.workflows.is_empty() && b.profiles.is_empty() && b.images.is_empty());
    }

    #[test]
    fn a_bundle_roundtrips() {
        let b = Bundle {
            format: BUNDLE_FORMAT.into(),
            version: BUNDLE_VERSION,
            workflows: vec![BWorkflow { id: "w".into(), name: "W".into(), system_prompt: "p".into(), workflow: workflow::standard_workflow() }],
            ..Default::default()
        };
        let text = serde_json::to_string(&b).unwrap();
        let back = parse_bundle(&text).unwrap();
        assert_eq!(back.workflows[0].workflow, workflow::standard_workflow());
    }

    #[test]
    fn the_audit_markdown_names_the_parts() {
        let a = json!({"session": {"id": "s-1", "name": "", "status": "done", "created": "x", "autonomy": "ask", "workflow": "w", "task": "do it", "messages": ["more"]},
            "cost_usd": 1.5, "agent_runs": 2, "exported": "now", "result": "ok", "runs": [{"started": "t", "agent": "dev", "profile": "developer", "status": "done", "seconds": 4, "cost_usd": 0.5}],
            "timeline": [{"time": "t", "kind": "reply", "detail": {"text": "yes"}}], "network": [{"time": "t", "agent": "dev", "line": "egress: api.anthropic.com:443 allowed"}]});
        let m = markdown(&a);
        assert!(m.contains("s-1") && m.contains("Follow-up 1") && m.contains("| dev | developer |") && m.contains("api.anthropic.com") && m.contains("reply"));
    }
}
