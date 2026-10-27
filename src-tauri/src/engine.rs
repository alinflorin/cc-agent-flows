//! The workflow engine: runs a session, walking the project's workflow graph (`projects.rs`).
//!
//! The graph is the set of allowed moves. A plain step has one exit; a condition or an approval has
//! two, picked by the result; the *coordinator* (the orchestrator, `Workflow::coordinator`) has
//! several and picks one by a structured decision, which the engine checks against the graph before
//! acting. Every step's result goes into a history that the next prompts carry: each agent run is a
//! fresh VM with no memory of the earlier ones (a Claude session cannot be resumed in another VM),
//! so the engine rebuilds the context every time. Humans take part through approvals, free-text
//! answers, and the questions an agent asks when it ends with `needs_input`.
//!
//! `drive` holds the logic and talks to the outside only through `Host` (running an agent, logging,
//! saving), so tests use a scripted host. `RealHost` is the one that boots VMs. The state is saved
//! at every transition, so a session survives a restart of the app (`session_continue`).

use std::collections::HashMap;
use std::future::Future;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, oneshot, watch};

use crate::profiles::Profile;
use crate::projects::{HumanMode, Node, Op, Outcome, Step, Workflow};
use crate::protocol::{Final, Outbound, Status, Task, TaskEnv};
use crate::runs::{Spec, failed};

/// Agent runs (VMs) one session may start, whatever the per-step limits say.
const MAX_RUNS: u32 = 300;
/// How much history the prompts carry.
const HISTORY_CHARS: usize = 24_000;
const ITEM_CHARS: usize = 6_000;
/// Attempts the coordinator gets to give a decision the graph allows.
const DECISION_ATTEMPTS: u32 = 2;

// ---- state ----

/// The result of one step, as the following prompts and conditions see it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Done {
    pub node: String,
    pub label: String,
    /// `done | needs_input | failed`
    pub status: String,
    pub summary: String,
    #[serde(default)]
    pub output: Option<Value>,
    #[serde(default)]
    pub questions: Vec<String>,
    #[serde(default)]
    pub cost_usd: f64,
}

/// What a session is waiting for the user to answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pending {
    pub node: String,
    /// `approve | input | questions`
    pub kind: String,
    pub prompt: String,
    #[serde(default)]
    pub questions: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct State {
    pub current: Option<String>,
    /// Completed visits per step, against `max_runs`.
    #[serde(default)]
    pub visits: HashMap<String, u32>,
    #[serde(default)]
    pub history: Vec<Done>,
    /// Instructions for the next step (from the coordinator, or the user's answers).
    #[serde(default)]
    pub carry: Option<String>,
    /// The coordinator's split of the work for the next step's parallel instances.
    #[serde(default)]
    pub parts: Vec<String>,
    #[serde(default)]
    pub pending: Option<Pending>,
    #[serde(default)]
    pub runs: u32,
    #[serde(default)]
    pub cost_usd: f64,
}

/// The user's answer to a `Pending`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Reply {
    /// For `approve`.
    #[serde(default)]
    pub approved: Option<bool>,
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, PartialEq)]
pub enum Ended {
    Done,
    Failed(String),
    Cancelled,
}

pub struct AgentReq {
    pub node: String,
    pub instance: u32,
    pub instances: u32,
    pub profile: String,
    pub prompt: String,
    /// JSON schema of the answer; empty for the default one.
    pub schema: String,
    /// Minutes the run may take.
    pub timeout_mins: u32,
}

pub trait Host: Send + Sync + 'static {
    /// Runs one agent to its end. Never fails: a problem is a `failed` final.
    fn run_agent(&self, req: AgentReq) -> impl Future<Output = Final> + Send;
    fn log(&self, kind: &str, body: Value);
    /// Saves the state and sets the session's status (`running | waiting`).
    fn checkpoint(&self, state: &State, status: &str);
}

// ---- prompts ----

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push('…');
    out
}

/// The history as text, newest entries kept first when it is too long.
pub fn render_history(history: &[Done]) -> String {
    if history.is_empty() {
        return "Nothing has been done yet.".into();
    }
    let items: Vec<String> = history
        .iter()
        .map(|d| {
            let mut s = format!("### {} ({})\n{}", d.label, d.status, clip(&d.summary, ITEM_CHARS));
            if let Some(o) = &d.output {
                s.push_str(&format!("\nStructured result: {}", clip(&o.to_string(), 2000)));
            }
            s
        })
        .collect();
    let (mut kept, mut total) = (vec![], 0);
    for item in items.iter().rev() {
        if total + item.len() > HISTORY_CHARS && !kept.is_empty() {
            kept.push("(earlier steps left out)".to_string());
            break;
        }
        total += item.len();
        kept.push(item.clone());
    }
    kept.reverse();
    kept.join("\n\n")
}

fn section(title: &str, body: &str) -> String {
    format!("# {title}\n{}\n\n", body.trim())
}

pub fn worker_prompt(task: &str, history: &[Done], step_prompt: &str, carry: Option<&str>, part: Option<(u32, u32, &str)>) -> String {
    let mut p = section("Task of the session", task);
    p += &section("Work done so far", &render_history(history));
    if !step_prompt.trim().is_empty() {
        p += &section("Your step", step_prompt);
    }
    if let Some(c) = carry.filter(|c| !c.trim().is_empty()) {
        p += &section("Instructions for this run", c);
    }
    if let Some((i, n, text)) = part {
        p += &section("Your part", &format!("You are instance {i} of {n}, working in parallel with the others. Do only this part: {text}"));
    }
    p.trim_end().to_string()
}

