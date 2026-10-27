//! Projects: a project has one workflow (an org chart of agents, edited visually) and any number
//! of sessions (one execution of the workflow for a task the user gives). Stored under
//! `<app data>/projects/<id>/`: `project.json`, `workflow.json` and `sessions/<sid>.json`.
//!
//! The workflow does not describe a sequence of steps. Its nodes are agents (roles) and an edge
//! `A -> B` means A may hand work to B. The one node nobody hands work to is the root: it gets the
//! user's request. An agent with outgoing edges coordinates the agents it may call: it decides
//! what to delegate, in what order, how often (loops and "for each" included) and when it is done;
//! an agent without them just does its work. The engine (`engine.rs`) enforces the edges.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const MAX_NODES: usize = 100;
const MAX_EDGES: usize = 300;

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
    /// Empty = untitled.
    pub name: String,
    /// What the user asked for (the first chat message).
    #[serde(default)]
    pub task: String,
    /// Unix seconds.
    #[serde(default)]
    pub created: u64,
    #[serde(default = "new_status")]
    pub status: String,
    /// Whether the agents may reach the user with their questions: `ask` or `never`.
    #[serde(default = "ask")]
    pub autonomy: String,
}

fn new_status() -> String {
    "new".into()
}

fn ask() -> String {
    "ask".into()
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionEntry {
    pub id: String,
    #[serde(flatten)]
    pub session: Session,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// An agent (a role backed by a profile).
    #[default]
    Agent,
    /// The user: an agent with an arrow to it may ask the user its questions directly.
    Ask,
    /// A gate on the way from one agent to another: each handover through it needs the user's approval.
    Approve,
}

/// A node of the workflow: an agent, or the user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    #[serde(default)]
    pub label: String,
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub kind: NodeKind,
    /// Agents only.
    #[serde(default)]
    pub profile: String,
    /// What this agent does in this workflow (added to its prompts).
    #[serde(default)]
    pub prompt: String,
    /// How many times it may be called in one session (1 to 500); bounds loops.
    #[serde(default = "twenty")]
    pub max_calls: u32,
    /// Minutes one run of it may take (1 to 480).
    #[serde(default = "thirty")]
    pub timeout_mins: u32,
}

fn twenty() -> u32 {
    20
}

