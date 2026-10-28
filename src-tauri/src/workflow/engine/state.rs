//! What a session keeps (the journal, the pending asks) and the `Host` the engine talks to.

use super::*;

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
    pub tokens: u64,
    /// The root's final report.
    #[serde(default)]
    pub result: Option<String>,
    /// Turns that ended: the first request and then each follow-up message, one after the other.
    #[serde(default)]
    pub turns: u32,
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
    /// The agent that gets the session's request, when it is not the workflow's root.
    fn entry(&self) -> Option<String> {
        None
    }
    /// The user's `i`-th message after the first one (0-based), if it has been sent.
    fn follow_up(&self, _i: usize) -> Option<String> {
        None
    }
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

