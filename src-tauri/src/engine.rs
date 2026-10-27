//! The workflow engine: runs a session over the project's org chart (`projects.rs`).
//!
//! The root agent gets the user's request. An agent with outgoing edges is a *coordinator*: each
//! time it runs it answers with a structured decision, to `delegate` work to agents it has an
//! edge to (several at once run in parallel), to `finish` (its result goes to whoever called it),
//! or to `ask` (questions it cannot answer: they go to its caller, and from the root to the user).
//! An agent without outgoing edges does its work and reports once. A caller is run again, in a
//! fresh VM, when the agents it called report back: no Claude session is ever resumed, so each
//! prompt is rebuilt from the session goal, what the agent was asked, its own `notes` (progress it
//! keeps for itself, only inside this session) and the results it is handed. The edges are the
//! only allowed moves; the engine rejects a decision that leaves them, and the limits (calls per
//! agent, depth, rounds, runs per session, parallel VMs) hold whatever the agents decide.
//!
//! The user takes part through the root's questions (unless the session's autonomy is `never`)
//! and through edges marked as needing approval.
//!
//! `drive` holds the logic and talks to the outside only through `Host` (running an agent, logging,
//! saving), so tests use a scripted host. `RealHost` boots VMs. Resuming a session after a restart
//! replays it: every agent run is journaled under a key made of the path of the call, and `drive`
//! walks the same decisions again, taking the journaled result instead of starting a VM.

use std::collections::HashMap;
use std::future::Future;
use std::io::Write as _;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{Semaphore, mpsc, oneshot, watch};

use crate::profiles::Profile;
use crate::projects::{Node, Workflow};
use crate::protocol::{Final, Outbound, Status, Task, TaskEnv};
use crate::runs::{Spec, failed};

/// Agent runs (VMs) one session may start, whatever the per-agent limits say.
const MAX_RUNS: u32 = 300;
/// How deep delegation may go (the root is 0).
const MAX_DEPTH: usize = 8;
/// Turns one coordinator call may take.
const MAX_ROUNDS: u32 = 60;
/// Agents one decision may call at once.
const MAX_PARALLEL_CALLS: usize = 5;
/// VMs running at the same time.
const MAX_VMS: usize = 4;
/// Questions that may reach the user in one run of the session; later ones are refused.
const MAX_USER_QUESTIONS: u32 = 10;
/// Attempts a coordinator gets to give a decision the graph allows.
const DECISION_ATTEMPTS: u32 = 2;
/// How much text the prompts carry.
const LOG_CHARS: usize = 8_000;
const ITEM_CHARS: usize = 6_000;

/// What a coordinator answers: where `calls` go, or `finish` with a `result`, or `ask`.
pub const COORDINATOR_SCHEMA: &str = r#"{"type":"object","properties":{"action":{"type":"string","enum":["delegate","finish","ask"]},"calls":{"type":"array","items":{"type":"object","properties":{"to":{"type":"string"},"instructions":{"type":"string"}},"required":["to","instructions"]}},"result":{"type":"string"},"success":{"type":"boolean"},"questions":{"type":"array","items":{"type":"string"}},"notes":{"type":"string"},"assumptions":{"type":"array","items":{"type":"string"}}},"required":["action","notes"]}"#;

// ---- state ----

/// What a session is waiting for the user to answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pending {
    /// Key of the answer in `State::replies`.
    pub id: String,
    pub node: String,
    pub label: String,
    /// `approve | questions`
    pub kind: String,
    pub prompt: String,
    #[serde(default)]
    pub questions: Vec<String>,
    /// The user's node of the workflow that this goes through, if any.
    #[serde(default)]
    pub human: Option<String>,
}

/// A finished agent run, in the order they finished.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Journaled {
    pub seq: u64,
    pub done: Final,
}

/// Saved at every transition (`sessions/<sid>.state.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    /// Finished agent runs by key (the path of the call, the round and the attempt).
    #[serde(default)]
    pub journal: HashMap<String, Journaled>,
    #[serde(default)]
    pub seq: u64,
    /// The user's answers by the key of the question.
    #[serde(default)]
    pub replies: HashMap<String, Reply>,
    #[serde(default)]
    pub pending: Option<Pending>,
    #[serde(default)]
    pub runs: u32,
    #[serde(default)]
    pub cost_usd: f64,
    /// The root's final report.
    #[serde(default)]
    pub result: Option<String>,
}

impl State {
    /// Before a session is continued: forgets the first failed run and everything decided after
    /// it, so that run is tried again (what came before is replayed).
    pub fn rewind_failures(&mut self) {
        if let Some(first) = self.journal.values().filter(|j| j.done.status == Status::Failed).map(|j| j.seq).min() {
            self.journal.retain(|_, j| j.seq < first);
        }
        self.pending = None;
        self.result = None;
    }
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
    /// The key of this run in the journal.
    pub call: String,
    pub depth: usize,
    pub profile: String,
    pub prompt: String,
    /// JSON schema of the answer.
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
    /// Whether the agents may reach the user: `ask` or `never` (read each time, the user may change it).
    fn autonomy(&self) -> String;
}

