//! The messages between the app and `agentd`, the program inside each agent VM.
//!
//! They travel over one virtio-serial port per VM (the guest sees `/dev/virtio-ports/agent`), one
//! JSON message per line, in this order: the app sends `Bootstrap`, `agentd` answers `Ready`, the
//! app sends the `Task`, then `agentd` streams `Heartbeat`, `Event` and finally one `Final`. The
//! channel belongs to one VM, so there are no tokens or identities: the app knows who is talking
//! because of the socket. Everything the agent sends is untrusted: the app validates it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Longest line either side accepts. A longer line is dropped (events) or ends the run (the app).
pub const MAX_MESSAGE: usize = 1 << 20;

/// The first message, from the app: what this VM is and the Claude setup token. It never touches a
/// disk or a command line: it goes through the channel into `agentd`, and from there only into
/// `claude`'s environment.
#[derive(Clone, Serialize, Deserialize)]
pub struct Bootstrap {
    pub id: String,
    /// The Claude setup token. It goes only into the `claude` process environment.
    pub claude_token: String,
}

/// Messages from the app to the agent.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum Inbound {
    Task(Task),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Task {
    pub prompt: String,
    #[serde(default)]
    pub model: Option<String>,
    /// `low | medium | high | xhigh | max`.
    #[serde(default)]
    pub effort: Option<String>,
    /// The Claude session id: new with `--session-id`, or resumed with `--resume` when `resume`.
    pub session_id: String,
    #[serde(default)]
    pub resume: bool,
    /// A git bundle (base64) to start from; an empty workspace if absent.
    #[serde(default)]
    pub bundle: Option<String>,
    /// JSON schema of the final answer; `RESULT_SCHEMA` if absent.
    #[serde(default)]
    pub schema: Option<String>,
    /// Hard limit for the `claude` run.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_timeout() -> u64 {
    30 * 60
}

/// Messages from the agent to the app.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum Outbound {
    /// `agentd` has read the bootstrap and asks for its task.
    Ready,
    Heartbeat(Heartbeat),
    Event(Event),
    Final(Final),
}

/// Sent every few seconds once the task started.
#[derive(Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    pub seq: u64,
}

/// One line of `claude --output-format stream-json`, numbered. `event` is replaced by
/// `{"truncated": true, ...}` when it would not fit in a message.
#[derive(Clone, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub event: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Done,
    NeedsInput,
    Failed,
}

/// The one message that ends a run.
#[derive(Clone, Serialize, Deserialize)]
pub struct Final {
    pub status: Status,
    /// What the agent reports, or the reason of a failure.
    pub result: String,
    #[serde(default)]
    pub questions: Vec<String>,
    /// The Claude session, to resume it on the next run.
    #[serde(default)]
    pub session_id: Option<String>,
    /// The workspace as a git bundle (base64); absent when empty, not a repository or too large.
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub workspace_error: Option<String>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub exit_code: Option<i32>,
}

/// The answer `claude` is asked to end with (`--json-schema`).
pub const RESULT_SCHEMA: &str = r#"{"type":"object","properties":{"status":{"type":"string","enum":["done","needs_input","failed"]},"summary":{"type":"string"},"questions":{"type":"array","items":{"type":"string"}}},"required":["status","summary"]}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format_is_kind_and_body() {
        let m = Outbound::Heartbeat(Heartbeat { seq: 3 });
        assert_eq!(serde_json::to_value(&m).unwrap(), serde_json::json!({"kind":"heartbeat","body":{"seq":3}}));
        assert_eq!(serde_json::to_string(&Outbound::Ready).unwrap(), r#"{"kind":"ready"}"#);
    }

    #[test]
    fn result_schema_is_json() {
        serde_json::from_str::<Value>(RESULT_SCHEMA).unwrap();
    }
}
