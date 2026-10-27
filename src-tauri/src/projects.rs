//! Projects: a project has one workflow (a graph of steps, edited visually) and any number of
//! sessions (one execution of the workflow for a task the user gives). Stored under
//! `<app data>/projects/<id>/`: `project.json`, `workflow.json` and `sessions/<sid>.json`.
//!
//! The workflow is data only; the engine that runs it does not exist yet. Conditions are
//! structured (a field of the previous agent's result, an operator, a value) so the engine can
//! evaluate them without running anything an agent wrote.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const MAX_NODES: usize = 200;
const MAX_EDGES: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Appended to the system prompt of every agent of the project, before the profile's own.
    #[serde(default)]
    pub system_prompt: String,
}

/// The system prompt an agent of a project gets: the project's, then the profile's.
#[allow(dead_code)] // used by the engine
pub fn combine_system_prompts(project: &str, profile: &str) -> Option<String> {
    let parts: Vec<&str> = [project.trim(), profile.trim()].into_iter().filter(|p| !p.is_empty()).collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectEntry {
    pub id: String,
    #[serde(flatten)]
    pub project: Project,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Session {
    pub name: String,
    /// What the user wants done; the workflow's first input.
    #[serde(default)]
    pub task: String,
    /// Unix seconds.
    #[serde(default)]
    pub created: u64,
    /// `new` until the engine exists.
    #[serde(default = "new_status")]
    pub status: String,
}

fn new_status() -> String {
    "new".into()
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionEntry {
    pub id: String,
    #[serde(flatten)]
    pub session: Session,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Eq,
    Ne,
    Contains,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HumanMode {
    /// The user approves or rejects; two exits, `approved` and `rejected`.
    Approve,
    /// The user types an answer that travels on; one exit.
    Input,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Failure,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    Start,
    /// Runs an agent profile once per visit, at most `max_runs` visits in a session.
    Agent {
        profile: String,
        #[serde(default)]
        prompt: String,
        /// Optional JSON schema of the structured result, for conditions downstream.
        #[serde(default)]
        schema: String,
        #[serde(default = "three")]
        max_runs: u32,
        /// How many agents of this profile work on the step at once: the engine picks a number
        /// between `min_instances` and `max_instances` and splits the work between them.
        #[serde(default = "one")]
        min_instances: u32,
        #[serde(default = "one")]
        max_instances: u32,
        /// Minutes an agent run may take before it is killed (the VM's boot and shutdown come on top).
        #[serde(default = "thirty")]
        timeout_mins: u32,
    },
    /// Branches on the previous agent's result: `status`, or `result.<key>`.
    Condition { field: String, op: Op, value: String },
    Human {
        mode: HumanMode,
        #[serde(default)]
        prompt: String,
    },
    End { outcome: Outcome },
}

fn three() -> u32 {
    3
}

fn one() -> u32 {
    1
}

fn thirty() -> u32 {
    30
}

impl Step {
    /// The exit labels edges leaving this step must use (empty: one unlabelled exit).
    pub fn branches(&self) -> &'static [&'static str] {
        match self {
            Step::Condition { .. } => &["true", "false"],
            Step::Human { mode: HumanMode::Approve, .. } => &["approved", "rejected"],
            _ => &[],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    #[serde(default)]
    pub label: String,
    pub x: i32,
    pub y: i32,
    #[serde(flatten)]
    pub step: Step,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Edge {
    pub id: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workflow {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// The agent step that coordinates the others (the orchestrator), if any. The engine hands the
    /// result of every step to it, by resuming its session, and it answers with a structured
    /// decision: which of the step's outgoing edges to take (for the coordinator itself: any of
    /// its edges, one is a worker or an end), with what instructions, and how to split the work
    /// across parallel instances. The engine rejects a decision that is not an edge of the graph,
    /// so the graph stays the set of allowed moves. Without a coordinator the engine follows the
    /// edges itself and puts each result into the next step's prompt.
    #[serde(default)]
    pub coordinator: Option<String>,
}

/// Something wrong with a workflow that does not stop it being saved (it may be a draft).
/// The UI turns the code into a message.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Problem {
    pub code: &'static str,
    pub node: Option<String>,
}

fn problem(code: &'static str, node: Option<&str>) -> Problem {
    Problem { code, node: node.map(str::to_string) }
}

impl Workflow {
    /// Errors that make the data unusable (bad references, wrong shape).
    pub fn validate(&self) -> Result<(), String> {
        if self.nodes.len() > MAX_NODES || self.edges.len() > MAX_EDGES {
            return Err("the workflow is too large".into());
        }
        let mut ids = std::collections::HashMap::new();
        for n in &self.nodes {
            if n.id.is_empty() || n.id.len() > 64 || n.label.len() > 120 {
                return Err("a step has a bad id or label".into());
            }
            if ids.insert(n.id.as_str(), n).is_some() {
                return Err(format!("step {} appears twice", n.id));
            }
            match &n.step {
                Step::Agent { profile, prompt, schema, max_runs, min_instances, max_instances, timeout_mins } => {
                    if profile.is_empty() {
                        return Err("an agent step needs a profile".into());
                    }
                    if !(1..=100).contains(max_runs) {
                        return Err("max runs must be between 1 and 100".into());
                    }
                    if !(1..=10).contains(min_instances) || !(*min_instances..=10).contains(max_instances) {
                        return Err("instances must be between 1 and 10, with min not above max".into());
                    }
                    if !(1..=480).contains(timeout_mins) {
                        return Err("the timeout must be between 1 and 480 minutes".into());
                    }
                    if prompt.len() > 64 * 1024 || schema.len() > 64 * 1024 {
                        return Err("an agent step is too large".into());
                    }
                    if !schema.trim().is_empty() && serde_json::from_str::<serde_json::Value>(schema).is_err() {
                        return Err(format!("the schema of {} is not valid JSON", n.id));
                    }
                }
                Step::Condition { field, value, .. } => {
                    let ok = field == "status" || field.strip_prefix("result.").is_some_and(|k| !k.is_empty());
                    if !ok || field.len() > 200 || value.len() > 1000 {
                        return Err("a condition needs `status` or `result.<key>` as its field".into());
                    }
                }
                Step::Human { prompt, .. } if prompt.len() > 16 * 1024 => return Err("a prompt is too large".into()),
                _ => {}
            }
        }
        if let Some(c) = &self.coordinator
            && !matches!(ids.get(c.as_str()).map(|n| &n.step), Some(Step::Agent { .. }))
        {
            return Err("the coordinator must be an agent step".into());
        }
        if self.nodes.iter().filter(|n| n.step == Step::Start).count() != 1 {
            return Err("a workflow has exactly one start".into());
        }
        let mut edge_ids = std::collections::HashSet::new();
        for e in &self.edges {
            if !edge_ids.insert(e.id.as_str()) {
                return Err(format!("edge {} appears twice", e.id));
            }
            let (Some(from), Some(to)) = (ids.get(e.from.as_str()), ids.get(e.to.as_str())) else {
                return Err("an edge points to a missing step".into());
            };
            if matches!(from.step, Step::End { .. }) || to.step == Step::Start {
                return Err("nothing leaves an end or enters the start".into());
            }
            let allowed = from.step.branches();
            match &e.branch {
                None if !allowed.is_empty() => return Err(format!("an edge from {} needs a branch", from.id)),
                Some(b) if !allowed.contains(&b.as_str()) => return Err(format!("an edge from {} has a wrong branch", from.id)),
                _ => {}
            }
        }
        Ok(())
    }

    /// Soft problems: a draft may have them, a run would refuse them.
    pub fn problems(&self, profiles: &[String]) -> Vec<Problem> {
        let mut out = vec![];
        let by_id: std::collections::HashMap<&str, &Node> = self.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        // reachability from the start
        let mut seen = std::collections::HashSet::new();
        let mut stack: Vec<&str> = self.nodes.iter().filter(|n| n.step == Step::Start).map(|n| n.id.as_str()).collect();
        while let Some(id) = stack.pop() {
            if seen.insert(id) {
                stack.extend(self.edges.iter().filter(|e| e.from == id).map(|e| e.to.as_str()));
            }
        }
        for n in &self.nodes {
            let outs: Vec<&Edge> = self.edges.iter().filter(|e| e.from == n.id).collect();
            if !seen.contains(n.id.as_str()) {
                out.push(problem("unreachable", Some(&n.id)));
            }
            if let Step::Agent { profile, .. } = &n.step
                && !profiles.contains(profile)
            {
                out.push(problem("unknown_profile", Some(&n.id)));
            }
            let branches = n.step.branches();
            if branches.is_empty() {
                match (&n.step, outs.len()) {
                    (Step::End { .. }, _) => {}
                    (_, 0) => out.push(problem("no_next", Some(&n.id))),
                    (_, 1) => {}
                    // the coordinator chooses among its exits
                    _ if self.coordinator.as_deref() == Some(n.id.as_str()) => {}
                    _ => out.push(problem("multi_out", Some(&n.id))),
                }
            } else {
                for b in branches {
                    match outs.iter().filter(|e| e.branch.as_deref() == Some(b)).count() {
                        0 => out.push(problem("missing_branch", Some(&n.id))),
                        1 => {}
                        _ => out.push(problem("duplicate_branch", Some(&n.id))),
                    }
                }
            }
        }
        if !by_id.values().any(|n| matches!(n.step, Step::End { .. })) {
            out.push(problem("no_end", None));
        }
        out
    }

    pub fn start_only() -> Self {
        Workflow { nodes: vec![Node { id: "start".into(), label: String::new(), x: 0, y: 0, step: Step::Start }], edges: vec![], coordinator: None }
    }
}

/// What the orchestrator answers after each step (see `Workflow::coordinator`): `next` names one of
/// the options the engine lists in its task, `instructions` go to that step, `parts` splits the
/// work between parallel instances.
pub const ORCHESTRATOR_SCHEMA: &str = r#"{"type":"object","properties":{"next":{"type":"string"},"instructions":{"type":"string"},"parts":{"type":"array","items":{"type":"string"}}},"required":["next","instructions"]}"#;

/// The standard delivery workflow, with the orchestrator as the hub: it starts the work and every
/// other step reports back to it. It decides whether the project manager, the business analyst, the
/// requirements review (the user), the developers (1 to 3 in parallel) or the tester go next, and
/// when the work is done.
pub fn standard_workflow() -> Workflow {
    fn node(id: &str, label: &str, x: i32, y: i32, step: Step) -> Node {
        Node { id: id.into(), label: label.into(), x, y, step }
    }
    fn agent(profile: &str, prompt: &str, schema: &str, max_runs: u32, instances: (u32, u32)) -> Step {
        Step::Agent { profile: profile.into(), prompt: prompt.into(), schema: schema.into(), max_runs, min_instances: instances.0, max_instances: instances.1, timeout_mins: 30 }
    }
    fn edge(from: &str, to: &str, branch: Option<&str>) -> Edge {
        Edge { id: format!("{from}-{to}{}", branch.map(|b| format!("-{b}")).unwrap_or_default()), from: from.into(), to: to.into(), branch: branch.map(Into::into) }
    }
    let test_schema = r#"{"type":"object","properties":{"passed":{"type":"boolean"},"report":{"type":"string"}},"required":["passed","report"]}"#;
    let mut edges = vec![edge("start", "orch", None)];
    for worker in ["pm", "ba", "dev", "test"] {
        edges.push(edge("orch", worker, None));
        edges.push(edge(worker, "orch", None));
    }
    edges.extend([
        edge("orch", "review", None),
        edge("review", "orch", Some("approved")),
        edge("review", "orch", Some("rejected")),
        edge("orch", "done", None),
    ]);
    Workflow {
        coordinator: Some("orch".into()),
        nodes: vec![
            node("start", "", 0, 240, Step::Start),
            node(
                "orch",
                "Orchestrator",
                260,
                240,
                agent(
                    "orchestrator",
                    "You coordinate the team. After every step you receive its result. Decide what happens next: the project manager to define the scope, the business analyst to write testable requirements, the requirements review by the user (always before any implementation starts), the developers to implement (split the work between them when it can be done in parallel), the tester to verify, and the end when the delivered work meets the goal. When the tester reports a failure, send the developers back with the report. Give the next step precise instructions.",
                    ORCHESTRATOR_SCHEMA,
                    30,
                    (1, 1),
                ),
            ),
            node("pm", "Project manager", 640, 0, agent("project-manager", "Define the scope: deliverables, milestones in order, dependencies, risks and open questions. Be short and concrete.", "", 3, (1, 1))),
            node("ba", "Business analyst", 640, 120, agent("business-analyst", "Write the requirements: user stories with acceptance criteria a tester can check, and the edge cases.", "", 4, (1, 1))),
            node("review", "Approve requirements", 640, 240, Step::Human { mode: HumanMode::Approve, prompt: "Review the requirements. Approve to let the implementation start, or reject with your comments.".into() }),
            node("dev", "Developer", 640, 360, agent("developer", "Implement what the orchestrator asks. When several developers work at once, each takes only its own part.", "", 5, (1, 3))),
            node("test", "Tester", 640, 480, agent("qa", "Test the result against the acceptance criteria. Report whether everything passed and, for each failure, the steps to reproduce it.", test_schema, 5, (1, 1))),
            node("done", "", 1020, 240, Step::End { outcome: Outcome::Success }),
        ],
        edges,
    }
}

// ---- storage ----

fn safe_id(id: &str) -> Result<&str, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err("bad id".into());
    }
    Ok(id)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

fn read_json<T: for<'a> Deserialize<'a>>(path: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn project_dir(root: &Path, id: &str) -> Result<PathBuf, String> {
    Ok(root.join(safe_id(id)?))
}

fn validate_project(p: &Project) -> Result<(), String> {
    if p.name.trim().is_empty() || p.name.len() > 80 {
        return Err("the name must be 1 to 80 characters".into());
    }
    if p.system_prompt.len() > 64 * 1024 {
        return Err("the system prompt is too large".into());
    }
    Ok(())
}

pub fn list(root: &Path) -> Vec<ProjectEntry> {
    let Ok(rd) = std::fs::read_dir(root) else { return vec![] };
    let mut out: Vec<ProjectEntry> = rd
        .flatten()
        .filter_map(|e| {
            let id = e.file_name().to_str()?.to_string();
            let project: Project = read_json(&e.path().join("project.json"))?;
            validate_project(&project).ok()?;
            Some(ProjectEntry { id, project })
        })
        .collect();
    out.sort_by_key(|x| x.project.name.to_lowercase());
    out
}

/// Saves a project; a new one starts with a workflow that only has its start. Returns the id.
pub fn save(root: &Path, id: Option<&str>, project: &Project) -> Result<String, String> {
    validate_project(project)?;
    let id = match id {
        Some(id) => id.to_string(),
        None => {
            let base = crate::profiles::slug(&project.name);
            if base.is_empty() {
                return Err("the name needs letters or digits".into());
            }
            let (mut id, mut n) = (base.clone(), 2);
            while root.join(&id).exists() {
                id = format!("{base}-{n}");
                n += 1;
            }
            id
        }
    };
    let dir = project_dir(root, &id)?;
    write_json(&dir.join("project.json"), project)?;
    if !dir.join("workflow.json").exists() {
        write_json(&dir.join("workflow.json"), &Workflow::start_only())?;
    }
    Ok(id)
}

pub fn delete(root: &Path, id: &str) -> Result<(), String> {
    std::fs::remove_dir_all(project_dir(root, id)?).map_err(|e| e.to_string())
}

pub fn workflow(root: &Path, id: &str) -> Result<Workflow, String> {
    let dir = project_dir(root, id)?;
    if !dir.join("project.json").exists() {
        return Err("no such project".into());
    }
    Ok(read_json(&dir.join("workflow.json")).unwrap_or_else(Workflow::start_only))
}

pub fn save_workflow(root: &Path, id: &str, wf: &Workflow) -> Result<(), String> {
    wf.validate()?;
    let dir = project_dir(root, id)?;
    if !dir.join("project.json").exists() {
        return Err("no such project".into());
    }
    write_json(&dir.join("workflow.json"), wf)
}

fn session_path(root: &Path, project: &str, sid: &str) -> Result<PathBuf, String> {
    Ok(project_dir(root, project)?.join("sessions").join(format!("{}.json", safe_id(sid)?)))
}

/// A file next to a session's own: `<sid>.<ext>` in the project's `sessions/` folder (`state.json`
/// and `log.jsonl` belong to the engine).
pub fn session_file(root: &Path, project: &str, sid: &str, ext: &str) -> Result<PathBuf, String> {
    Ok(project_dir(root, project)?.join("sessions").join(format!("{}.{ext}", safe_id(sid)?)))
}

pub fn read_session(root: &Path, project: &str, sid: &str) -> Result<Session, String> {
    read_json(&session_path(root, project, sid)?).ok_or_else(|| "no such session".to_string())
}

pub fn set_session_status(root: &Path, project: &str, sid: &str, status: &str) -> Result<(), String> {
    let mut session = read_session(root, project, sid)?;
    session.status = status.into();
    write_json(&session_path(root, project, sid)?, &session)
}

pub fn sessions(root: &Path, project: &str) -> Vec<SessionEntry> {
    let Ok(dir) = project_dir(root, project) else { return vec![] };
    let Ok(rd) = std::fs::read_dir(dir.join("sessions")) else { return vec![] };
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

pub fn create_session(root: &Path, project: &str, name: &str, task: &str) -> Result<String, String> {
    if name.trim().is_empty() || name.len() > 120 || task.len() > 64 * 1024 {
        return Err("the session needs a name (up to 120 characters)".into());
    }
    if !project_dir(root, project)?.join("project.json").exists() {
        return Err("no such project".into());
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?;
    let base = format!("s-{:x}", now.as_millis());
    let (mut id, mut n) = (base.clone(), 2);
    while session_path(root, project, &id)?.exists() {
        id = format!("{base}-{n}");
        n += 1;
    }
    let session = Session { name: name.trim().into(), task: task.into(), created: now.as_secs(), status: new_status() };
    write_json(&session_path(root, project, &id)?, &session)?;
    Ok(id)
}

pub fn delete_session(root: &Path, project: &str, sid: &str) -> Result<(), String> {
    for ext in ["state.json", "log.jsonl"] {
        let _ = std::fs::remove_file(session_file(root, project, sid, ext)?);
    }
    std::fs::remove_file(session_path(root, project, sid)?).map_err(|e| e.to_string())
}

pub fn root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("projects"))
}

/// The demo project with the standard workflow; an existing one is left alone.
pub fn provision_defaults(app: &tauri::AppHandle) -> Result<(), String> {
    let root = root(app)?;
    if root.join("demo").exists() {
        return Ok(());
    }
    let project = Project {
        name: "Demo".into(),
        description: "A sample project with the standard delivery workflow: an orchestrator coordinating a project manager, a business analyst, developers and a tester.".into(),
        system_prompt: String::new(),
    };
    let id = save(&root, Some("demo"), &project)?;
    save_workflow(&root, &id, &standard_workflow())
}

#[derive(Serialize)]
pub struct WorkflowView {
    pub workflow: Workflow,
    pub problems: Vec<Problem>,
}

fn view(app: &tauri::AppHandle, wf: Workflow) -> Result<WorkflowView, String> {
    use tauri::Manager;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("profiles");
    let ids: Vec<String> = crate::profiles::list(&dir).into_iter().map(|e| e.id).collect();
    Ok(WorkflowView { problems: wf.problems(&ids), workflow: wf })
}

#[tauri::command]
pub fn projects_list(app: tauri::AppHandle) -> Result<Vec<ProjectEntry>, String> {
    Ok(list(&root(&app)?))
}

#[tauri::command]
pub fn projects_save(app: tauri::AppHandle, id: Option<String>, project: Project) -> Result<String, String> {
    save(&root(&app)?, id.as_deref(), &project)
}

#[tauri::command]
pub fn projects_delete(app: tauri::AppHandle, id: String) -> Result<(), String> {
    delete(&root(&app)?, &id)
}

#[tauri::command]
pub fn workflow_get(app: tauri::AppHandle, project: String) -> Result<WorkflowView, String> {
    let wf = workflow(&root(&app)?, &project)?;
    view(&app, wf)
}

/// Saves the workflow; returns it with its (soft) problems.
#[tauri::command]
pub fn workflow_save(app: tauri::AppHandle, project: String, workflow: Workflow) -> Result<WorkflowView, String> {
    save_workflow(&root(&app)?, &project, &workflow)?;
    view(&app, workflow)
}

#[tauri::command]
pub fn workflow_standard(app: tauri::AppHandle) -> Result<WorkflowView, String> {
    view(&app, standard_workflow())
}

#[tauri::command]
pub fn sessions_list(app: tauri::AppHandle, project: String) -> Result<Vec<SessionEntry>, String> {
    Ok(sessions(&root(&app)?, &project))
}

#[tauri::command]
pub fn sessions_create(app: tauri::AppHandle, project: String, name: String, task: String) -> Result<String, String> {
    create_session(&root(&app)?, &project, &name, &task)
}

#[tauri::command]
pub fn sessions_delete(app: tauri::AppHandle, project: String, id: String) -> Result<(), String> {
    delete_session(&root(&app)?, &project, &id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ccaf-proj-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn profiles() -> Vec<String> {
        ["orchestrator", "project-manager", "business-analyst", "developer", "qa"].map(String::from).to_vec()
    }

    #[test]
    fn standard_workflow_is_valid_and_complete() {
        let wf = standard_workflow();
        wf.validate().unwrap();
        assert_eq!(wf.problems(&profiles()), vec![]);
        assert_eq!(wf.problems(&[]).iter().filter(|p| p.code == "unknown_profile").count(), 5);
    }

    #[test]
    fn rejects_broken_workflows() {
        let mut wf = standard_workflow();
        wf.edges.push(Edge { id: "x".into(), from: "pm".into(), to: "nope".into(), branch: None });
        assert!(wf.validate().is_err());

        let mut wf = standard_workflow();
        wf.edges.iter_mut().find(|e| e.from == "review" && e.branch.as_deref() == Some("approved")).unwrap().branch = None;
        assert!(wf.validate().is_err());

        let mut wf = standard_workflow();
        wf.edges.push(Edge { id: "back".into(), from: "dev".into(), to: "start".into(), branch: None });
        assert!(wf.validate().is_err());

        let mut wf = standard_workflow();
        wf.coordinator = Some("review".into());
        assert!(wf.validate().is_err(), "a human step cannot coordinate");

        let mut wf = standard_workflow();
        wf.nodes.retain(|n| n.id != "start");
        wf.edges.retain(|e| e.from != "start");
        assert!(wf.validate().is_err());
    }

    #[test]
    fn validates_instances() {
        let mut wf = standard_workflow();
        let dev = wf.nodes.iter_mut().find(|n| n.id == "dev").unwrap();
        let Step::Agent { min_instances, max_instances, .. } = &mut dev.step else { panic!() };
        assert_eq!((*min_instances, *max_instances), (1, 3));
        *min_instances = 4;
        assert!(wf.validate().is_err());
        // old files without the fields mean one instance
        let old: Step = serde_json::from_str(r#"{"type":"agent","profile":"x"}"#).unwrap();
        assert!(matches!(old, Step::Agent { min_instances: 1, max_instances: 1, timeout_mins: 30, .. }));
        let with_timeout = |m: u32| {
            let mut w = standard_workflow();
            if let Step::Agent { timeout_mins, .. } = &mut w.nodes.iter_mut().find(|n| n.id == "dev").unwrap().step {
                *timeout_mins = m;
            }
            w.validate()
        };
        assert!(with_timeout(0).is_err() && with_timeout(481).is_err());
        assert!(with_timeout(90).is_ok());
    }

    #[test]
    fn reports_soft_problems() {
        let mut wf = standard_workflow();
        wf.edges.retain(|e| !(e.from == "review" && e.branch.as_deref() == Some("rejected")));
        wf.nodes.push(Node { id: "lonely".into(), label: String::new(), x: 0, y: 0, step: Step::End { outcome: Outcome::Failure } });
        wf.validate().unwrap();
        let codes: Vec<_> = wf.problems(&profiles()).into_iter().map(|p| (p.code, p.node.unwrap_or_default())).collect();
        assert!(codes.contains(&("missing_branch", "review".into())));
        assert!(codes.contains(&("unreachable", "lonely".into())));
        // only the coordinator may have several exits
        wf.coordinator = None;
        assert!(wf.problems(&profiles()).iter().any(|p| p.code == "multi_out" && p.node.as_deref() == Some("orch")));
    }

    #[test]
    fn conditions_need_both_branches() {
        let mut wf = Workflow::start_only();
        wf.nodes.push(Node { id: "c".into(), label: String::new(), x: 0, y: 0, step: Step::Condition { field: "result.passed".into(), op: Op::Eq, value: "true".into() } });
        wf.nodes.push(Node { id: "e".into(), label: String::new(), x: 0, y: 0, step: Step::End { outcome: Outcome::Success } });
        let e = |id: &str, b: Option<&str>| Edge { id: id.into(), from: if id == "s" { "start".into() } else { "c".into() }, to: if id == "s" { "c".into() } else { "e".into() }, branch: b.map(Into::into) };
        wf.edges = vec![e("s", None), e("t", Some("true"))];
        wf.validate().unwrap();
        assert!(wf.problems(&[]).iter().any(|p| p.code == "missing_branch"));
        wf.edges.push(e("f", Some("false")));
        assert!(wf.problems(&[]).is_empty());
        wf.nodes[1].step = Step::Condition { field: "result".into(), op: Op::Eq, value: String::new() };
        assert!(wf.validate().is_err());
    }

    #[test]
    fn project_prompt_comes_before_the_profile_prompt() {
        assert_eq!(combine_system_prompts("P", "Q").as_deref(), Some("P\n\nQ"));
        assert_eq!(combine_system_prompts(" ", "Q").as_deref(), Some("Q"));
        assert_eq!(combine_system_prompts("", " "), None);
    }

    #[test]
    fn json_shape_is_flat() {
        let json = serde_json::to_value(standard_workflow()).unwrap();
        assert_eq!(json["nodes"][1]["type"], "agent");
        assert_eq!(json["nodes"][1]["profile"], "orchestrator");
        let back: Workflow = serde_json::from_value(json).unwrap();
        assert_eq!(back, standard_workflow());
    }

    #[test]
    fn projects_and_sessions_roundtrip() {
        let root = tmp("rt");
        let p = Project { name: "My App".into(), description: String::new(), system_prompt: String::new() };
        let id = save(&root, None, &p).unwrap();
        assert_eq!(id, "my-app");
        assert_eq!(save(&root, None, &p).unwrap(), "my-app-2");
        assert_eq!(workflow(&root, &id).unwrap().nodes.len(), 1);
        save_workflow(&root, &id, &standard_workflow()).unwrap();
        assert_eq!(workflow(&root, &id).unwrap(), standard_workflow());
        // saving the project again keeps its workflow
        save(&root, Some(&id), &Project { name: "Renamed".into(), description: String::new(), system_prompt: "x".into() }).unwrap();
        assert_eq!(workflow(&root, &id).unwrap().nodes.len(), 8);

        let s1 = create_session(&root, &id, "first", "do it").unwrap();
        let s2 = create_session(&root, &id, "second", "").unwrap();
        assert_ne!(s1, s2);
        assert_eq!(sessions(&root, &id).len(), 2);
        assert!(create_session(&root, &id, " ", "").is_err());
        delete_session(&root, &id, &s1).unwrap();
        assert_eq!(sessions(&root, &id).len(), 1);

        assert!(delete(&root, "../x").is_err());
        delete(&root, &id).unwrap();
        assert!(workflow(&root, &id).is_err());
        assert_eq!(list(&root).len(), 1);
    }
}
