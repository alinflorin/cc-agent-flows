//! The workflow data model: nodes, edges, the sessions' stored shape, and the rules a workflow must
//! follow (`Workflow::validate`, `problems`).

use serde::{Deserialize, Serialize};

const MAX_NODES: usize = 100;
const MAX_EDGES: usize = 300;

/// Instructions every agent of the workflow gets before its profile's own system prompt.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Instructions {
    #[serde(default)]
    pub system_prompt: String,
}

/// The system prompt an agent gets: the workflow's instructions, then the profile's.
pub fn combine_system_prompts(workflow: &str, profile: &str) -> Option<String> {
    let parts: Vec<&str> = [workflow.trim(), profile.trim()].into_iter().filter(|p| !p.is_empty()).collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
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
    /// The id of the workflow this session runs (fixed once it has started).
    #[serde(default = "default_workflow")]
    pub workflow: String,
    /// What the user wrote after the first message, in order. Each one is a turn of the session.
    #[serde(default)]
    pub messages: Vec<String>,
    /// The agent that gets the request, when a trigger fixed it (empty: the chat entry).
    #[serde(default)]
    pub entry: Option<String>,
    /// What started it: `chat`, `cron` or `webhook`.
    #[serde(default = "chat")]
    pub source: String,
}

pub(super) fn default_workflow() -> String {
    "default".into()
}

pub(super) fn chat() -> String {
    "chat".into()
}

pub(super) fn new_status() -> String {
    "new".into()
}

pub(super) fn ask() -> String {
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
    /// Trigger: the user's chat message starts the session, and goes to the agent this points to
    /// (the root when there is no chat node).
    Chat,
    /// Trigger: starts a session on a cron schedule with the node's request, for the agent it points to.
    Cron,
    /// Trigger: a POST to the node's URL starts a session whose request is the body as it came.
    Webhook,
}

impl NodeKind {
    pub fn is_trigger(self) -> bool {
        matches!(self, NodeKind::Chat | NodeKind::Cron | NodeKind::Webhook)
    }
}

/// Settings of the trigger nodes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TriggerCfg {
    /// Cron nodes: the schedule, e.g. `0 9 * * 1-5`.
    #[serde(default)]
    pub cron: String,
    /// Webhook nodes: the secret part of the URL. Made when the workflow is saved with it empty;
    /// never exported.
    #[serde(default)]
    pub token: String,
    /// What the sessions this node starts do about questions: `ask` or `never` (empty is `never`:
    /// nobody is watching).
    #[serde(default)]
    pub autonomy: String,
}

impl TriggerCfg {
    fn validate(&self, kind: NodeKind) -> Result<(), String> {
        if !matches!(self.autonomy.as_str(), "" | "ask" | "never") {
            return Err("a trigger's autonomy is `ask` or `never`".into());
        }
        if kind == NodeKind::Cron {
            crate::workflow::cron::Cron::parse(&self.cron).map_err(|e| format!("the schedule: {e}"))?;
        }
        if self.token.len() > 64 || !self.token.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) {
            return Err("a webhook token is up to 64 lowercase letters and digits".into());
        }
        Ok(())
    }
}

