//! The prompts an agent is given, rebuilt for every run.

use super::*;

pub(super) fn clip(s: &str, n: usize) -> String {
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

pub(super) fn label_of(n: &Node) -> String {
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