/// A way out of the coordinator: a step it may send the work to.
#[derive(Debug, Clone, PartialEq)]
pub struct Opt {
    pub id: String,
    pub label: String,
    pub desc: String,
    pub min: u32,
    pub max: u32,
}

fn label_of(n: &Node) -> String {
    if !n.label.trim().is_empty() {
        return n.label.clone();
    }
    match &n.step {
        Step::Agent { profile, .. } => profile.clone(),
        _ => n.id.clone(),
    }
}

pub fn coordinator_options(wf: &Workflow, coord: &str) -> Vec<Opt> {
    let mut out: Vec<Opt> = vec![];
    for e in wf.edges.iter().filter(|e| e.from == coord) {
        let Some(n) = wf.nodes.iter().find(|n| n.id == e.to) else { continue };
        if out.iter().any(|o| o.id == n.id) {
            continue;
        }
        let (desc, min, max) = match &n.step {
            Step::Agent { profile, min_instances, max_instances, .. } => {
                let mut d = format!("the {profile} agent");
                if max_instances > &1 {
                    d += &format!(" (between {min_instances} and {max_instances} instances in parallel: put one entry per instance in `parts`)");
                }
                (d, *min_instances, *max_instances)
            }
            Step::Human { mode, prompt, .. } => {
                let how = if *mode == HumanMode::Approve { "approves or rejects" } else { "answers" };
                (format!("the user {how}: {}", clip(prompt, 200)), 1, 1)
            }
            Step::End { outcome } => (format!("end the session ({})", if *outcome == Outcome::Success { "success" } else { "failure" }), 1, 1),
            Step::Condition { .. } => ("a check on the last result".into(), 1, 1),
            Step::Start => continue,
        };
        out.push(Opt { id: n.id.clone(), label: label_of(n), desc, min, max });
    }
    out
}

pub fn coordinator_prompt(task: &str, history: &[Done], step_prompt: &str, options: &[Opt], carry: Option<&str>, feedback: Option<&str>) -> String {
    let mut p = section("Task of the session", task);
    p += &section("What has happened so far", &render_history(history));
    if !step_prompt.trim().is_empty() {
        p += &section("Your role", step_prompt);
    }
    if let Some(c) = carry.filter(|c| !c.trim().is_empty()) {
        p += &section("Note", c);
    }
    let list: Vec<String> = options.iter().map(|o| format!("- `{}`: {} ({})", o.id, o.label, o.desc)).collect();
    p += &section(
        "Decide the next step",
        &format!(
            "Choose where the work goes next, from exactly these options:\n{}\n\nAnswer with `next` (one of the ids above), `instructions` (what that step must do, precise and complete: it knows only what you tell it and the history above) and, only when the step runs several instances, `parts` (one entry per instance).",
            list.join("\n")
        ),
    );
    if let Some(f) = feedback {
        p += &section("Your previous answer was rejected", f);
    }
    p.trim_end().to_string()
}

/// Reads the coordinator's answer and checks it against the options. Returns the chosen option,
/// the instructions and the parts for its instances.
pub fn parse_decision<'a>(output: Option<&Value>, options: &'a [Opt]) -> Result<(&'a Opt, String, Vec<String>), String> {
    let Some(o) = output.and_then(Value::as_object) else {
        return Err("the answer was not the structured object with `next` and `instructions`".into());
    };
    let next = o.get("next").and_then(Value::as_str).unwrap_or("").trim();
    let Some(opt) = options.iter().find(|x| x.id == next).or_else(|| options.iter().find(|x| x.label.eq_ignore_ascii_case(next))) else {
        let ids: Vec<&str> = options.iter().map(|x| x.id.as_str()).collect();
        return Err(format!("`next` was {next:?}, which is not one of the options: {}", ids.join(", ")));
    };
    let instructions = o.get("instructions").and_then(Value::as_str).unwrap_or("").to_string();
    let parts: Vec<String> = o.get("parts").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    if opt.max > 1 && !parts.is_empty() && !(opt.min as usize..=opt.max as usize).contains(&parts.len()) {
        return Err(format!("`parts` has {} entries, but {} takes between {} and {} instances", parts.len(), opt.label, opt.min, opt.max));
    }
    // a step that cannot run in parallel ignores parts
    let parts = if opt.max > 1 { parts } else { vec![] };
    Ok((opt, instructions, parts))
}

// ---- graph ----

fn node<'a>(wf: &'a Workflow, id: &str) -> Result<&'a Node, String> {
    wf.nodes.iter().find(|n| n.id == id).ok_or_else(|| format!("the step {id} does not exist (the workflow changed?)"))
}

/// The one exit of a plain step.
fn single_next(wf: &Workflow, n: &Node) -> Result<String, String> {
    let outs: Vec<_> = wf.edges.iter().filter(|e| e.from == n.id).collect();
    match outs.as_slice() {
        [e] => Ok(e.to.clone()),
        [] => Err(format!("{} has no next step", label_of(n))),
        _ => Err(format!("{} has several exits and is not the coordinator", label_of(n))),
    }
}

fn branch_next(wf: &Workflow, n: &Node, branch: &str) -> Result<String, String> {
    wf.edges
        .iter()
        .find(|e| e.from == n.id && e.branch.as_deref() == Some(branch))
        .map(|e| e.to.clone())
        .ok_or_else(|| format!("{} has no `{branch}` exit", label_of(n)))
}

fn lookup<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |v, k| v.get(k))
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Array(a) => a.iter().map(text_of).collect::<Vec<_>>().join(","),
        other => other.to_string(),
    }
}