/// A result as the agent that called sees it.
#[derive(Debug, Clone)]
pub struct Reported {
    pub label: String,
    /// `done | failed | needs_input | rejected | user`
    pub status: String,
    pub summary: String,
    pub questions: Vec<String>,
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

fn section(title: &str, body: &str) -> String {
    format!("# {title}\n{}\n\n", body.trim())
}

fn label_of(n: &Node) -> String {
    if n.label.trim().is_empty() { n.profile.clone() } else { n.label.clone() }
}

/// A way out of a coordinator: an agent it may call.
#[derive(Debug, Clone, PartialEq)]
pub struct Opt {
    pub id: String,
    pub label: String,
    pub desc: String,
    /// The approval gate (a node of the workflow) the handover goes through.
    pub gate: Option<String>,
    /// Agents that must have finished, in the same call, before this one may be called.
    pub needs: Vec<String>,
}

pub fn options(wf: &Workflow, from: &str) -> Vec<Opt> {
    let mut out: Vec<Opt> = vec![];
    for e in wf.call_edges().into_iter().filter(|e| e.from == from) {
        let Some(n) = wf.nodes.iter().find(|n| n.id == e.to) else { continue };
        if out.iter().any(|o| o.id == n.id) {
            continue;
        }
        let role = if n.prompt.trim().is_empty() { String::new() } else { format!(": {}", clip(n.prompt.trim(), 240)) };
        out.push(Opt { id: n.id.clone(), label: label_of(n), desc: format!("{} agent{role}", n.profile), gate: e.gate, needs: vec![] });
    }
    let ids: Vec<String> = out.iter().map(|o| o.id.clone()).collect();
    for o in &mut out {
        o.needs = wf.must_precede(&o.id).into_iter().filter(|a| ids.contains(a)).collect();
    }
    out
}

fn render_reported(items: &[Reported]) -> String {
    items
        .iter()
        .map(|r| {
            let mut s = format!("### {} ({})\n{}", r.label, r.status, clip(&r.summary, ITEM_CHARS));
            if !r.questions.is_empty() {
                s += &format!("\nQuestions:\n{}", r.questions.iter().map(|q| format!("- {q}")).collect::<Vec<_>>().join("\n"));
            }
            s
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

const WORKER_ANSWER: &str = "Do the work, then end with: `status` (`done`; `failed` if you could not do it; `needs_input` only when you cannot continue at all without an answer, and then put the blocking questions in `questions`), `summary` (what you did and found, with every identifier the next agent needs: ticket ids, branches, pull request numbers, links) and `assumptions` (what you assumed instead of asking). Do not ask about what you can reasonably assume or find out yourself: assume it and say so in `assumptions`.";

pub fn worker_prompt(task: &str, n: &Node, asked: &str) -> String {
    let mut p = section("Goal of the session", &clip(task, 4_000));
    if !n.prompt.trim().is_empty() {
        p += &section("Your role in this workflow", &n.prompt);
    }
    p += &section("What is asked of you", asked);
    p += &section("How to answer", WORKER_ANSWER);
    p.trim_end().to_string()
}

pub struct TurnContext<'a> {
    pub task: &'a str,
    /// What the caller asked (`None` for the root: the goal of the session is the request).
    pub asked: Option<&'a str>,
    pub notes: &'a str,
    pub log: &'a [String],
    pub inbox: &'a [Reported],
    pub options: &'a [Opt],
    pub is_root: bool,
    /// The agent may put its questions to the user directly (the root, or one linked to the user's node).
    pub may_ask: bool,
    pub can_ask_user: bool,
    pub feedback: Option<&'a str>,
}

pub fn coordinator_prompt(n: &Node, c: &TurnContext) -> String {
    let mut p = section(if c.is_root { "The user's request" } else { "Goal of the session" }, &clip(c.task, 4_000));
    if !n.prompt.trim().is_empty() {
        p += &section("Your role in this workflow", &n.prompt);
    }
    if let Some(a) = c.asked {
        p += &section("What was asked of you", a);
    }
    if !c.notes.trim().is_empty() {
        p += &section("Your notes (written by you on your last turn)", c.notes);
    }
    if !c.log.is_empty() {
        let mut kept: Vec<&str> = vec![];
        let mut total = 0;
        for line in c.log.iter().rev() {
            if total + line.len() > LOG_CHARS && !kept.is_empty() {
                kept.push("(earlier calls left out)");
                break;
            }
            total += line.len();
            kept.push(line);
        }
        kept.reverse();
        p += &section("What you have handed out so far", &kept.join("\n"));
    }
    if !c.inbox.is_empty() {
        p += &section("Results since your last turn", &render_reported(c.inbox));
    }
    let name = |id: &String| c.options.iter().find(|o| &o.id == id).map_or(id.clone(), |o| o.label.clone());
    let list: Vec<String> = c
        .options
        .iter()
        .map(|o| {
            let mut line = format!("- `{}`: {} ({})", o.id, o.label, o.desc);
            if o.gate.is_some() {
                line += " [the user approves each handover]";
            }
            if !o.needs.is_empty() {
                line += &format!(" [only after {} finished]", o.needs.iter().map(&name).collect::<Vec<_>>().join(" and "));
            }
            line
        })
        .collect();
    let up = if c.may_ask {
        if c.can_ask_user { "the user" } else { "nobody: the user cannot be asked in this session, decide yourself" }
    } else {
        "whoever called you"
    };
    p += &section(
        "Decide what happens now",
        &format!(
            "You can call these agents, and only these:\n{}\n\nAnswer with `action`:\n- `delegate`: `calls` is a list of `{{to, instructions}}`, up to {MAX_PARALLEL_CALLS}, `to` one of the ids above. Calls in the same answer run at the same time; each agent knows only what you write in its `instructions`, so make them complete. You are run again when they report back.\n- `finish`: you are done. `result` is your report to {} and `success` says whether the goal was met.\n- `ask`: `questions` you cannot answer and cannot go on without; they go to {up}. Use it rarely: prefer to assume and list what you assumed.\n\nAlways fill `notes` with the progress you will need on your next turn (what is done, what remains): you remember nothing else. Put what you assumed in `assumptions`.",
            list.join("\n"),
            if c.is_root { "the user" } else { "whoever called you" },
        ),
    );
    if let Some(f) = c.feedback {
        p += &section("Your previous answer was refused", f);
    }
    p.trim_end().to_string()
}

// ---- decisions ----

#[derive(Debug, PartialEq)]
pub enum Action {
    Delegate(Vec<(Opt, String)>),
    Finish { result: String, success: bool },
    Ask(Vec<String>),
}

#[derive(Debug, PartialEq)]
pub struct Decision {
    pub action: Action,
    pub notes: String,
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()).unwrap_or_default()
}

pub fn assumptions_of(output: Option<&Value>) -> Vec<String> {
    strings(output.and_then(|o| o.get("assumptions")))
}

/// Reads a coordinator's answer and checks it against the agents it may call.
pub fn parse_decision(output: Option<&Value>, options: &[Opt], finished: &std::collections::HashSet<String>) -> Result<Decision, String> {
    let Some(o) = output.and_then(Value::as_object) else {
        return Err("the answer was not the structured object with `action` and `notes`".into());
    };
    let notes = o.get("notes").and_then(Value::as_str).unwrap_or("").to_string();
    let action = match o.get("action").and_then(Value::as_str).unwrap_or("") {
        "delegate" => {
            let calls = o.get("calls").and_then(Value::as_array).cloned().unwrap_or_default();
            if calls.is_empty() || calls.len() > MAX_PARALLEL_CALLS {
                return Err(format!("`calls` needs between 1 and {MAX_PARALLEL_CALLS} entries"));
            }
            let mut out = vec![];
            for c in &calls {
                let to = c.get("to").and_then(Value::as_str).unwrap_or("").trim();
                let Some(opt) = options.iter().find(|x| x.id == to).or_else(|| options.iter().find(|x| x.label.eq_ignore_ascii_case(to))) else {
                    let ids: Vec<&str> = options.iter().map(|x| x.id.as_str()).collect();
                    return Err(format!("`to` was {to:?}, which you may not call; the options are: {}", ids.join(", ")));
                };
                if let Some(missing) = opt.needs.iter().find(|a| !finished.contains(*a)) {
                    let name = options.iter().find(|x| &x.id == missing).map_or(missing.as_str(), |x| x.label.as_str());
                    return Err(format!("`{}` cannot be called yet: {name} must have finished first (in an earlier answer of yours)", opt.id));
                }
                let instructions = c.get("instructions").and_then(Value::as_str).unwrap_or("").trim().to_string();
                if instructions.is_empty() {
                    return Err(format!("the call to {} has no `instructions`", opt.id));
                }
                out.push((opt.clone(), instructions));
            }
            Action::Delegate(out)
        }
        "finish" => {
            let result = o.get("result").and_then(Value::as_str).unwrap_or("").trim().to_string();
            if result.is_empty() {
                return Err("`finish` needs a `result`".into());
            }
            Action::Finish { result, success: o.get("success").and_then(Value::as_bool).unwrap_or(true) }
        }
        "ask" => {
            let questions = strings(o.get("questions"));
            if questions.is_empty() {
                return Err("`ask` needs `questions`".into());
            }
            Action::Ask(questions)
        }
        other => return Err(format!("`action` was {other:?}; it must be delegate, finish or ask")),
    };
    Ok(Decision { action, notes })
}

// ---- running ----

enum Stop {
    Cancelled,
    Fail(String),
}

type Res<T> = Result<T, Stop>;

struct Ctx<H: Host> {
    host: Arc<H>,
    wf: Workflow,
    task: String,
    cancel: watch::Receiver<bool>,
    state: Mutex<State>,
    visits: Mutex<HashMap<String, u32>>,
    /// Holding it is having the user's attention: one question at a time.
    human: tokio::sync::Mutex<mpsc::Receiver<Reply>>,
    vms: Semaphore,
    asked_user: AtomicU32,
}

fn reported(n: &Node, f: &Final) -> Reported {
    let status = match f.status {
        Status::Done => "done",
        Status::NeedsInput => "needs_input",
        Status::Failed => "failed",
    };
    Reported { label: label_of(n), status: status.into(), summary: f.result.clone(), questions: f.questions.clone() }
}

fn failure(n: &Node, why: impl Into<String>) -> Reported {
    Reported { label: label_of(n), status: "failed".into(), summary: why.into(), questions: vec![] }
}

impl<H: Host> Ctx<H> {
    fn cancelled(&self) -> bool {
        *self.cancel.borrow()
    }

    fn save(&self) {
        let (snap, status) = {
            let s = self.state.lock().unwrap();
            (s.clone(), if s.pending.is_some() { "waiting" } else { "running" })
        };
        self.host.checkpoint(&snap, status);
    }

    fn replayed(&self, key: &str) -> bool {
        self.state.lock().unwrap().journal.contains_key(key)
    }

    /// Runs an agent, or takes its result from the journal. The flag says it came from there.
    async fn run(&self, req: AgentReq) -> Res<(Final, bool)> {
        if self.cancelled() {
            return Err(Stop::Cancelled);
        }
        let journaled = self.state.lock().unwrap().journal.get(&req.call).map(|j| j.done.clone());
        if let Some(f) = journaled {
            return Ok((f, true));
        }
        {
            let mut s = self.state.lock().unwrap();
            if s.runs >= MAX_RUNS {
                return Err(Stop::Fail(format!("the session reached its limit of {MAX_RUNS} agent runs")));
            }
            s.runs += 1;
        }
        let key = req.call.clone();
        let permit = self.vms.acquire().await.map_err(|_| Stop::Cancelled)?;
        if self.cancelled() {
            return Err(Stop::Cancelled);
        }
        self.host.log("run_start", json!({"call": req.call, "node": req.node, "depth": req.depth, "profile": req.profile}));
        let (node, call_key) = (req.node.clone(), req.call.clone());
        let f = self.host.run_agent(req).await;
        drop(permit);
        if self.cancelled() {
            return Err(Stop::Cancelled);
        }
        self.host.log(
            "run_end",
            json!({"call": call_key, "node": node, "status": f.status, "cost_usd": f.cost_usd, "summary": clip(&f.result, 4000), "questions": f.questions, "assumptions": assumptions_of(f.output.as_ref())}),
        );
        {
            let mut s = self.state.lock().unwrap();
            s.cost_usd += f.cost_usd.unwrap_or(0.0);
            s.seq += 1;
            let seq = s.seq;
            s.journal.insert(key, Journaled { seq, done: f.clone() });
        }
        self.save();
        Ok((f, false))
    }

    /// Puts something in front of the user and waits for the answer. `gate`: an approval, which is
    /// asked whatever the autonomy; otherwise a question, refused (`None`) when the session is on
    /// `never` or too many have been asked. An answer given before (journaled) is reused.
    async fn ask_user(&self, pending: Pending, gate: bool) -> Res<Option<Reply>> {
        if !gate {
            self.asked_user.fetch_add(1, Ordering::SeqCst);
        }
        let known = self.state.lock().unwrap().replies.get(&pending.id).cloned();
        if let Some(r) = known {
            return Ok(Some(r));
        }
        if !gate && (self.host.autonomy() == "never" || self.asked_user.load(Ordering::SeqCst) > MAX_USER_QUESTIONS) {
            return Ok(None);
        }
        let mut rx = self.human.lock().await;
        let id = pending.id.clone();
        self.host.log("wait", json!({"id": id, "node": pending.node, "label": pending.label, "kind": pending.kind, "prompt": pending.prompt, "questions": pending.questions, "human": pending.human}));
        self.state.lock().unwrap().pending = Some(pending);
        self.save();
        let mut cancel = self.cancel.clone();
        let reply = loop {
            let r = tokio::select! {
                r = rx.recv() => r,
                _ = cancel.wait_for(|v| *v) => None,
            };
            match r {
                None => return Err(Stop::Cancelled),
                Some(r) if gate && r.approved.is_none() => continue, // an approval needs yes or no
                Some(r) => break r,
            }
        };
        while rx.try_recv().is_ok() {} // a second click must not answer the next question
        self.host.log("reply", json!({"id": id, "approved": reply.approved, "text": reply.text}));
        {
            let mut s = self.state.lock().unwrap();
            s.pending = None;
            s.replies.insert(id, reply.clone());
        }
        self.save();
        Ok(Some(reply))
    }
}

fn find<'a>(wf: &'a Workflow, id: &str) -> Res<&'a Node> {
    wf.nodes.iter().find(|n| n.id == id).ok_or_else(|| Stop::Fail(format!("the agent {id} does not exist (the workflow changed?)")))
}