fn thirty() -> u32 {
    30
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// `from` may hand work to `to` (an agent, or an approval gate leading to one), or ask the user (`to` is an `Ask` node).
    #[default]
    Delegate,
    /// Order: `to` may be called only after `from` has finished, within the same coordinator's call.
    Before,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Edge {
    pub id: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub kind: EdgeKind,
}

/// `from` may hand work to `to`, through the approval gate `gate` if there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallEdge {
    pub from: String,
    pub to: String,
    pub gate: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workflow {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
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
    fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Errors that make the data unusable (bad references, wrong shape).
    pub fn validate(&self) -> Result<(), String> {
        if self.nodes.len() > MAX_NODES || self.edges.len() > MAX_EDGES {
            return Err("the workflow is too large".into());
        }
        let mut ids = std::collections::HashSet::new();
        for n in &self.nodes {
            if n.id.is_empty() || n.id.len() > 64 || n.label.len() > 120 {
                return Err("a node has a bad id or label".into());
            }
            if !ids.insert(n.id.as_str()) {
                return Err(format!("node {} appears twice", n.id));
            }
            if n.kind != NodeKind::Agent {
                continue;
            }
            if n.profile.is_empty() {
                return Err("an agent needs a profile".into());
            }
            if !(1..=500).contains(&n.max_calls) {
                return Err("max calls must be between 1 and 500".into());
            }
            if !(1..=480).contains(&n.timeout_mins) {
                return Err("the timeout must be between 1 and 480 minutes".into());
            }
            if n.prompt.len() > 64 * 1024 {
                return Err("an agent's prompt is too large".into());
            }
        }
        let mut edge_ids = std::collections::HashSet::new();
        let mut pairs = std::collections::HashSet::new();
        for e in &self.edges {
            if !edge_ids.insert(e.id.as_str()) {
                return Err(format!("edge {} appears twice", e.id));
            }
            let (Some(from), Some(to)) = (self.node(&e.from), self.node(&e.to)) else {
                return Err("an edge points to a missing node".into());
            };
            if e.from == e.to {
                return Err("a node cannot point to itself".into());
            }
            if !pairs.insert((e.from.as_str(), e.to.as_str(), e.kind)) {
                return Err("two edges connect the same nodes".into());
            }
            match e.kind {
                EdgeKind::Before if from.kind != NodeKind::Agent || to.kind != NodeKind::Agent => {
                    return Err("an order arrow connects two agents".into());
                }
                EdgeKind::Delegate => {
                    if from.kind == NodeKind::Ask {
                        return Err("nothing leaves the user's node".into());
                    }
                    if from.kind == NodeKind::Approve && to.kind != NodeKind::Agent {
                        return Err("an approval hands work to an agent".into());
                    }
                }
                EdgeKind::Before => {}
            }
        }
        for g in self.nodes.iter().filter(|n| n.kind == NodeKind::Approve) {
            if self.edges.iter().filter(|e| e.from == g.id).count() > 1 {
                return Err("an approval gate leads to one agent".into());
            }
        }
        // an order that loops can never be satisfied
        let order: Vec<&Edge> = self.edges.iter().filter(|e| e.kind == EdgeKind::Before).collect();
        for start in &order {
            let (mut stack, mut seen) = (vec![start.to.as_str()], std::collections::HashSet::new());
            while let Some(id) = stack.pop() {
                if id == start.from {
                    return Err("the order arrows form a loop".into());
                }
                if seen.insert(id) {
                    stack.extend(order.iter().filter(|e| e.from == id).map(|e| e.to.as_str()));
                }
            }
        }
        Ok(())
    }

    /// Who may hand work to whom: agent to agent, or agent to agent through an approval gate.
    pub fn call_edges(&self) -> Vec<CallEdge> {
        let mut out = vec![];
        for e in self.edges.iter().filter(|e| e.kind == EdgeKind::Delegate) {
            let (Some(from), Some(to)) = (self.node(&e.from), self.node(&e.to)) else { continue };
            if from.kind != NodeKind::Agent {
                continue;
            }
            match to.kind {
                NodeKind::Agent => out.push(CallEdge { from: from.id.clone(), to: to.id.clone(), gate: None }),
                NodeKind::Approve => {
                    for e2 in self.edges.iter().filter(|x| x.from == to.id && x.kind == EdgeKind::Delegate) {
                        if self.node(&e2.to).is_some_and(|t| t.kind == NodeKind::Agent) {
                            out.push(CallEdge { from: from.id.clone(), to: e2.to.clone(), gate: Some(to.id.clone()) });
                        }
                    }
                }
                NodeKind::Ask => {}
            }
        }
        out
    }

    /// The user's node an agent is connected to, if any: it may ask the user directly.
    pub fn ask_node(&self, agent: &str) -> Option<&Node> {
        self.edges
            .iter()
            .filter(|e| e.from == agent && e.kind == EdgeKind::Delegate)
            .filter_map(|e| self.node(&e.to))
            .find(|n| n.kind == NodeKind::Ask)
    }

    /// The agents that must have finished before `agent` may be called (its order arrows).
    pub fn must_precede(&self, agent: &str) -> Vec<String> {
        self.edges.iter().filter(|e| e.kind == EdgeKind::Before && e.to == agent).map(|e| e.from.clone()).collect()
    }

    /// The agent nobody hands work to; it gets the user's request. `None` unless there is exactly one.
    pub fn root(&self) -> Option<&Node> {
        let calls = self.call_edges();
        let mut roots = self.nodes.iter().filter(|n| n.kind == NodeKind::Agent && !calls.iter().any(|c| c.to == n.id));
        let first = roots.next()?;
        roots.next().is_none().then_some(first)
    }

    /// Soft problems: a draft may have them, a run would refuse them.
    pub fn problems(&self, profiles: &[String]) -> Vec<Problem> {
        let mut out = vec![];
        if !self.nodes.iter().any(|n| n.kind == NodeKind::Agent) {
            return vec![problem("empty", None)];
        }
        for n in self.nodes.iter().filter(|n| n.kind == NodeKind::Agent) {
            if !profiles.contains(&n.profile) {
                out.push(problem("unknown_profile", Some(&n.id)));
            }
        }
        for n in self.nodes.iter().filter(|n| n.kind != NodeKind::Agent) {
            if !self.edges.iter().any(|e| e.to == n.id && e.kind == EdgeKind::Delegate) {
                out.push(problem("human_unused", Some(&n.id)));
            }
            if n.kind == NodeKind::Approve && !self.edges.iter().any(|e| e.from == n.id) {
                out.push(problem("gate_no_target", Some(&n.id)));
            }
        }
        let calls = self.call_edges();
        match self.root() {
            None => {
                let roots = self.nodes.iter().filter(|n| n.kind == NodeKind::Agent && !calls.iter().any(|c| c.to == n.id)).count();
                out.push(problem(if roots == 0 { "no_root" } else { "many_roots" }, None));
            }
            Some(root) => {
                let mut seen = std::collections::HashSet::new();
                let mut stack = vec![root.id.as_str()];
                while let Some(id) = stack.pop() {
                    if seen.insert(id) {
                        stack.extend(calls.iter().filter(|c| c.from == id).map(|c| c.to.as_str()));
                    }
                }
                for n in self.nodes.iter().filter(|n| n.kind == NodeKind::Agent && !seen.contains(n.id.as_str())) {
                    out.push(problem("unreachable", Some(&n.id)));
                }
            }
        }
        out
    }

    /// Reads a workflow file's JSON, converting the old step-graph format (start / agent /
    /// condition / human / end nodes) into an org chart: the agents stay, the edges between them
    /// stay, the ones back to the old coordinator and the return edges go.
    pub fn from_json(v: serde_json::Value) -> Option<Workflow> {
        let nodes = v.get("nodes")?.as_array()?;
        if !nodes.iter().any(|n| n.get("type").is_some()) {
            return serde_json::from_value(v).ok();
        }
        let text = |n: &serde_json::Value, k: &str| n.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let num = |n: &serde_json::Value, k: &str, d: u32| n.get(k).and_then(|x| x.as_u64()).map_or(d, |x| x as u32);
        let agents: Vec<Node> = nodes
            .iter()
            .filter(|n| n.get("type").and_then(|t| t.as_str()) == Some("agent"))
            .map(|n| Node {
                id: text(n, "id"),
                label: text(n, "label"),
                x: num(n, "x", 0) as i32,
                y: num(n, "y", 0) as i32,
                kind: NodeKind::Agent,
                profile: text(n, "profile"),
                prompt: text(n, "prompt"),
                max_calls: num(n, "max_runs", 20).max(20),
                timeout_mins: num(n, "timeout_mins", 30),
            })
            .collect();
        let coordinator = v.get("coordinator").and_then(|c| c.as_str()).unwrap_or("");
        let mut edges: Vec<Edge> = vec![];
        for e in v.get("edges")?.as_array()? {
            let (from, to) = (text(e, "from"), text(e, "to"));
            let known = |id: &str| agents.iter().any(|a| a.id == id);
            let linked = edges.iter().any(|k| (k.from == from && k.to == to) || (k.from == to && k.to == from));
            if known(&from) && known(&to) && to != coordinator && from != to && !linked {
                edges.push(Edge { id: format!("{from}-{to}"), from, to, kind: EdgeKind::Delegate });
            }
        }
        Some(Workflow { nodes: agents, edges })
    }
}

/// The standard delivery workflow, as an org chart: the orchestrator takes the request to the
/// project manager, who goes through the work items one by one, handing each to the delivery lead,
/// who has the business analyst, architect, developer, reviewer and tester do it, in that order
/// (the dashed arrows; the engine holds the lead to it). The business analyst may ask the user
/// directly. How often and in which loops is up to the agents, as their prompts say.
pub fn standard_workflow() -> Workflow {
    fn node(id: &str, label: &str, x: i32, y: i32, profile: &str, prompt: &str) -> Node {
        Node { id: id.into(), label: label.into(), x, y, kind: NodeKind::Agent, profile: profile.into(), prompt: prompt.into(), max_calls: 20, timeout_mins: 30 }
    }
    fn edge(from: &str, to: &str, kind: EdgeKind) -> Edge {
        let tag = if kind == EdgeKind::Before { "-before" } else { "" };
        Edge { id: format!("{from}-{to}{tag}"), from: from.into(), to: to.into(), kind }
    }
    let mut nodes = vec![
        node("orch", "Orchestrator", 0, 200, "orchestrator", "You receive the user's request. Restate it precisely, with its goal and what done means, and hand it to the project manager. When the project manager reports back, check the report against the request and finish with a short summary for the user: what was delivered and what was not."),
        node("pm", "Project manager", 280, 200, "project-manager", "Turn the request into work items. If it refers to a list of items (tickets, tasks), fetch the list yourself and go through the items one at a time: hand each to the delivery lead and wait for its report before the next. Keep your progress in your notes (items done, blocked, remaining). Finish with a summary per item."),
        node("lead", "Delivery lead", 560, 200, "delivery-lead", "Deliver one work item from start to finish, going through the specialists in order: the business analyst writes the requirements, the architect designs the change, the developer implements it in a branch and opens a pull request, the reviewer reviews it, the tester verifies the result once it is deployed. While the reviewer asks for changes, send the developer back, then the reviewer again; when the pull request is approved, have the developer merge it before the tester starts. Give each agent everything it needs (item id, links, branch, pull request number): it knows only what you tell it. Finish with a short report: delivered, or blocked and why."),
        node("ba", "Business analyst", 880, 0, "business-analyst", "Write testable requirements for the item you are given: user stories with acceptance criteria and the edge cases."),
        node("arch", "Architect", 880, 100, "architect", "Design the change for the item you are given: which parts of the code change and how, risks, and the order of the work. Do not write code."),
        node("dev", "Developer", 880, 200, "developer", "Implement what you are asked in the repository: work in a feature branch, run the tests, open a pull request and wait for its checks. When asked to, address review comments, and merge when the pull request is approved."),
        node("rev", "Reviewer", 880, 300, "reviewer", "Review the pull request you are given. Leave your comments on the pull request itself and report whether you approve it or ask for changes, with the list of changes."),
        node("qa", "Tester", 880, 400, "qa", "Test the item you are given against its acceptance criteria once it is deployed (watch the build until the deployment finishes), and report what passed and what failed, with steps to reproduce."),
    ];
    nodes.iter_mut().for_each(|n| n.max_calls = if n.id == "orch" || n.id == "pm" { 20 } else { 40 });
    nodes.push(Node { id: "you".into(), label: "You".into(), x: 1160, y: 0, kind: NodeKind::Ask, profile: String::new(), prompt: String::new(), max_calls: 20, timeout_mins: 30 });
    let mut edges = vec![edge("orch", "pm", EdgeKind::Delegate), edge("pm", "lead", EdgeKind::Delegate)];
    edges.extend(["ba", "arch", "dev", "rev", "qa"].map(|w| edge("lead", w, EdgeKind::Delegate)));
    edges.push(edge("ba", "you", EdgeKind::Delegate));
    edges.extend([("ba", "arch"), ("arch", "dev"), ("dev", "rev"), ("rev", "qa")].map(|(a, b)| edge(a, b, EdgeKind::Before)));
    Workflow { nodes, edges }
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

/// Saves a project; a new one starts with an empty workflow. Returns the id.
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
        write_json(&dir.join("workflow.json"), &Workflow::default())?;
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
    Ok(read_json::<serde_json::Value>(&dir.join("workflow.json")).and_then(Workflow::from_json).unwrap_or_default())
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
    if name.len() > 120 || task.len() > 64 * 1024 {
        return Err("the name is up to 120 characters and the task up to 64 KB".into());
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
    let session = Session { name: name.trim().into(), task: task.into(), created: now.as_secs(), status: new_status(), autonomy: ask() };
    write_json(&session_path(root, project, &id)?, &session)?;
    Ok(id)
}

/// Sets a session's name (empty = untitled) and/or its task (only before it starts).
pub fn edit_session(root: &Path, project: &str, sid: &str, name: Option<&str>, task: Option<&str>) -> Result<(), String> {
    let mut session = read_session(root, project, sid)?;
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
    write_json(&session_path(root, project, sid)?, &session)
}

pub fn set_session_autonomy(root: &Path, project: &str, sid: &str, autonomy: &str) -> Result<(), String> {
    if !matches!(autonomy, "ask" | "never") {
        return Err("autonomy is `ask` or `never`".into());
    }
    let mut session = read_session(root, project, sid)?;
    session.autonomy = autonomy.into();
    write_json(&session_path(root, project, sid)?, &session)
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
pub fn sessions_create(app: tauri::AppHandle, project: String) -> Result<String, String> {
    create_session(&root(&app)?, &project, "", "")
}

/// Renames a session; an empty name makes it untitled again.
#[tauri::command]
pub fn sessions_rename(app: tauri::AppHandle, project: String, id: String, name: String) -> Result<(), String> {
    edit_session(&root(&app)?, &project, &id, Some(&name), None)
}

/// Whether the agents may ask the user (`ask`) or must decide themselves (`never`).
#[tauri::command]
pub fn sessions_autonomy(app: tauri::AppHandle, project: String, id: String, autonomy: String) -> Result<(), String> {
    set_session_autonomy(&root(&app)?, &project, &id, &autonomy)
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
        ["orchestrator", "project-manager", "delivery-lead", "business-analyst", "architect", "developer", "reviewer", "qa"].map(String::from).to_vec()
    }

    fn n(id: &str) -> Node {
        Node { id: id.into(), label: String::new(), x: 0, y: 0, kind: NodeKind::Agent, profile: "developer".into(), prompt: String::new(), max_calls: 20, timeout_mins: 30 }
    }

    fn human(id: &str, kind: NodeKind) -> Node {
        Node { kind, profile: String::new(), ..n(id) }
    }

    fn e(from: &str, to: &str) -> Edge {
        Edge { id: format!("{from}-{to}"), from: from.into(), to: to.into(), kind: EdgeKind::Delegate }
    }

    fn before(from: &str, to: &str) -> Edge {
        Edge { id: format!("{from}-{to}-before"), kind: EdgeKind::Before, ..e(from, to) }
    }

    #[test]
    fn standard_workflow_is_valid_and_complete() {
        let wf = standard_workflow();
        wf.validate().unwrap();
        assert_eq!(wf.root().unwrap().id, "orch");
        assert_eq!(wf.problems(&profiles()), vec![]);
        assert_eq!(wf.problems(&[]).iter().filter(|p| p.code == "unknown_profile").count(), 8);
    }

    #[test]
    fn rejects_broken_workflows() {
        let mut wf = standard_workflow();
        wf.edges.push(e("pm", "nope"));
        assert!(wf.validate().is_err());

        let mut wf = standard_workflow();
        wf.edges.push(e("dev", "dev"));
        assert!(wf.validate().is_err(), "no self handover");

        let mut wf = standard_workflow();
        wf.edges.push(Edge { id: "again".into(), ..e("orch", "pm") });
        assert!(wf.validate().is_err(), "one edge per pair");

        let mut wf = standard_workflow();
        wf.nodes[0].max_calls = 0;
        assert!(wf.validate().is_err());
        wf.nodes[0].max_calls = 20;
        wf.nodes[0].timeout_mins = 481;
        assert!(wf.validate().is_err());
        wf.nodes[0].timeout_mins = 90;
        assert!(wf.validate().is_ok());
    }

    #[test]
    fn reports_soft_problems() {
        let mut wf = standard_workflow();
        wf.nodes.push(n("lonely"));
        wf.validate().unwrap();
        // a second node nobody calls: two roots
        assert!(wf.problems(&profiles()).iter().any(|p| p.code == "many_roots"));
        wf.edges.push(e("dev", "lonely"));
        assert_eq!(wf.problems(&profiles()), vec![]);
        // a cycle that swallows the root leaves no root
        wf.edges.push(e("lead", "orch"));
        assert!(wf.problems(&profiles()).iter().any(|p| p.code == "no_root"));
        assert!(Workflow::default().problems(&[]).iter().any(|p| p.code == "empty"));
    }

    #[test]
    fn human_nodes_and_order_arrows() {
        // a -> gate -> b: the handover goes through the approval; a may also ask the user
        let wf = Workflow {
            nodes: vec![n("a"), human("gate", NodeKind::Approve), n("b"), human("you", NodeKind::Ask)],
            edges: vec![e("a", "gate"), e("gate", "b"), e("a", "you")],
        };
        wf.validate().unwrap();
        assert_eq!(wf.call_edges(), vec![CallEdge { from: "a".into(), to: "b".into(), gate: Some("gate".into()) }]);
        assert_eq!(wf.root().unwrap().id, "a", "b is called through the gate, so it is not a root");
        assert_eq!(wf.ask_node("a").unwrap().id, "you");
        assert!(wf.ask_node("b").is_none());
        assert_eq!(wf.problems(&["developer".into()]), vec![]);

        // shapes that make no sense
        let mut bad = wf.clone();
        bad.edges.push(e("you", "b"));
        assert!(bad.validate().is_err(), "nothing leaves the user's node");
        let mut bad = wf.clone();
        bad.edges.push(e("gate", "you"));
        assert!(bad.validate().is_err(), "a gate hands work to an agent");
        let mut bad = wf.clone();
        bad.nodes.push(n("c"));
        bad.edges.push(e("gate", "c"));
        assert!(bad.validate().is_err(), "a gate leads to one agent");
        let mut bad = wf.clone();
        bad.edges.push(before("a", "you"));
        assert!(bad.validate().is_err(), "an order arrow connects agents");

        // unused human nodes and gates without a target are soft problems
        let mut draft = wf.clone();
        draft.edges.retain(|x| x.id != "gate-b" && x.id != "a-you");
        draft.nodes.push(n("c"));
        draft.edges.push(e("a", "c"));
        let codes: Vec<_> = draft.problems(&["developer".into()]).into_iter().map(|p| p.code).collect();
        assert!(codes.contains(&"human_unused") && codes.contains(&"gate_no_target"));

        // order arrows: who must come first, and a loop is refused
        let mut ord = Workflow { nodes: vec![n("l"), n("x"), n("y")], edges: vec![e("l", "x"), e("l", "y"), before("x", "y")] };
        ord.validate().unwrap();
        assert_eq!(ord.must_precede("y"), ["x"]);
        assert!(ord.must_precede("x").is_empty());
        ord.edges.push(before("y", "x"));
        assert!(ord.validate().is_err());
        assert_eq!(standard_workflow().must_precede("dev"), ["arch"]);
    }

    #[test]
    fn converts_the_old_step_graph() {
        let old = serde_json::json!({
            "nodes": [
                {"id": "start", "type": "start", "x": 0, "y": 0},
                {"id": "orch", "type": "agent", "profile": "orchestrator", "label": "O", "x": 1, "y": 2, "max_runs": 30},
                {"id": "ba", "type": "agent", "profile": "business-analyst", "prompt": "p", "timeout_mins": 45, "x": 0, "y": 0},
                {"id": "end", "type": "end", "outcome": "success", "x": 0, "y": 0}
            ],
            "edges": [
                {"id": "a", "from": "start", "to": "orch"},
                {"id": "b", "from": "orch", "to": "ba"},
                {"id": "c", "from": "ba", "to": "orch"},
                {"id": "d", "from": "orch", "to": "end"}
            ],
            "coordinator": "orch"
        });
        let wf = Workflow::from_json(old).unwrap();
        wf.validate().unwrap();
        assert_eq!(wf.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["orch", "ba"]);
        assert_eq!(wf.edges.len(), 1);
        assert_eq!((wf.nodes[0].max_calls, wf.nodes[1].timeout_mins, wf.nodes[1].prompt.as_str()), (30, 45, "p"));
        assert_eq!(wf.root().unwrap().id, "orch");
        // the new format passes through
        let new = serde_json::to_value(standard_workflow()).unwrap();
        assert_eq!(Workflow::from_json(new), Some(standard_workflow()));
    }

    #[test]
    fn project_prompt_comes_before_the_profile_prompt() {
        assert_eq!(combine_system_prompts("P", "Q").as_deref(), Some("P\n\nQ"));
        assert_eq!(combine_system_prompts(" ", "Q").as_deref(), Some("Q"));
        assert_eq!(combine_system_prompts("", " "), None);
    }

    #[test]
    fn projects_and_sessions_roundtrip() {
        let root = tmp("rt");
        let p = Project { name: "My App".into(), description: String::new(), system_prompt: String::new() };
        let id = save(&root, None, &p).unwrap();
        assert_eq!(id, "my-app");
        assert_eq!(save(&root, None, &p).unwrap(), "my-app-2");
        assert!(workflow(&root, &id).unwrap().nodes.is_empty());
        save_workflow(&root, &id, &standard_workflow()).unwrap();
        assert_eq!(workflow(&root, &id).unwrap(), standard_workflow());
        // saving the project again keeps its workflow
        save(&root, Some(&id), &Project { name: "Renamed".into(), description: String::new(), system_prompt: "x".into() }).unwrap();
        assert_eq!(workflow(&root, &id).unwrap().nodes.len(), 9);

        let s1 = create_session(&root, &id, "first", "do it").unwrap();
        let s2 = create_session(&root, &id, "second", "").unwrap();
        assert_ne!(s1, s2);
        assert_eq!(sessions(&root, &id).len(), 2);
        assert!(create_session(&root, &id, &"x".repeat(121), "").is_err());
        delete_session(&root, &id, &s1).unwrap();
        assert_eq!(sessions(&root, &id).len(), 1);

        assert!(delete(&root, "../x").is_err());
        delete(&root, &id).unwrap();
        assert!(workflow(&root, &id).is_err());
        assert_eq!(list(&root).len(), 1);
    }
}