/// A condition on the last result: its `status`, or `result.<path>` in its structured output.
pub fn eval_condition(field: &str, op: Op, value: &str, last: Option<&Done>) -> bool {
    let actual = match (field, last) {
        (_, None) => String::new(),
        ("status", Some(d)) => d.status.clone(),
        (f, Some(d)) => f
            .strip_prefix("result.")
            .and_then(|path| d.output.as_ref().and_then(|o| lookup(o, path)))
            .map(text_of)
            .unwrap_or_default(),
    };
    match op {
        Op::Eq => actual == value,
        Op::Ne => actual != value,
        Op::Contains => actual.contains(value),
    }
}

// ---- running ----

fn done_from(n: &Node, f: &Final) -> Done {
    Done {
        node: n.id.clone(),
        label: label_of(n),
        status: match f.status {
            Status::Done => "done",
            Status::NeedsInput => "needs_input",
            Status::Failed => "failed",
        }
        .into(),
        summary: f.result.clone(),
        output: f.output.clone(),
        questions: f.questions.clone(),
        cost_usd: f.cost_usd.unwrap_or(0.0),
    }
}

/// Folds the runs of a step's parallel instances into one result.
fn aggregate(n: &Node, finals: &[Final]) -> Done {
    if let [one] = finals {
        return done_from(n, one);
    }
    let dones: Vec<Done> = finals.iter().map(|f| done_from(n, f)).collect();
    let status = if dones.iter().any(|d| d.status == "failed") {
        "failed"
    } else if dones.iter().any(|d| d.status == "needs_input") {
        "needs_input"
    } else {
        "done"
    };
    Done {
        node: n.id.clone(),
        label: label_of(n),
        status: status.into(),
        summary: dones.iter().enumerate().map(|(i, d)| format!("Instance {} ({}): {}", i + 1, d.status, d.summary)).collect::<Vec<_>>().join("\n\n"),
        output: Some(json!({"instances": dones.iter().map(|d| d.output.clone()).collect::<Vec<_>>()})),
        questions: dones.iter().flat_map(|d| d.questions.clone()).collect(),
        cost_usd: dones.iter().map(|d| d.cost_usd).sum(),
    }
}

async fn wait_reply(cancel: &watch::Receiver<bool>, replies: &mut mpsc::Receiver<Reply>) -> Option<Reply> {
    let mut c = cancel.clone();
    tokio::select! {
        r = replies.recv() => r,
        _ = c.wait_for(|v| *v) => None,
    }
}

fn step_end(host: &impl Host, d: &Done) {
    host.log("step_end", json!({"node": d.node, "label": d.label, "status": d.status, "summary": d.summary, "output": d.output, "cost_usd": d.cost_usd}));
}

