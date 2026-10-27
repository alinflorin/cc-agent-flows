//! The messages between the app and `agentd`, the script inside each agent VM
//! (`resources/agentd/agentd.sh`, which builds and parses these by hand with `jq`: keep them in step).
//!
//! They travel over one virtio-serial port per VM (the guest sees `/dev/virtio-ports/agent`), one
//! JSON message per line, in this order: the app sends `Bootstrap`, `agentd` answers `Ready`, the
//! app sends the `Task`, then `agentd` streams `Event`s and finally one `Final`. The
//! channel belongs to one VM, so there are no tokens or identities: the app knows who is talking
//! because of the socket. Everything the agent sends is untrusted: the app validates it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Longest line either side accepts. A longer line is dropped (events) or ends the run (the app).
pub const MAX_MESSAGE: usize = 1 << 20;

/// The first message, from the app: what this VM is and the Claude setup token. `id` is the run's
/// identity (a SPIFFE-style name, see `runs::identity`), a label and not a credential: the channel
/// is what authenticates the VM. It never touches a
/// disk or a command line: it goes through the channel into `agentd`, and from there only into
/// `claude`'s environment.
#[derive(Clone, Serialize, Deserialize)]
pub struct Bootstrap {
    pub id: String,
    /// The Claude setup token. It goes only into the `claude` process environment.
    pub claude_token: String,
    /// The run's way out to the internet, when the VM's network is sealed (see `tunnel.rs`);
    /// without it `agentd` sets up no proxy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel: Option<TunnelKeys>,
    /// The certificate (PEM) of the CA that signs the certificates the egress proxy shows for the
    /// hosts of the run's credential injections. Public; `agentd` makes the VM trust it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca: Option<String>,
}

/// What `agentd` needs to open the SSH tunnel to the app's egress proxy.
#[derive(Clone, Serialize, Deserialize)]
pub struct TunnelKeys {
    /// The client's private key, OpenSSH format. Used by `agentd` only, never readable by `claude`.
    pub key: String,
    /// The app's SSH host key (`ssh-ed25519 AAAA...`).
    pub host_key: String,
}

/// A variable for the `claude` process (from the agent profile). `agentd` skips reserved names.
#[derive(Clone, Serialize, Deserialize)]
pub struct TaskEnv {
    pub name: String,
    #[serde(default)]
    pub value: String,
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
    /// JSON schema of the final answer, as a string; `RESULT_SCHEMA` by default. Always sent, so
    /// the script needs no copy of it.
    #[serde(default = "default_schema")]
    pub schema: String,
    /// Appended to Claude's system prompt (`--append-system-prompt`), from the agent profile.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Exported for `claude` only (not for `agentd`), from the agent profile.
    #[serde(default)]
    pub env: Vec<TaskEnv>,
    /// Hard limit for the `claude` run.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_schema() -> String {
    RESULT_SCHEMA.to_string()
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
    Event(Event),
    Final(Final),
}

/// One line of `claude --output-format stream-json`, numbered. `event` is replaced by
/// `{"truncated": true, ...}` when it would not fit in a message, or `{"type": "raw", ...}` when
/// it was not JSON.
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Final {
    pub status: Status,
    /// What the agent reports, or the reason of a failure.
    pub result: String,
    #[serde(default)]
    pub questions: Vec<String>,
    /// The Claude session, to resume it on the next run.
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// The structured answer (matching the task's schema), for conditions and the coordinator.
    /// Untrusted: the app validates it.
    #[serde(default)]
    pub output: Option<Value>,
    #[serde(default)]
    pub exit_code: Option<i32>,
}

/// The answer `claude` is asked to end with (`--json-schema`).
pub const RESULT_SCHEMA: &str = r#"{"type":"object","properties":{"status":{"type":"string","enum":["done","needs_input","failed"]},"summary":{"type":"string"},"questions":{"type":"array","items":{"type":"string"}},"assumptions":{"type":"array","items":{"type":"string"}}},"required":["status","summary"]}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format_is_kind_and_body() {
        let m = Outbound::Event(Event { seq: 3, event: serde_json::json!({"type": "x"}) });
        assert_eq!(serde_json::to_value(&m).unwrap(), serde_json::json!({"kind":"event","body":{"seq":3,"event":{"type":"x"}}}));
        assert_eq!(serde_json::to_string(&Outbound::Ready).unwrap(), r#"{"kind":"ready"}"#);
    }

    #[test]
    fn result_schema_is_json() {
        serde_json::from_str::<Value>(RESULT_SCHEMA).unwrap();
    }
}
