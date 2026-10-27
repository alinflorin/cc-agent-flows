//! The messages between the app and `agentd`, the program inside each agent VM.
//!
//! On the wire every message is `{"kind": ..., "body": ...}`. The app sends `Inbound` on
//! `agent.<id>.in` (the router adds `from`, which is ignored here) and `agentd` sends `Outbound`
//! on `agent.<id>.out`. Everything an agent sends is untrusted: the app validates it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Largest message either side sends. The broker router refuses more than 1 MiB.
pub const MAX_MESSAGE: usize = 1 << 20;

/// What the app writes to the one-shot bootstrap channel when it boots the VM. `seed` is the only
/// secret that travels outside NATS.
#[derive(Clone, Serialize, Deserialize)]
pub struct Bootstrap {
    pub id: String,
    /// The agent's SPIFFE name: `iss` and `sub` of its proofs.
    pub name: String,
    /// The agent's private key (nkey seed).
    pub seed: String,
    /// The pull consumer to read the task from.
    pub consumer: String,
    /// The issuer string of the identity provider. Only compared with, never connected to.
    pub issuer: String,
    /// Where the identity provider can be reached from the VM, e.g. `10.0.2.2:41000`.
    pub idp_addr: String,
    /// Where NATS can be reached from the VM, e.g. `10.0.2.2:4222`.
    pub nats_addr: String,
}

/// Messages from the app to the agent.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum Inbound {
    Task(Task),
    /// The router tells an agent when it sent something malformed.
    Error { reason: String },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Task {
    pub prompt: String,
    #[serde(default)]
    pub model: Option<String>,
    /// `low | medium | high | xhigh | max`.
    #[serde(default)]
    pub effort: Option<String>,
    /// The Claude setup token. It goes only into the `claude` process environment.
    pub claude_token: String,
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
    Heartbeat(Heartbeat),
    Event(Event),
    Final(Final),
}

/// Sent every few seconds, the first one right at start.
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
    }

    #[test]
    fn router_stamp_is_ignored() {
        let raw = r#"{"from":"app","kind":"error","body":{"reason":"x"}}"#;
        assert!(matches!(serde_json::from_str::<Inbound>(raw).unwrap(), Inbound::Error { .. }));
    }

    #[test]
    fn result_schema_is_json() {
        serde_json::from_str::<Value>(RESULT_SCHEMA).unwrap();
    }
}