type Call = Pin<Box<dyn Future<Output = Res<Reported>> + Send>>;

/// One call of an agent, to its end. `path` identifies the call in the journal; `parent` is the
/// agent that made it.
fn call<H: Host>(ctx: Arc<Ctx<H>>, node_id: String, path: String, asked: String, depth: usize, parent: Option<String>) -> Call {
    Box::pin(async move {
        let n = find(&ctx.wf, &node_id)?.clone();
        if depth > MAX_DEPTH {
            return Ok(failure(&n, format!("calls are nested more than {MAX_DEPTH} deep")));
        }
        {
            let mut v = ctx.visits.lock().unwrap();
            let c = v.entry(n.id.clone()).or_insert(0);
            if *c >= n.max_calls {
                return Ok(failure(&n, format!("{} was already called {c} times, its limit", label_of(&n))));
            }
            *c += 1;
        }
        let opts = options(&ctx.wf, &n.id);
        // A call that was already replayed from the journal says nothing again.
        let first_run = if opts.is_empty() { path.clone() } else { format!("{path}:1:1") };
        let started = !ctx.replayed(&first_run);
        let seq = ctx.state.lock().unwrap().seq;
        if started {
            ctx.host.log("call_start", json!({"call": path, "node": n.id, "label": label_of(&n), "depth": depth, "parent": parent, "asked": clip(&asked, 2000)}));
        }
        let res = if opts.is_empty() { worker(ctx.clone(), n.clone(), path.clone(), asked, depth).await } else { coordinator(ctx.clone(), n.clone(), opts, path.clone(), asked, depth).await };
        if let Ok(r) = &res
            && (started || ctx.state.lock().unwrap().seq != seq)
        {
            ctx.host.log("call_end", json!({"call": path, "node": n.id, "status": r.status, "summary": clip(&r.summary, 4000), "questions": r.questions}));
        }
        res
    })
}