/// Runs the session until it ends. `state` is updated (and saved through the host) as it goes.
pub async fn drive<H: Host>(
    host: Arc<H>,
    wf: &Workflow,
    task: &str,
    state: &mut State,
    cancel: watch::Receiver<bool>,
    replies: &mut mpsc::Receiver<Reply>,
) -> Ended {
    macro_rules! fail {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(e) => return Ended::Failed(e),
            }
        };
    }
    loop {
        if *cancel.borrow() {
            return Ended::Cancelled;
        }

        // The user's turn.
        if let Some(p) = state.pending.clone() {
            host.checkpoint(state, "waiting");
            let Some(reply) = wait_reply(&cancel, replies).await else { return Ended::Cancelled };
            let n = fail!(node(wf, &p.node));
            if p.kind == "approve" && reply.approved.is_none() {
                continue; // an approval needs yes or no
            }
            host.log("reply", json!({"node": p.node, "approved": reply.approved, "text": reply.text}));
            let text = clip(reply.text.trim(), 20_000);
            match p.kind.as_str() {
                "approve" => {
                    let ok = reply.approved == Some(true);
                    let summary = format!("The user {} it.{}", if ok { "approved" } else { "rejected" }, if text.is_empty() { String::new() } else { format!(" Comment: {text}") });
                    let d = Done { node: n.id.clone(), label: label_of(n), status: "done".into(), summary, output: Some(json!({"approved": ok, "comment": text})), questions: vec![], cost_usd: 0.0 };
                    state.current = Some(fail!(branch_next(wf, n, if ok { "approved" } else { "rejected" })));
                    step_end(&*host, &d);
                    state.history.push(d);
                }
                "input" => {
                    let d = Done { node: n.id.clone(), label: label_of(n), status: "done".into(), summary: text.clone(), output: Some(json!({"answer": text})), questions: vec![], cost_usd: 0.0 };
                    state.current = Some(fail!(single_next(wf, n)));
                    step_end(&*host, &d);
                    state.history.push(d);
                }
                _ => {
                    // an agent's questions: run it again with the answer
                    let qa = format!("You stopped to ask the user:\n{}\n\nThe user answered:\n{text}\n\nContinue the work with this answer.", p.questions.join("\n"));
                    state.carry = Some(match state.carry.take() {
                        Some(c) if !c.trim().is_empty() => format!("{c}\n\n{qa}"),
                        _ => qa,
                    });
                    state.current = Some(p.node.clone());
                }
            }
            state.pending = None;
            host.checkpoint(state, "running");
            continue;
        }

        let Some(cur) = state.current.clone() else { return Ended::Failed("the session has no current step".into()) };
        let n = fail!(node(wf, &cur));
        match &n.step {
            Step::Start => state.current = Some(fail!(single_next(wf, n))),

            Step::Condition { field, op, value } => {
                let ok = eval_condition(field, *op, value, state.history.last());
                host.log("condition", json!({"node": n.id, "field": field, "result": ok}));
                state.current = Some(fail!(branch_next(wf, n, if ok { "true" } else { "false" })));
            }

            Step::Human { mode, prompt } => {
                let kind = if *mode == HumanMode::Approve { "approve" } else { "input" };
                state.pending = Some(Pending { node: n.id.clone(), kind: kind.into(), prompt: prompt.clone(), questions: vec![] });
                host.log("wait", json!({"node": n.id, "label": label_of(n), "kind": kind, "prompt": prompt}));
            }

            Step::End { outcome } => {
                host.log("end", json!({"node": n.id, "outcome": if *outcome == Outcome::Success { "success" } else { "failure" }}));
                return match outcome {
                    Outcome::Success => Ended::Done,
                    Outcome::Failure => Ended::Failed("the workflow ended in failure".into()),
                };
            }

            Step::Agent { profile, prompt, schema, max_runs, min_instances, max_instances, timeout_mins } => {
                let is_coord = wf.coordinator.as_deref() == Some(n.id.as_str());
                let visits = state.visits.get(&n.id).copied().unwrap_or(0);
                if visits >= *max_runs {
                    return Ended::Failed(format!("{} already ran {visits} times, its limit", label_of(n)));
                }

                // How many instances, and what each one does.
                let count = if is_coord {
                    1
                } else if state.parts.is_empty() {
                    *min_instances
                } else {
                    (state.parts.len() as u32).clamp(*min_instances, *max_instances)
                };
                if state.runs + count > MAX_RUNS {
                    return Ended::Failed(format!("the session reached its limit of {MAX_RUNS} agent runs"));
                }
                host.log("step", json!({"node": n.id, "label": label_of(n), "profile": profile, "instances": count}));
                host.checkpoint(state, "running");

                let options = if is_coord { coordinator_options(wf, &n.id) } else { vec![] };
                if is_coord && options.is_empty() {
                    return Ended::Failed("the coordinator has no exits".into());
                }

                // Run (the coordinator may need a second try to give an allowed decision).
                let mut feedback: Option<String> = None;
                let mut attempt = 0;
                let (done, decision) = loop {
                    attempt += 1;
                    let finals = if is_coord {
                        let req = AgentReq {
                            node: n.id.clone(),
                            instance: 1,
                            instances: 1,
                            profile: profile.clone(),
                            prompt: coordinator_prompt(task, &state.history, prompt, &options, state.carry.as_deref(), feedback.as_deref()),
                            schema: crate::projects::ORCHESTRATOR_SCHEMA.into(),
                            timeout_mins: *timeout_mins,
                        };
                        vec![host.run_agent(req).await]
                    } else {
                        let mut set = tokio::task::JoinSet::new();
                        for i in 0..count {
                            let part = state.parts.get(i as usize).map(String::as_str);
                            let req = AgentReq {
                                node: n.id.clone(),
                                instance: i + 1,
                                instances: count,
                                profile: profile.clone(),
                                prompt: worker_prompt(task, &state.history, prompt, state.carry.as_deref(), part.map(|t| (i + 1, count, t))),
                                schema: schema.clone(),
                                timeout_mins: *timeout_mins,
                            };
                            let h = host.clone();
                            set.spawn(async move { (i, h.run_agent(req).await) });
                        }
                        let mut slots: Vec<Option<Final>> = (0..count).map(|_| None).collect();
                        while let Some(r) = set.join_next().await {
                            match r {
                                Ok((i, f)) => slots[i as usize] = Some(f),
                                Err(e) => return Ended::Failed(format!("an agent task crashed: {e}")),
                            }
                        }
                        slots.into_iter().map(|f| f.unwrap_or_else(|| failed("no result"))).collect()
                    };
                    state.runs += finals.len() as u32;
                    let done = aggregate(n, &finals);
                    state.cost_usd += done.cost_usd;
                    if *cancel.borrow() {
                        return Ended::Cancelled;
                    }
                    if !is_coord || done.status != "done" {
                        break (done, None);
                    }
                    match parse_decision(done.output.as_ref(), &options) {
                        Ok((opt, instr, parts)) => break (done, Some((opt.clone(), instr, parts))),
                        Err(e) if attempt < DECISION_ATTEMPTS => {
                            host.log("decision_rejected", json!({"node": n.id, "reason": e}));
                            feedback = Some(e);
                        }
                        Err(e) => return Ended::Failed(format!("the coordinator gave no valid decision: {e}")),
                    }
                };

                let mut done = done;
                if let Some((opt, instr, parts)) = &decision {
                    done.summary = format!("Decided that {} goes next.{}", opt.label, if instr.trim().is_empty() { String::new() } else { format!(" Instructions: {instr}") });
                    if !parts.is_empty() {
                        done.summary += &format!(" Parts: {}", parts.join(" | "));
                    }
                    host.log("decision", json!({"node": n.id, "next": opt.id, "label": opt.label, "instructions": instr, "parts": parts}));
                }
                step_end(&*host, &done);

                if done.status == "needs_input" {
                    // Ask the user; the same step runs again with the answer (its visit is not counted).
                    state.pending = Some(Pending { node: n.id.clone(), kind: "questions".into(), prompt: done.summary.clone(), questions: done.questions.clone() });
                    state.history.push(done);
                    continue;
                }

                *state.visits.entry(n.id.clone()).or_insert(0) += 1;
                state.carry = None;
                state.parts.clear();
                let status = done.status.clone();
                let reason = done.summary.clone();
                state.history.push(done);

                if let Some((opt, instr, parts)) = decision {
                    state.current = Some(opt.id);
                    state.carry = Some(instr).filter(|i| !i.trim().is_empty());
                    state.parts = parts;
                } else if status == "failed" {
                    // Only a coordinator downstream may deal with a failure.
                    let next = fail!(single_next(wf, n));
                    if wf.coordinator.as_deref() != Some(next.as_str()) {
                        return Ended::Failed(format!("{} failed: {reason}", label_of(n)));
                    }
                    state.current = Some(next);
                } else if is_coord {
                    return Ended::Failed("the coordinator ended without a decision".into());
                } else {
                    state.current = Some(fail!(single_next(wf, n)));
                }
            }
        }
        host.checkpoint(state, "running");
    }
}