/// A node of the workflow: an agent, or the user, or a trigger.
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
    /// Trigger nodes only.
    #[serde(default)]
    pub trigger: TriggerCfg,
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
            if n.kind.is_trigger() {
                n.trigger.validate(n.kind)?;
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
                    if to.kind.is_trigger() {
                        return Err("nothing leads to a trigger".into());
                    }
                    if from.kind.is_trigger() && to.kind != NodeKind::Agent {
                        return Err("a trigger hands the request to an agent".into());
                    }
                }
                EdgeKind::Before => {}
            }
        }
        for t in self.nodes.iter().filter(|n| n.kind.is_trigger()) {
            if self.edges.iter().filter(|e| e.from == t.id).count() > 1 {
                return Err("a trigger hands the request to one agent".into());
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
                NodeKind::Ask | NodeKind::Chat | NodeKind::Cron | NodeKind::Webhook => {}
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

    /// The agent a trigger node hands its request to.
    pub fn trigger_target(&self, trigger: &str) -> Option<&Node> {
        let e = self.edges.iter().find(|e| e.from == trigger && e.kind == EdgeKind::Delegate)?;
        self.node(&e.to).filter(|n| n.kind == NodeKind::Agent)
    }

    /// The agent that gets the user's chat message: where the chat node points, else the root.
    pub fn chat_entry(&self) -> Option<&Node> {
        match self.nodes.iter().find(|n| n.kind == NodeKind::Chat) {
            Some(chat) => self.trigger_target(&chat.id),
            None => self.root(),
        }
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
        for n in self.nodes.iter().filter(|n| n.kind.is_trigger()) {
            if self.trigger_target(&n.id).is_none() {
                out.push(problem("trigger_no_target", Some(&n.id)));
            }
            if n.kind == NodeKind::Cron && n.prompt.trim().is_empty() {
                out.push(problem("cron_no_request", Some(&n.id)));
            }
        }
        for n in self.nodes.iter().filter(|n| n.kind != NodeKind::Agent && !n.kind.is_trigger()) {
            if !self.edges.iter().any(|e| e.to == n.id && e.kind == EdgeKind::Delegate) {
                out.push(problem("human_unused", Some(&n.id)));
            }
            if n.kind == NodeKind::Approve && !self.edges.iter().any(|e| e.from == n.id) {
                out.push(problem("gate_no_target", Some(&n.id)));
            }
        }
        // The agents nobody hands work to are where work enters; with several, each needs a
        // trigger of its own (and the chat goes to the one the chat node points to).
        let calls = self.call_edges();
        let roots: Vec<&str> = self.nodes.iter().filter(|n| n.kind == NodeKind::Agent && !calls.iter().any(|c| c.to == n.id)).map(|n| n.id.as_str()).collect();
        let targets: Vec<&str> = self.nodes.iter().filter(|n| n.kind.is_trigger()).filter_map(|n| self.trigger_target(&n.id)).map(|n| n.id.as_str()).collect();
        if roots.is_empty() {
            out.push(problem("no_root", None));
        } else if roots.len() > 1 && roots.iter().any(|r| !targets.contains(r)) {
            out.push(problem("many_roots", None));
        } else if self.chat_entry().is_none() {
            out.push(problem("no_chat_entry", None));
        }
        if !roots.is_empty() {
            let mut seen = std::collections::HashSet::new();
            let mut stack: Vec<&str> = roots.iter().chain(targets.iter()).copied().collect();
            while let Some(id) = stack.pop() {
                if seen.insert(id) {
                    stack.extend(calls.iter().filter(|c| c.from == id).map(|c| c.to.as_str()));
                }
            }
            for n in self.nodes.iter().filter(|n| n.kind == NodeKind::Agent && !seen.contains(n.id.as_str())) {
                out.push(problem("unreachable", Some(&n.id)));
            }
        }
        out
    }
}

/// The standard delivery workflow, as an org chart (`demo/demo.yaml`): the orchestrator takes the request to the
/// project manager, who goes through the work items one by one and has the business analyst,
/// architect, developer, reviewer and tester do each, in that order (the dashed arrows; the engine
/// holds the project manager to it). Only the root reaches the user: the others'
/// questions go up the chain (add a user node to give an agent a direct line). How often and in
/// which loops is up to the agents, as their prompts say.
pub fn standard_workflow() -> Workflow {
    crate::demo::workflow().workflow
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn profiles() -> Vec<String> {
        ["orchestrator", "project-manager", "business-analyst", "architect", "developer", "reviewer", "qa"].map(String::from).to_vec()
    }

    pub(crate) fn n(id: &str) -> Node {
        Node { id: id.into(), label: String::new(), x: 0, y: 0, kind: NodeKind::Agent, profile: "developer".into(), prompt: String::new(), max_calls: 20, timeout_mins: 30, trigger: Default::default() }
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
        assert_eq!(wf.problems(&[]).iter().filter(|p| p.code == "unknown_profile").count(), 7);
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
        wf.edges.push(e("pm", "orch"));
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
    fn trigger_nodes_hand_the_request_to_one_agent() {
        let trig = |id: &str, kind, cron: &str| Node { kind, profile: String::new(), trigger: TriggerCfg { cron: cron.into(), ..Default::default() }, ..n(id) };
        let edge = |a: &str, b: &str| Edge { id: format!("{a}-{b}"), from: a.into(), to: b.into(), kind: EdgeKind::Delegate };
        let mut wf = standard_workflow();
        wf.nodes.push(trig("cron", NodeKind::Cron, "0 9 * * 1-5"));
        wf.nodes.push(trig("hook", NodeKind::Webhook, ""));
        wf.nodes.push(trig("chat", NodeKind::Chat, ""));
        wf.edges.extend([edge("cron", "orch"), edge("hook", "pm"), edge("chat", "orch")]);
        wf.validate().unwrap();
        assert_eq!(wf.trigger_target("hook").unwrap().id, "pm");
        assert_eq!(wf.chat_entry().unwrap().id, "orch");
        assert_eq!(wf.root().unwrap().id, "orch", "triggers do not make anything a called agent");
        let ids: Vec<String> = ["orchestrator", "project-manager", "business-analyst", "architect", "developer", "reviewer", "qa"].map(String::from).into();
        let codes: Vec<&str> = wf.problems(&ids).iter().map(|p| p.code).collect();
        assert_eq!(codes, ["cron_no_request"], "the cron node has no request yet");

        // refused shapes
        let mut bad = wf.clone();
        bad.edges.push(edge("pm", "cron"));
        assert!(bad.validate().is_err(), "nothing leads to a trigger");
        let mut bad = wf.clone();
        bad.edges.push(edge("cron", "pm"));
        assert!(bad.validate().is_err(), "one target only");
        let mut bad = wf.clone();
        bad.nodes.iter_mut().find(|n| n.id == "cron").unwrap().trigger.cron = "61 * * * *".into();
        assert!(bad.validate().is_err(), "a bad schedule");
        let mut bad = wf.clone();
        bad.nodes.iter_mut().find(|n| n.id == "hook").unwrap().trigger.token = "../x".into();
        assert!(bad.validate().is_err(), "a bad token");

        // a trigger that points nowhere is a draft problem; two entry points need a trigger each
        let mut draft = wf.clone();
        draft.edges.retain(|e| e.from != "hook");
        assert!(draft.problems(&ids).iter().any(|p| p.code == "trigger_no_target" && p.node.as_deref() == Some("hook")));
        let mut two = chart_two_roots();
        assert!(two.problems(&["developer".to_string()]).iter().any(|p| p.code == "many_roots"));
        two.nodes.push(trig("hook", NodeKind::Webhook, ""));
        two.nodes.push(trig("hook2", NodeKind::Webhook, ""));
        two.edges.extend([edge("hook", "a"), edge("hook2", "b")]);
        two.validate().unwrap();
        assert!(!two.problems(&["developer".to_string()]).iter().any(|p| p.code == "many_roots"), "each root has its trigger");
    }

    fn chart_two_roots() -> Workflow {
        Workflow { nodes: vec![n("a"), n("b")], edges: vec![] }
    }

    #[test]
    fn project_prompt_comes_before_the_profile_prompt() {
        assert_eq!(combine_system_prompts("P", "Q").as_deref(), Some("P\n\nQ"));
        assert_eq!(combine_system_prompts(" ", "Q").as_deref(), Some("Q"));
        assert_eq!(combine_system_prompts("", " "), None);
    }
}