fn request(n: &Node, call: &str, depth: usize, prompt: String, schema: &str) -> AgentReq {
    AgentReq { node: n.id.clone(), call: call.into(), depth, profile: n.profile.clone(), prompt, schema: schema.into(), timeout_mins: n.timeout_mins }
}

async fn worker<H: Host>(ctx: Arc<Ctx<H>>, n: Node, path: String, asked: String, depth: usize) -> Res<Reported> {
    let mut asked = if depth == 0 { ctx.task.clone() } else { asked };
    let mut round = 0;
    loop {
        let key = if round == 0 { path.clone() } else { format!("{path}~{round}") };
        let (f, _) = ctx.run(request(&n, &key, depth, worker_prompt(&ctx.task, &n, &asked), crate::protocol::RESULT_SCHEMA)).await?;
        let r = reported(&n, &f);
        // The root, or an agent linked to the user's node, puts its blocking questions to the user.
        if (depth == 0 || ctx.wf.ask_node(&n.id).is_some()) && r.status == "needs_input" && round < MAX_ROUNDS {
            let human = ctx.wf.ask_node(&n.id).map(|h| h.id.clone());
            let pending = Pending { id: format!("{key}:ask"), node: n.id.clone(), label: label_of(&n), kind: "questions".into(), prompt: clip(&r.summary, 2000), questions: r.questions.clone(), human };
            asked = match ctx.ask_user(pending, false).await? {
                Some(reply) => format!("{asked}\n\nYou stopped to ask:\n{}\n\nThe user answered:\n{}\n\nContinue with this answer.", r.questions.join("\n"), clip(reply.text.trim(), 20_000)),
                None => format!("{asked}\n\nThe user cannot be asked in this session: decide yourself and list your assumptions."),
            };
            round += 1;
            continue;
        }
        return Ok(r);
    }
}

async fn coordinator<H: Host>(ctx: Arc<Ctx<H>>, n: Node, opts: Vec<Opt>, path: String, asked: String, depth: usize) -> Res<Reported> {
    let is_root = depth == 0;
    let may_ask = is_root || ctx.wf.ask_node(&n.id).is_some();
    let mut finished: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut notes = String::new();
    let mut log: Vec<String> = vec![];
    let mut inbox: Vec<Reported> = vec![];
    let mut feedback: Option<String> = None;
    let mut round = 0;
    loop {
        round += 1;
        if round > MAX_ROUNDS {
            return Ok(failure(&n, format!("{} took more than {MAX_ROUNDS} turns", label_of(&n))));
        }
        let can_ask_user = may_ask && ctx.host.autonomy() != "never";
        let mut attempt = 0;
        let (decision, replayed) = loop {
            attempt += 1;
            let key = format!("{path}:{round}:{attempt}");
            let prompt = coordinator_prompt(
                &n,
                &TurnContext { task: &ctx.task, asked: (!is_root).then_some(asked.as_str()), notes: &notes, log: &log, inbox: &inbox, options: &opts, is_root, may_ask, can_ask_user, feedback: feedback.as_deref() },
            );
            let (f, replayed) = ctx.run(request(&n, &key, depth, prompt, COORDINATOR_SCHEMA)).await?;
            if f.status == Status::Failed {
                return Ok(reported(&n, &f));
            }
            match parse_decision(f.output.as_ref(), &opts, &finished) {
                Ok(d) => break (d, replayed),
                Err(e) if attempt < DECISION_ATTEMPTS => {
                    if !replayed {
                        ctx.host.log("decision_rejected", json!({"node": n.id, "label": label_of(&n), "call": key, "reason": e}));
                    }
                    feedback = Some(e);
                }
                Err(e) => return Ok(failure(&n, format!("{} gave no valid decision: {e}", label_of(&n)))),
            }
        };
        feedback = None;
        if !decision.notes.trim().is_empty() {
            notes = decision.notes.clone();
        }
        if !replayed {
            let (action, detail) = match &decision.action {
                Action::Delegate(calls) => ("delegate", json!({"calls": calls.iter().map(|(o, i)| json!({"to": o.id, "label": o.label, "instructions": i})).collect::<Vec<_>>()})),
                Action::Finish { result, success } => ("finish", json!({"result": result, "success": success})),
                Action::Ask(q) => ("ask", json!({"questions": q})),
            };
            ctx.host.log("decision", json!({"call": path, "node": n.id, "label": label_of(&n), "depth": depth, "round": round, "action": action, "detail": detail, "notes": notes}));
        }

        match decision.action {
            Action::Finish { result, success } => {
                return Ok(Reported { label: label_of(&n), status: if success { "done" } else { "failed" }.into(), summary: result, questions: vec![] });
            }
            Action::Ask(questions) => {
                if !may_ask {
                    return Ok(Reported { label: label_of(&n), status: "needs_input".into(), summary: format!("{} cannot go on without answers.", label_of(&n)), questions });
                }
                let pending = Pending { id: format!("{path}:{round}:ask"), node: n.id.clone(), label: label_of(&n), kind: "questions".into(), prompt: String::new(), questions: questions.clone(), human: ctx.wf.ask_node(&n.id).map(|h| h.id.clone()) };
                match ctx.ask_user(pending, false).await? {
                    Some(reply) => {
                        inbox = vec![Reported {
                                            label: "The user".into(),
                            status: "user".into(),
                            summary: format!("Your questions:\n{}\n\nThe user answered:\n{}", questions.join("\n"), clip(reply.text.trim(), 20_000)),
                            questions: vec![],
                        }];
                    }
                    None => feedback = Some("The user cannot be asked (this session does not allow it, or too many questions were asked). Decide yourself, list what you assumed in `assumptions`, and go on.".into()),
                }
            }
            Action::Delegate(calls) => {
                let mut set = tokio::task::JoinSet::new();
                for (i, (opt, instructions)) in calls.iter().cloned().enumerate() {
                    let ctx = ctx.clone();
                    let child_path = format!("{path}/{round}.{i}");
                    let from = n.clone();
                    set.spawn(async move {
                        if opt.gate.is_some() {
                            let pending = Pending {
                                id: format!("{child_path}:gate"),
                                node: opt.id.clone(),
                                label: opt.label.clone(),
                                kind: "approve".into(),
                                prompt: format!("{} wants to hand work to {}:\n\n{}", label_of(&from), opt.label, clip(&instructions, 3000)),
                                questions: vec![],
                                human: opt.gate.clone(),
                            };
                            let approved = ctx.ask_user(pending, true).await?.is_some_and(|r| r.approved == Some(true));
                            if !approved {
                                let summary = "The user did not approve this handover. Do not do it; choose another way or finish.".to_string();
                                return Ok((i, Reported { label: opt.label, status: "rejected".into(), summary, questions: vec![] }));
                            }
                        }
                        let r = call(ctx, opt.id.clone(), child_path, instructions, depth + 1, Some(from.id.clone())).await?;
                        Ok::<_, Stop>((i, r))
                    });
                }
                let mut results: Vec<Option<Reported>> = (0..calls.len()).map(|_| None).collect();
                while let Some(joined) = set.join_next().await {
                    match joined {
                        Ok(Ok((i, r))) => results[i] = Some(r),
                        Ok(Err(stop)) => return Err(stop),
                        Err(e) => return Err(Stop::Fail(format!("an agent task crashed: {e}"))),
                    }
                }
                inbox = Vec::new();
                for ((opt, instructions), r) in calls.iter().zip(results) {
                    let r = r.unwrap_or_else(|| failure(&n, "no result"));
                    if r.status == "done" {
                        finished.insert(opt.id.clone());
                    }
                    log.push(format!("- round {round}: {} ← {} → {}: {}", opt.label, clip(instructions, 160), r.status, clip(&r.summary, 200)));
                    inbox.push(r);
                }
            }
        }
    }
}