// ---- the real host ----

/// What a running session is, for the commands.
struct Live {
    cancel: watch::Sender<bool>,
    reply: mpsc::Sender<Reply>,
}

#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, Live>>);

fn key(project: &str, sid: &str) -> String {
    format!("{project}/{sid}")
}

struct RealHost {
    app: AppHandle,
    root: PathBuf,
    project: String,
    sid: String,
    project_prompt: String,
    profiles: HashMap<String, Profile>,
    cancel: watch::Receiver<bool>,
    log_file: Mutex<Option<std::fs::File>>,
}

impl RealHost {
    fn on_msg(&self, req: (&str, u32, u32), m: &Outbound) {
        let (node, instance, instances) = req;
        // The partial-message stream is far too chatty to keep; whole messages are enough.
        if let Outbound::Event(e) = m
            && e.event.get("type").and_then(Value::as_str) != Some("stream_event")
        {
            self.log("agent", json!({"node": node, "instance": instance, "instances": instances, "seq": e.seq, "event": e.event}));
        }
    }
}

impl Host for RealHost {
    async fn run_agent(&self, req: AgentReq) -> Final {
        let Some(profile) = self.profiles.get(&req.profile) else {
            return failed(format!("the profile {} does not exist", req.profile));
        };
        let run_id = crate::runs::new_run_id();
        let identity = crate::runs::identity(&[("project", &self.project), ("session", &self.sid), ("step", &req.node)], &run_id);
        let task = Task {
            prompt: req.prompt,
            model: profile.model.clone(),
            effort: profile.effort.clone(),
            session_id: crate::runs::uuid_v4(),
            resume: false,
            schema: if req.schema.trim().is_empty() { crate::protocol::RESULT_SCHEMA.into() } else { req.schema },
            system_prompt: crate::projects::combine_system_prompts(&self.project_prompt, &profile.system_prompt),
            env: profile.env.iter().map(|v| TaskEnv { name: v.name.clone(), value: v.value.clone() }).collect(),
            timeout_secs: u64::from(req.timeout_mins) * 60,
        };
        let spec = Spec { run_id, image: profile.image.clone(), identity, cpus: profile.cpus, memory_mb: profile.memory_mb, task };
        // Cancelling the session cancels this VM.
        let (tx, rx) = oneshot::channel();
        let mut c = self.cancel.clone();
        let forward = tokio::spawn(async move {
            if c.wait_for(|v| *v).await.is_ok() {
                let _ = tx.send(());
            }
        });
        let res = crate::runs::run_vm(&self.app, spec, rx, |m| self.on_msg((&req.node, req.instance, req.instances), m)).await;
        forward.abort();
        res.unwrap_or_else(failed)
    }

    fn log(&self, kind: &str, body: Value) {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let entry = json!({"t": t, "kind": kind, "body": body});
        if let Some(f) = self.log_file.lock().unwrap().as_mut() {
            let _ = writeln!(f, "{entry}");
        }
        let _ = self.app.emit("session-log", json!({"project": self.project, "session": self.sid, "entry": entry}));
    }

    fn checkpoint(&self, state: &State, status: &str) {
        if let Ok(path) = crate::projects::session_file(&self.root, &self.project, &self.sid, "state.json")
            && let Ok(text) = serde_json::to_string(state)
        {
            let _ = std::fs::write(path, text);
        }
        let _ = crate::projects::set_session_status(&self.root, &self.project, &self.sid, status);
        self.log("status", json!({"status": status, "cost_usd": state.cost_usd}));
    }
}

fn read_state(root: &std::path::Path, project: &str, sid: &str) -> Option<State> {
    serde_json::from_str(&std::fs::read_to_string(crate::projects::session_file(root, project, sid, "state.json").ok()?).ok()?).ok()
}

/// Everything a run needs, checked before anything starts.
fn prepare(app: &AppHandle, project: &str) -> Result<(Workflow, crate::projects::Project, HashMap<String, Profile>), String> {
    let root = crate::projects::root(app)?;
    let wf = crate::projects::workflow(&root, project)?;
    wf.validate()?;
    let profiles_dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("profiles");
    let profiles: HashMap<String, Profile> = crate::profiles::list(&profiles_dir).into_iter().map(|e| (e.id, e.profile)).collect();
    let ids: Vec<String> = profiles.keys().cloned().collect();
    let problems = wf.problems(&ids);
    if !problems.is_empty() {
        let codes: Vec<String> = problems.iter().map(|p| format!("{} ({})", p.code, p.node.as_deref().unwrap_or("-"))).collect();
        return Err(format!("the workflow has problems: {}", codes.join(", ")));
    }
    for n in &wf.nodes {
        if let Step::Agent { profile, .. } = &n.step {
            let image = &profiles[profile].image;
            if crate::images::path(app, image).is_none() {
                return Err(format!("the image {image} (profile {profile}) is not built yet"));
            }
        }
    }
    if crate::secrets::claude_token().is_none() {
        return Err("no Claude token is set (Settings > LLM)".into());
    }
    let project_info = crate::projects::list(&root).into_iter().find(|p| p.id == project).ok_or("no such project")?.project;
    Ok((wf, project_info, profiles))
}

fn launch(app: AppHandle, project: String, sid: String, fresh: bool) -> Result<(), String> {
    let root = crate::projects::root(&app)?;
    let session = crate::projects::read_session(&root, &project, &sid)?;
    let k = key(&project, &sid);
    if app.state::<Sessions>().0.lock().unwrap().contains_key(&k) {
        return Err("the session is already running".into());
    }
    match (session.status.as_str(), fresh) {
        ("new", true) | ("interrupted" | "failed" | "cancelled", false) => {}
        _ => return Err(format!("a session that is {} cannot be {}", session.status, if fresh { "started" } else { "continued" })),
    }
    let (wf, project_info, profiles) = prepare(&app, &project)?;
    let mut state = if fresh { State::default() } else { read_state(&root, &project, &sid).unwrap_or_default() };
    if state.current.is_none() {
        state.current = wf.nodes.iter().find(|n| n.step == Step::Start).map(|n| n.id.clone());
    }

    let (cancel_tx, cancel_rx) = watch::channel(false);
    let (reply_tx, mut reply_rx) = mpsc::channel(4);
    app.state::<Sessions>().0.lock().unwrap().insert(k.clone(), Live { cancel: cancel_tx, reply: reply_tx });

    let log_file = crate::projects::session_file(&root, &project, &sid, "log.jsonl")
        .ok()
        .and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
    let host = Arc::new(RealHost {
        app: app.clone(),
        root,
        project,
        sid,
        project_prompt: project_info.system_prompt,
        profiles,
        cancel: cancel_rx.clone(),
        log_file: Mutex::new(log_file),
    });
    let task = session.task;
    tauri::async_runtime::spawn(async move {
        host.log("started", json!({"continued": !fresh}));
        host.checkpoint(&state, "running");
        let ended = drive(host.clone(), &wf, &task, &mut state, cancel_rx, &mut reply_rx).await;
        let status = match &ended {
            Ended::Done => "done",
            Ended::Failed(reason) => {
                host.log("error", json!({"reason": reason}));
                "failed"
            }
            Ended::Cancelled => "cancelled",
        };
        // `checkpoint` writes the state and status; the final status is logged by it too
        host.checkpoint(&state, status);
        host.app.state::<Sessions>().0.lock().unwrap().remove(&k);
    });
    Ok(())
}

/// Marks sessions that were running or waiting when the app closed as interrupted (run at startup).
pub fn recover(app: &AppHandle) {
    let Ok(root) = crate::projects::root(app) else { return };
    for p in crate::projects::list(&root) {
        for s in crate::projects::sessions(&root, &p.id) {
            if matches!(s.session.status.as_str(), "running" | "waiting") {
                let _ = crate::projects::set_session_status(&root, &p.id, &s.id, "interrupted");
            }
        }
    }
}

#[tauri::command]
pub fn session_start(app: AppHandle, project: String, id: String, task: Option<String>) -> Result<(), String> {
    if let Some(task) = task {
        crate::projects::edit_session(&crate::projects::root(&app)?, &project, &id, None, Some(&task))?;
    }
    launch(app, project, id, true)
}

/// Continues an interrupted, failed or cancelled session from the step it stopped at.
#[tauri::command]
pub fn session_continue(app: AppHandle, project: String, id: String) -> Result<(), String> {
    launch(app, project, id, false)
}

#[tauri::command]
pub fn session_cancel(app: AppHandle, project: String, id: String) -> bool {
    app.state::<Sessions>().0.lock().unwrap().get(&key(&project, &id)).is_some_and(|l| l.cancel.send(true).is_ok())
}

#[tauri::command]
pub fn session_reply(app: AppHandle, project: String, id: String, reply: Reply) -> Result<(), String> {
    if reply.text.len() > 64 * 1024 {
        return Err("the answer is too large".into());
    }
    let sessions = app.state::<Sessions>();
    let live = sessions.0.lock().unwrap();
    let l = live.get(&key(&project, &id)).ok_or("the session is not running")?;
    l.reply.try_send(reply).map_err(|_| "the session is not waiting for an answer".to_string())
}

#[derive(Serialize)]
pub struct SessionView {
    pub session: crate::projects::Session,
    pub pending: Option<Pending>,
    pub live: bool,
    pub cost_usd: f64,
}

#[tauri::command]
pub fn session_get(app: AppHandle, project: String, id: String) -> Result<SessionView, String> {
    let root = crate::projects::root(&app)?;
    let session = crate::projects::read_session(&root, &project, &id)?;
    let state = read_state(&root, &project, &id).unwrap_or_default();
    let live = app.state::<Sessions>().0.lock().unwrap().contains_key(&key(&project, &id));
    Ok(SessionView { session, pending: state.pending, live, cost_usd: state.cost_usd })
}