/// Runs the session until it ends. `state` is the journal of an earlier run (empty for a new one).
pub async fn drive<H: Host>(host: Arc<H>, wf: Workflow, task: String, state: State, cancel: watch::Receiver<bool>, replies: mpsc::Receiver<Reply>) -> (Ended, State) {
    let Some(root) = wf.root().map(|n| n.id.clone()) else {
        return (Ended::Failed("the workflow needs exactly one root agent: one that no other agent hands work to".into()), state);
    };
    let ctx = Arc::new(Ctx {
        host,
        wf,
        task,
        cancel,
        state: Mutex::new(state),
        visits: Mutex::new(HashMap::new()),
        human: tokio::sync::Mutex::new(replies),
        vms: Semaphore::new(MAX_VMS),
        asked_user: AtomicU32::new(0),
    });
    let res = call(ctx.clone(), root, "r".into(), String::new(), 0, None).await;
    let ended = match res {
        Ok(r) => {
            ctx.state.lock().unwrap().result = Some(r.summary.clone());
            if r.status == "done" { Ended::Done } else { Ended::Failed(r.summary) }
        }
        Err(Stop::Cancelled) => Ended::Cancelled,
        Err(Stop::Fail(e)) => Ended::Failed(e),
    };
    let state = ctx.state.lock().unwrap().clone();
    (ended, state)
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
    fn on_msg(&self, req: (&str, &str, usize), m: &Outbound) {
        let (node, call, depth) = req;
        // The partial-message stream is far too chatty to keep; whole messages are enough.
        if let Outbound::Event(e) = m
            && e.event.get("type").and_then(Value::as_str) != Some("stream_event")
        {
            self.log("agent", json!({"node": node, "call": call, "depth": depth, "seq": e.seq, "event": e.event}));
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
            schema: req.schema,
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
        let res = crate::runs::run_vm(&self.app, spec, rx, |m| self.on_msg((&req.node, &req.call, req.depth), m)).await;
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
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
        let _ = crate::projects::set_session_status(&self.root, &self.project, &self.sid, status);
        self.log("status", json!({"status": status, "cost_usd": state.cost_usd}));
    }

    fn autonomy(&self) -> String {
        crate::projects::read_session(&self.root, &self.project, &self.sid).map_or_else(|_| "ask".into(), |s| s.autonomy)
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
    for n in wf.nodes.iter().filter(|n| n.kind == crate::projects::NodeKind::Agent) {
        let image = &profiles.get(&n.profile).ok_or_else(|| format!("the profile {} does not exist", n.profile))?.image;
        if crate::images::path(app, image).is_none() {
            return Err(format!("the image {image} (profile {}) is not built yet", n.profile));
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
    state.rewind_failures();

    let (cancel_tx, cancel_rx) = watch::channel(false);
    let (reply_tx, reply_rx) = mpsc::channel(4);
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
        let (ended, state) = drive(host.clone(), wf, task, state, cancel_rx, reply_rx).await;
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

/// Starts a new session; `task` (the user's first message) is saved first.
#[tauri::command]
pub fn session_start(app: AppHandle, project: String, id: String, task: Option<String>) -> Result<(), String> {
    if let Some(task) = task {
        crate::projects::edit_session(&crate::projects::root(&app)?, &project, &id, None, Some(&task))?;
    }
    launch(app, project, id, true)
}

/// Continues an interrupted, failed or cancelled session: what finished is replayed from the
/// journal, the rest runs again.
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
    pub result: Option<String>,
}

#[tauri::command]
pub fn session_get(app: AppHandle, project: String, id: String) -> Result<SessionView, String> {
    let root = crate::projects::root(&app)?;
    let session = crate::projects::read_session(&root, &project, &id)?;
    let state = read_state(&root, &project, &id).unwrap_or_default();
    let live = app.state::<Sessions>().0.lock().unwrap().contains_key(&key(&project, &id));
    Ok(SessionView { session, pending: state.pending.filter(|_| live), live, cost_usd: state.cost_usd, result: state.result })
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
    use crate::projects::{Edge, EdgeKind, NodeKind, standard_workflow};

    /// A host whose agents are a function of the request.
    type Script = Box<dyn FnMut(&AgentReq) -> Final + Send>;

    struct Fake {
        script: Mutex<Script>,
        /// (node, call key, prompt) of every run, in the order they started.
        seen: Mutex<Vec<(String, String, String)>>,
        autonomy: Mutex<String>,
        /// Keeps the cancel channel open.
        gone: Mutex<Option<watch::Sender<bool>>>,
    }

    impl Fake {
        fn new(f: impl FnMut(&AgentReq) -> Final + Send + 'static) -> Arc<Self> {
            Arc::new(Fake { script: Mutex::new(Box::new(f)), seen: Mutex::new(vec![]), autonomy: Mutex::new("ask".into()), gone: Mutex::new(None) })
        }
        fn nodes(&self) -> Vec<String> {
            self.seen.lock().unwrap().iter().map(|s| s.0.clone()).collect()
        }
        fn prompt(&self, i: usize) -> String {
            self.seen.lock().unwrap()[i].2.clone()
        }
    }

    impl Host for Fake {
        async fn run_agent(&self, req: AgentReq) -> Final {
            self.seen.lock().unwrap().push((req.node.clone(), req.call.clone(), req.prompt.clone()));
            (self.script.lock().unwrap())(&req)
        }
        fn log(&self, _: &str, _: Value) {}
        fn checkpoint(&self, _: &State, _: &str) {}
        fn autonomy(&self) -> String {
            self.autonomy.lock().unwrap().clone()
        }
    }

    fn fin(status: Status, summary: &str, output: Option<Value>) -> Final {
        Final { status, result: summary.into(), questions: vec![], session_id: None, cost_usd: Some(0.5), output, exit_code: Some(0) }
    }

    fn done(summary: &str) -> Final {
        fin(Status::Done, summary, None)
    }

    fn delegate(calls: &[(&str, &str)], notes: &str) -> Final {
        let calls: Vec<Value> = calls.iter().map(|(to, i)| json!({"to": to, "instructions": i})).collect();
        fin(Status::Done, "", Some(json!({"action": "delegate", "calls": calls, "notes": notes})))
    }

    fn finish(result: &str) -> Final {
        fin(Status::Done, "", Some(json!({"action": "finish", "result": result, "success": true, "notes": "done"})))
    }

    fn ask(questions: &[&str]) -> Final {
        fin(Status::Done, "", Some(json!({"action": "ask", "questions": questions, "notes": "blocked"})))
    }

    async fn run_with(host: &Arc<Fake>, wf: &Workflow, state: State, replies: Vec<Reply>) -> (Ended, State) {
        let (tx, rx) = mpsc::channel(16);
        for r in replies {
            tx.send(r).await.unwrap();
        }
        let (c, crx) = watch::channel(false);
        *host.gone.lock().unwrap() = Some(c);
        drive(host.clone(), wf.clone(), "build a thing".into(), state, crx, rx).await
    }

    async fn run(host: &Arc<Fake>, wf: &Workflow, replies: Vec<Reply>) -> (Ended, State) {
        run_with(host, wf, State::default(), replies).await
    }

    /// An org chart from `(from, to)` pairs; the nodes are the names in order of appearance.
    fn chart(edges: &[(&str, &str)]) -> Workflow {
        let mut seen = std::collections::HashSet::new();
        let nodes = edges
            .iter()
            .flat_map(|(a, b)| [*a, *b])
            .filter(|i| seen.insert(*i))
            .map(|id| Node { id: id.into(), label: id.into(), x: 0, y: 0, kind: NodeKind::Agent, profile: "dev".into(), prompt: String::new(), max_calls: 20, timeout_mins: 30 })
            .collect();
        Workflow { nodes, edges: edges.iter().map(|(a, b)| Edge { id: format!("{a}-{b}"), from: (*a).into(), to: (*b).into(), kind: EdgeKind::Delegate }).collect() }
    }

    /// `wf` with the user's node `id` of `kind`, wired with delegate edges `from -> id` and `id -> to`.
    fn with_human(mut wf: Workflow, id: &str, kind: NodeKind, from: &str, to: Option<&str>) -> Workflow {
        wf.nodes.push(Node { id: id.into(), label: id.into(), x: 0, y: 0, kind, profile: String::new(), prompt: String::new(), max_calls: 20, timeout_mins: 30 });
        let edge = |a: &str, b: &str| Edge { id: format!("{a}-{b}"), from: a.into(), to: b.into(), kind: EdgeKind::Delegate };
        wf.edges.push(edge(from, id));
        if let Some(to) = to {
            wf.edges.push(edge(id, to));
        }
        wf
    }

    #[tokio::test]
    async fn a_coordinator_loops_over_items_and_the_result_goes_back_up() {
        // orch -> pm -> lead -> {ba, dev}; the pm goes through two tickets one at a time
        let wf = chart(&[("orch", "pm"), ("pm", "lead"), ("lead", "ba"), ("lead", "dev")]);
        let (mut orch, mut pm, mut lead) = (0, 0, 0);
        let host = Fake::new(move |req| match req.node.as_str() {
            "orch" => {
                orch += 1;
                if orch == 1 { delegate(&[("pm", "deliver PROJ-1 and PROJ-2")], "waiting for pm") } else { finish("both delivered") }
            }
            "pm" => {
                pm += 1;
                match pm {
                    1 => delegate(&[("lead", "PROJ-1")], "ticket 1 started"),
                    2 => delegate(&[("lead", "PROJ-2")], "ticket 1 done, ticket 2 started"),
                    _ => finish("PROJ-1 and PROJ-2 done"),
                }
            }
            "lead" => {
                lead += 1;
                match lead % 3 {
                    1 => delegate(&[("ba", "write the requirements")], "ba asked"),
                    2 => delegate(&[("dev", "implement it")], "dev asked"),
                    _ => finish("delivered"),
                }
            }
            other => done(&format!("{other} finished")),
        });
        let (ended, state) = run(&host, &wf, vec![]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(state.result.as_deref(), Some("both delivered"));
        assert_eq!(host.nodes(), ["orch", "pm", "lead", "ba", "lead", "dev", "lead", "pm", "lead", "ba", "lead", "dev", "lead", "pm", "orch"]);
        // a coordinator gets the result of what it called, its own notes and what it handed out
        let pm_second = host.prompt(7);
        assert!(pm_second.contains("delivered") && pm_second.contains("ticket 1 started") && pm_second.contains("PROJ-1"));
        // workers know the goal, their role and only what they were told
        let ba = host.prompt(3);
        assert!(ba.contains("build a thing") && ba.contains("write the requirements") && !ba.contains("PROJ-2"));
        assert_eq!(state.cost_usd, 7.5);
    }

    #[tokio::test]
    async fn calls_in_one_decision_run_in_parallel() {
        let wf = chart(&[("root", "dev")]);
        let mut n = 0;
        let host = Fake::new(move |req| match req.node.as_str() {
            "root" => {
                n += 1;
                if n == 1 { delegate(&[("dev", "api"), ("dev", "ui")], "two devs") } else { finish("ok") }
            }
            _ => done("built"),
        });
        let (ended, _) = run(&host, &wf, vec![]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(host.nodes().iter().filter(|n| *n == "dev").count(), 2);
        assert_eq!(host.prompt(3).matches("### dev (done)").count(), 2, "both results come back together");
    }

    #[tokio::test]
    async fn a_decision_outside_the_chart_is_refused_then_fails_the_call() {
        let wf = chart(&[("root", "dev")]);
        let host = Fake::new(|_| delegate(&[("qa", "test")], ""));
        let (ended, _) = run(&host, &wf, vec![]).await;
        assert!(matches!(ended, Ended::Failed(r) if r.contains("no valid decision") && r.contains("qa")));
        assert_eq!(host.nodes().len(), 2, "one retry, then it gives up");
        assert!(host.prompt(1).contains("refused") && host.prompt(1).contains("may not call"));

        // a first bad answer followed by a good one goes through
        let mut n = 0;
        let host = Fake::new(move |req| {
            n += 1;
            match (req.node.as_str(), n) {
                ("root", 1) => delegate(&[("qa", "x")], ""),
                ("root", 2) => delegate(&[("dev", "x")], "n"),
                ("root", _) => finish("ok"),
                _ => done("built"),
            }
        });
        assert_eq!(run(&host, &wf, vec![]).await.0, Ended::Done);
    }

    #[tokio::test]
    async fn a_subordinate_question_goes_to_its_caller_which_may_answer() {
        let wf = chart(&[("root", "lead"), ("lead", "ba")]);
        let (mut lead, mut ba) = (0, 0);
        let host = Fake::new(move |req| match req.node.as_str() {
            "root" => {
                if req.prompt.contains("Results since") { finish("done") } else { delegate(&[("lead", "go")], "") }
            }
            "lead" => {
                lead += 1;
                match lead {
                    1 => delegate(&[("ba", "write requirements")], ""),
                    2 => delegate(&[("ba", "write requirements; the limit is 10 items")], ""),
                    _ => finish("delivered"),
                }
            }
            _ => {
                ba += 1;
                if ba == 1 { Final { questions: vec!["What is the limit?".into()], ..fin(Status::NeedsInput, "blocked", None) } } else { done("requirements written") }
            }
        });
        let (ended, state) = run(&host, &wf, vec![]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(host.nodes(), ["root", "lead", "ba", "lead", "ba", "lead", "root"]);
        assert!(host.prompt(3).contains("What is the limit?") && host.prompt(3).contains("needs_input"));
        assert!(state.pending.is_none() && state.replies.is_empty(), "the user was not bothered");
    }

    #[tokio::test]
    async fn a_question_that_reaches_the_root_goes_to_the_user() {
        let wf = chart(&[("root", "dev")]);
        let mut n = 0;
        let host = Fake::new(move |_| {
            n += 1;
            if n == 1 { ask(&["Which database?"]) } else { finish("used postgres") }
        });
        let (ended, state) = run(&host, &wf, vec![Reply { approved: None, text: "postgres".into() }]).await;
        assert_eq!(ended, Ended::Done);
        assert!(host.prompt(1).contains("Which database?") && host.prompt(1).contains("postgres"));
        assert_eq!(state.replies.len(), 1);

        // never: nobody is asked, the root is told to assume
        let mut n = 0;
        let host = Fake::new(move |_| {
            n += 1;
            if n == 1 { ask(&["Which database?"]) } else { finish("assumed") }
        });
        *host.autonomy.lock().unwrap() = "never".into();
        let (ended, state) = run(&host, &wf, vec![]).await;
        assert_eq!(ended, Ended::Done);
        assert!(host.prompt(0).contains("decide yourself"));
        assert!(host.prompt(1).contains("cannot be asked") && state.replies.is_empty());
    }

    #[tokio::test]
    async fn a_lone_agent_that_needs_input_asks_the_user_and_runs_again() {
        let wf = Workflow { nodes: chart(&[("a", "b")]).nodes.into_iter().filter(|n| n.id == "b").collect(), edges: vec![] };
        let mut n = 0;
        let host = Fake::new(move |_| {
            n += 1;
            if n == 1 { Final { questions: vec!["Name?".into()], ..fin(Status::NeedsInput, "need a name", None) } } else { done("hello Ana") }
        });
        let (ended, state) = run(&host, &wf, vec![Reply { approved: None, text: "Ana".into() }]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(state.result.as_deref(), Some("hello Ana"));
        assert!(host.prompt(1).contains("Name?") && host.prompt(1).contains("Ana"));
    }

    #[tokio::test]
    async fn an_approval_gate_waits_for_the_user() {
        // root -> gate -> dev: the direct edge is replaced by the gate
        let wf = with_human(Workflow { nodes: chart(&[("root", "dev")]).nodes, edges: vec![] }, "gate", NodeKind::Approve, "root", Some("dev"));
        let mut n = 0;
        let host = Fake::new(move |req| match req.node.as_str() {
            "root" => {
                n += 1;
                if n == 1 { delegate(&[("dev", "merge it")], "") } else { finish("stopped") }
            }
            _ => done("merged"),
        });
        *host.autonomy.lock().unwrap() = "never".into(); // the gate asks anyway
        let (ended, _) = run(&host, &wf, vec![Reply { approved: Some(false), text: "not yet".into() }]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(host.nodes(), ["root", "root"], "the dev never ran");
        assert!(host.prompt(0).contains("approves each handover") && host.prompt(1).contains("did not approve"));

        let mut n = 0;
        let host = Fake::new(move |req| match req.node.as_str() {
            "root" => {
                n += 1;
                if n == 1 { delegate(&[("dev", "merge it")], "") } else { finish("merged") }
            }
            _ => done("merged"),
        });
        let (_, state) = run(&host, &wf, vec![Reply { approved: Some(true), text: String::new() }]).await;
        assert_eq!(host.nodes(), ["root", "dev", "root"]);
        assert_eq!(state.replies.len(), 1);
    }

    #[tokio::test]
    async fn only_agents_linked_to_the_user_ask_directly() {
        // lead -> ba -> you: the analyst asks the user itself; the architect's question goes to the lead
        let wf = with_human(chart(&[("lead", "ba"), ("lead", "arch"), ("root", "lead")]), "you", NodeKind::Ask, "ba", None);
        let (mut lead, mut ba, mut arch) = (0, 0, 0);
        let host = Fake::new(move |req| match req.node.as_str() {
            "root" => {
                if req.prompt.contains("Results since") { finish("done") } else { delegate(&[("lead", "go")], "") }
            }
            "lead" => {
                lead += 1;
                match lead {
                    1 => delegate(&[("ba", "requirements"), ("arch", "design")], ""),
                    _ => finish("delivered"),
                }
            }
            "ba" => {
                ba += 1;
                if ba == 1 { Final { questions: vec!["Who signs off?".into()], ..fin(Status::NeedsInput, "blocked", None) } } else { done("written") }
            }
            _ => {
                arch += 1;
                if arch == 1 { Final { questions: vec!["Which cloud?".into()], ..fin(Status::NeedsInput, "blocked", None) } } else { done("designed") }
            }
        });
        let (ended, state) = run(&host, &wf, vec![Reply { approved: None, text: "the CTO".into() }]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(state.replies.len(), 1, "only the analyst's question reached the user");
        let ba_again = host.seen.lock().unwrap().iter().filter(|s| s.0 == "ba").map(|s| s.2.clone()).collect::<Vec<_>>();
        assert!(ba_again[1].contains("Who signs off?") && ba_again[1].contains("the CTO"));
        let lead_second = host.seen.lock().unwrap().iter().filter(|s| s.0 == "lead").nth(1).unwrap().2.clone();
        assert!(lead_second.contains("Which cloud?"), "the architect's question went up to the lead");
        assert!(!lead_second.contains("Who signs off?"));
    }

    #[tokio::test]
    async fn order_arrows_make_a_coordinator_follow_the_order() {
        let mut wf = chart(&[("lead", "arch"), ("lead", "dev")]);
        wf.edges.push(Edge { id: "arch-dev-before".into(), from: "arch".into(), to: "dev".into(), kind: EdgeKind::Before });
        let mut n = 0;
        let host = Fake::new(move |req| match req.node.as_str() {
            "lead" => {
                n += 1;
                match n {
                    1 => delegate(&[("dev", "just build it")], "skipping the design"), // refused
                    2 => delegate(&[("arch", "design"), ("dev", "build")], ""),           // same answer: refused too
                    3 => delegate(&[("arch", "design")], ""),
                    4 => delegate(&[("dev", "build")], ""),
                    _ => finish("shipped"),
                }
            }
            other => done(&format!("{other} done")),
        });
        // the lead is the root: two refusals in a row end the call, so give it a chance on each turn
        let (ended, _) = run(&host, &wf, vec![]).await;
        assert!(matches!(&ended, Ended::Failed(r) if r.contains("no valid decision") && r.contains("arch")), "{ended:?}");
        assert!(host.prompt(1).contains("cannot be called yet") && host.prompt(1).contains("arch"));
        assert!(host.prompt(0).contains("only after arch finished"));
        assert_eq!(host.nodes(), ["lead", "lead"]);

        // following the order works, and a later turn may call dev again (the loop after a review)
        let mut n = 0;
        let host = Fake::new(move |req| match req.node.as_str() {
            "lead" => {
                n += 1;
                match n {
                    1 => delegate(&[("arch", "design")], ""),
                    2 => delegate(&[("dev", "build")], ""),
                    3 => delegate(&[("dev", "fix the review comments")], ""),
                    _ => finish("shipped"),
                }
            }
            other => done(&format!("{other} done")),
        });
        let (ended, _) = run(&host, &wf, vec![]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(host.nodes(), ["lead", "arch", "lead", "dev", "lead", "dev", "lead"]);
    }

    #[tokio::test]
    async fn limits_hold() {
        // an agent may be called only max_calls times; a coordinator that keeps going runs out of turns
        let mut wf = chart(&[("root", "dev")]);
        wf.nodes[1].max_calls = 2;
        let host = Fake::new(|req| if req.node == "root" { delegate(&[("dev", "again")], "") } else { done("x") });
        let (ended, _) = run(&host, &wf, vec![]).await;
        assert!(matches!(&ended, Ended::Failed(r) if r.contains("took more than")), "{ended:?}");
        assert_eq!(host.nodes().iter().filter(|n| *n == "dev").count(), 2);
        assert!(host.prompt(5).contains("its limit"), "the caller is told the limit was hit");

        // a failed worker is reported to its caller, which decides
        let wf = chart(&[("root", "dev")]);
        let mut n = 0;
        let host = Fake::new(move |req| match req.node.as_str() {
            "root" => {
                n += 1;
                if n == 1 { delegate(&[("dev", "x")], "") } else { fin(Status::Done, "", Some(json!({"action": "finish", "result": "gave up", "success": false, "notes": "n"}))) }
            }
            _ => fin(Status::Failed, "the VM died", None),
        });
        let (ended, _) = run(&host, &wf, vec![]).await;
        assert_eq!(ended, Ended::Failed("gave up".into()));
        assert!(host.prompt(2).contains("the VM died"));
    }

    #[tokio::test]
    async fn a_failed_session_continues_from_its_journal() {
        let wf = chart(&[("root", "dev")]);
        // the decisions depend on the round, so a second script makes the same ones
        let script = |dev_fails: bool| {
            Fake::new(move |req| match (req.node.as_str(), req.call.as_str()) {
                ("root", "r:1:1") => delegate(&[("dev", "a")], "first"),
                ("root", "r:2:1") => delegate(&[("dev", "b")], "second"),
                ("root", _) => finish("all done"),
                ("dev", call) if dev_fails && call == "r/2.0" => fin(Status::Failed, "boom", None),
                _ => done("built"),
            })
        };
        let first = script(true);
        let (ended, mut state) = run(&first, &wf, vec![]).await;
        assert_eq!(ended, Ended::Done, "the root finished anyway");
        assert_eq!(first.nodes(), ["root", "dev", "root", "dev", "root"]);

        // continuing forgets the failed run and what was decided after it
        state.rewind_failures();
        assert_eq!(state.journal.len(), 3, "root, dev a, root again");
        let again = script(false);
        let (ended, state) = run_with(&again, &wf, state, vec![]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(again.nodes(), ["dev", "root"], "only the failed run and what came after start VMs");
        assert_eq!(state.journal.len(), 5);
    }

    #[tokio::test]
    async fn cancelling_stops_the_session() {
        let wf = chart(&[("root", "dev")]);
        let host = Fake::new(|_| done("x"));
        let (_tx, rx) = mpsc::channel(1);
        let (_c, crx) = watch::channel(true);
        let (ended, _) = drive(host.clone(), wf, "t".into(), State::default(), crx, rx).await;
        assert_eq!(ended, Ended::Cancelled);
        assert!(host.nodes().is_empty());
    }

    #[test]
    fn decisions_are_checked_against_the_options() {
        let wf = standard_workflow();
        let opts = options(&wf, "lead");
        assert_eq!(opts.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), ["ba", "arch", "dev", "rev", "qa"]);
        let ok = parse_decision(Some(&json!({"action": "delegate", "calls": [{"to": "Developer", "instructions": "x"}], "notes": "n"})), &opts, &["arch".to_string()].into()).unwrap();
        assert!(matches!(&ok.action, Action::Delegate(c) if c[0].0.id == "dev"));
        // the developer comes after the architect
        let early = json!({"action": "delegate", "calls": [{"to": "dev", "instructions": "x"}], "notes": ""});
        assert!(parse_decision(Some(&early), &opts, &Default::default()).unwrap_err().contains("Architect"));
        for bad in [
            json!({"action": "delegate", "calls": [], "notes": ""}),
            json!({"action": "delegate", "calls": [{"to": "pm", "instructions": "x"}], "notes": ""}),
            json!({"action": "delegate", "calls": [{"to": "dev", "instructions": " "}], "notes": ""}),
            json!({"action": "finish", "notes": ""}),
            json!({"action": "ask", "questions": [], "notes": ""}),
            json!({"action": "dance", "notes": ""}),
        ] {
            assert!(parse_decision(Some(&bad), &opts, &Default::default()).is_err(), "{bad}");
        }
        let six: Vec<Value> = (0..6).map(|_| json!({"to": "dev", "instructions": "x"})).collect();
        assert!(parse_decision(Some(&json!({"action": "delegate", "calls": six, "notes": ""})), &opts, &Default::default()).is_err());
        assert!(parse_decision(None, &opts, &Default::default()).is_err());
        serde_json::from_str::<Value>(COORDINATOR_SCHEMA).unwrap();
    }
}