/// The session's log (the last entries when it is long).
#[tauri::command]
pub fn session_log(app: AppHandle, project: String, id: String) -> Result<Vec<Value>, String> {
    let root = crate::projects::root(&app)?;
    let text = std::fs::read_to_string(crate::projects::session_file(&root, &project, &id, "log.jsonl")?).unwrap_or_default();
    let entries: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let skip = entries.len().saturating_sub(5000);
    Ok(entries.into_iter().skip(skip).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::{Edge, standard_workflow};

    /// A host whose agents are a function of the request.
    type Script = Box<dyn FnMut(&AgentReq) -> Final + Send>;

    struct Fake {
        script: Mutex<Script>,
        seen: Mutex<Vec<(String, u32, u32, String)>>,
        states: Mutex<Vec<String>>,
    }

    impl Fake {
        fn new(f: impl FnMut(&AgentReq) -> Final + Send + 'static) -> Arc<Self> {
            Arc::new(Fake { script: Mutex::new(Box::new(f)), seen: Mutex::new(vec![]), states: Mutex::new(vec![]) })
        }
        fn nodes(&self) -> Vec<String> {
            self.seen.lock().unwrap().iter().map(|s| format!("{}#{}/{}", s.0, s.1, s.2)).collect()
        }
        fn prompt(&self, i: usize) -> String {
            self.seen.lock().unwrap()[i].3.clone()
        }
    }

    impl Host for Fake {
        async fn run_agent(&self, req: AgentReq) -> Final {
            self.seen.lock().unwrap().push((req.node.clone(), req.instance, req.instances, req.prompt.clone()));
            (self.script.lock().unwrap())(&req)
        }
        fn log(&self, _: &str, _: Value) {}
        fn checkpoint(&self, _: &State, status: &str) {
            self.states.lock().unwrap().push(status.into());
        }
    }

    fn fin(status: Status, summary: &str, output: Option<Value>) -> Final {
        Final { status, result: summary.into(), questions: vec![], session_id: None, cost_usd: Some(0.5), output, exit_code: Some(0) }
    }

    fn decide(next: &str, instr: &str, parts: &[&str]) -> Final {
        fin(Status::Done, "", Some(json!({"next": next, "instructions": instr, "parts": parts})))
    }

    async fn run(host: &Arc<Fake>, wf: &Workflow, replies: Vec<Reply>) -> (Ended, State) {
        let (tx, mut rx) = mpsc::channel(16);
        for r in replies {
            tx.send(r).await.unwrap();
        }
        let (_c, crx) = watch::channel(false);
        let mut state = State { current: Some("start".into()), ..Default::default() };
        let ended = drive(host.clone(), wf, "build a thing", &mut state, crx, &mut rx).await;
        (ended, state)
    }

    #[tokio::test]
    async fn the_orchestrator_drives_the_standard_workflow() {
        let mut orch = 0;
        let host = Fake::new(move |req| match req.node.as_str() {
            "orch" => {
                orch += 1;
                match orch {
                    1 => decide("pm", "scope it", &[]),
                    2 => decide("ba", "write requirements", &[]),
                    3 => decide("review", "", &[]),
                    4 => decide("dev", "build it", &["api", "ui", "docs", "extra"]), // too many parts: rejected
                    5 => decide("dev", "build it", &["api", "ui"]),
                    6 => decide("test", "verify", &[]),
                    _ => decide("done", "", &[]),
                }
            }
            "test" => fin(Status::Done, "all good", Some(json!({"passed": true, "report": "ok"}))),
            other => fin(Status::Done, &format!("{other} finished"), None),
        });
        let reply = Reply { approved: Some(true), text: "looks fine".into() };
        let (ended, state) = run(&host, &standard_workflow(), vec![reply]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(
            host.nodes(),
            ["orch#1/1", "pm#1/1", "orch#1/1", "ba#1/1", "orch#1/1", "orch#1/1", "orch#1/1", "dev#1/2", "dev#2/2", "orch#1/1", "test#1/1", "orch#1/1"]
        );
        // the retry says why the first answer was refused
        assert!(host.prompt(6).contains("rejected") && host.prompt(6).contains("4 entries"));
        // workers get the task, the history, the coordinator's instructions and their part
        let dev = host.prompt(7);
        assert!(dev.contains("build a thing") && dev.contains("pm finished") && dev.contains("build it"));
        assert!(dev.contains("instance 1 of 2") && dev.contains("api"));
        // the user's approval is in the history the coordinator sees afterwards
        assert!(host.prompt(5).contains("The user approved it. Comment: looks fine"));
        assert_eq!(state.visits["dev"], 1);
        assert_eq!(state.visits["orch"], 6, "the refused answer is not a visit");
        assert!(state.pending.is_none() && state.parts.is_empty());
    }

    #[tokio::test]
    async fn a_decision_outside_the_graph_fails_the_session() {
        let host = Fake::new(|_| decide("nowhere", "", &[]));
        let (ended, _) = run(&host, &standard_workflow(), vec![]).await;
        assert!(matches!(ended, Ended::Failed(r) if r.contains("no valid decision") && r.contains("nowhere")));
        assert_eq!(host.nodes().len(), 2, "one retry, then it gives up");
    }

    fn linear() -> Workflow {
        let n = |id: &str, step: Step| Node { id: id.into(), label: id.into(), x: 0, y: 0, step };
        let e = |from: &str, to: &str, b: Option<&str>| Edge { id: format!("{from}{to}{}", b.unwrap_or("")), from: from.into(), to: to.into(), branch: b.map(Into::into) };
        Workflow {
            nodes: vec![
                n("start", Step::Start),
                n("a", Step::Agent { profile: "dev".into(), prompt: "do it".into(), schema: String::new(), max_runs: 2, min_instances: 1, max_instances: 1, timeout_mins: 30 }),
                n("c", Step::Condition { field: "result.passed".into(), op: Op::Eq, value: "true".into() }),
                n("end", Step::End { outcome: Outcome::Success }),
            ],
            edges: vec![e("start", "a", None), e("a", "c", None), e("c", "end", Some("true")), e("c", "a", Some("false"))],
            coordinator: None,
        }
    }

    #[tokio::test]
    async fn conditions_loop_back_until_they_pass() {
        let mut n = 0;
        let host = Fake::new(move |_| {
            n += 1;
            fin(Status::Done, "tried", Some(json!({"passed": n == 2})))
        });
        let (ended, state) = run(&host, &linear(), vec![]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(host.nodes(), ["a#1/1", "a#1/1"]);
        assert_eq!(state.cost_usd, 1.0);
        assert!(host.prompt(1).contains("Structured result: {\"passed\":false}"), "the second try sees the first result");
    }

    #[tokio::test]
    async fn each_step_passes_its_own_timeout() {
        let seen = Arc::new(Mutex::new(vec![]));
        let s2 = seen.clone();
        let host = Fake::new(move |req| {
            s2.lock().unwrap().push(req.timeout_mins);
            fin(Status::Done, "ok", Some(json!({"passed": true})))
        });
        let mut wf = linear();
        if let Step::Agent { timeout_mins, .. } = &mut wf.nodes[1].step {
            *timeout_mins = 7;
        }
        let (ended, _) = run(&host, &wf, vec![]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(*seen.lock().unwrap(), [7]);
    }

    #[tokio::test]
    async fn max_runs_stops_a_loop() {
        let host = Fake::new(|_| fin(Status::Done, "nope", Some(json!({"passed": false}))));
        let (ended, _) = run(&host, &linear(), vec![]).await;
        assert!(matches!(ended, Ended::Failed(r) if r.contains("its limit")));
        assert_eq!(host.nodes().len(), 2);
    }

    #[tokio::test]
    async fn a_failed_agent_fails_the_session_unless_the_coordinator_is_next() {
        let host = Fake::new(|_| fin(Status::Failed, "boom", None));
        let (ended, _) = run(&host, &linear(), vec![]).await;
        assert!(matches!(ended, Ended::Failed(r) if r.contains("failed: boom")));

        // in the standard workflow the orchestrator sees the failure and decides
        let mut orch = 0;
        let host = Fake::new(move |req| match req.node.as_str() {
            "orch" => {
                orch += 1;
                match orch {
                    1 => decide("pm", "", &[]),
                    _ => decide("done", "", &[]),
                }
            }
            _ => fin(Status::Failed, "pm crashed", None),
        });
        let (ended, _) = run(&host, &standard_workflow(), vec![]).await;
        assert_eq!(ended, Ended::Done);
        assert!(host.prompt(2).contains("pm crashed"));
    }

    #[tokio::test]
    async fn an_agent_that_needs_input_waits_for_the_user_and_runs_again() {
        let mut n = 0;
        let host = Fake::new(move |_| {
            n += 1;
            if n == 1 {
                let mut f = fin(Status::NeedsInput, "half done", None);
                f.questions = vec!["which database?".into()];
                f
            } else {
                fin(Status::Done, "done", Some(json!({"passed": true})))
            }
        });
        let reply = Reply { approved: None, text: "postgres".into() };
        let (ended, state) = run(&host, &linear(), vec![reply]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(host.nodes().len(), 2);
        let second = host.prompt(1);
        assert!(second.contains("which database?") && second.contains("postgres") && second.contains("half done"));
        assert_eq!(state.visits["a"], 1, "the question round is not a visit");
    }

    #[tokio::test]
    async fn waits_for_an_answer_that_is_a_decision_and_can_be_cancelled() {
        let host = Fake::new(|req| if req.node == "orch" { decide("review", "", &[]) } else { fin(Status::Done, "", None) });
        let (tx, mut rx) = mpsc::channel(4);
        let (ctx, crx) = watch::channel(false);
        let mut state = State { current: Some("start".into()), ..Default::default() };
        let wf = standard_workflow();
        let h = host.clone();
        let task = tokio::spawn(async move {
            let ended = drive(h, &wf, "t", &mut state, crx, &mut rx).await;
            (ended, state)
        });
        // an approval without yes or no is ignored; then the session is cancelled while it waits
        tx.send(Reply::default()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        ctx.send(true).unwrap();
        let (ended, state) = tokio::time::timeout(std::time::Duration::from_secs(5), task).await.unwrap().unwrap();
        assert_eq!(ended, Ended::Cancelled);
        assert_eq!(state.pending.as_ref().map(|p| p.kind.as_str()), Some("approve"));
    }

    #[test]
    fn conditions_read_status_and_nested_results() {
        let d = Done { node: "a".into(), label: "a".into(), status: "done".into(), summary: String::new(), output: Some(json!({"a": {"b": ["x", "y"]}, "n": 3, "ok": true})), questions: vec![], cost_usd: 0.0 };
        let c = |f: &str, op, v: &str| eval_condition(f, op, v, Some(&d));
        assert!(c("status", Op::Eq, "done"));
        assert!(c("result.n", Op::Eq, "3") && c("result.ok", Op::Eq, "true"));
        assert!(c("result.a.b", Op::Contains, "y"));
        assert!(c("result.missing", Op::Ne, "x") && !c("result.missing", Op::Eq, "x"));
        assert!(!eval_condition("status", Op::Eq, "done", None));
    }

    #[test]
    fn long_histories_keep_the_newest_steps() {
        let d = |i: usize| Done { node: "n".into(), label: format!("step {i}"), status: "done".into(), summary: "x".repeat(5000), output: None, questions: vec![], cost_usd: 0.0 };
        let all: Vec<Done> = (0..20).map(d).collect();
        let text = render_history(&all);
        assert!(text.contains("step 19") && !text.contains("step 0\n") && text.contains("earlier steps left out"));
        assert!(text.len() < HISTORY_CHARS + 6000);
    }

    #[test]
    fn the_coordinator_is_offered_the_graph_exits() {
        let opts = coordinator_options(&standard_workflow(), "orch");
        let ids: Vec<&str> = opts.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, ["pm", "ba", "dev", "test", "review", "done"]);
        assert_eq!(opts[2].max, 3);
        let (opt, _, parts) = parse_decision(Some(&json!({"next": "Developer", "instructions": "x", "parts": ["a"]})), &opts).unwrap();
        assert_eq!((opt.id.as_str(), parts.len()), ("dev", 1), "labels work too");
        let (_, _, parts) = parse_decision(Some(&json!({"next": "pm", "parts": ["a", "b"]})), &opts).unwrap();
        assert!(parts.is_empty(), "a step without instances ignores parts");
        assert!(parse_decision(Some(&json!("dev")), &opts).is_err());
    }
}
