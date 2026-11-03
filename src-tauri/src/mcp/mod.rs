//! An MCP server so the user's own agent (Claude Code, or any MCP client) can set the app up:
//! projects, agent profiles, images, variables, secrets, injections, integrations, allowed hosts and
//! workflows, in a project or in the global settings. It offers two tools only, to keep the
//! agent's context small: `read_instructions` (the model of the app and the shapes of the bodies)
//! and `execute`, which runs a command of a fake command line, `ccaf` (see `cli.rs`).
//!
//! Transport: MCP "streamable HTTP", answering every POST to `/mcp` with one JSON message (no
//! streaming, no sessions). It listens on the loopback address only, and a call needs the bearer
//! token (Settings > MCP). Requests with an `Origin` header (a browser) or a foreign `Host` (DNS
//! rebinding) are refused. It is off until the user turns it on. The settings are `mcp.json`.

mod cli;
pub mod quick_add;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Mutex;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Manager};

pub const DEFAULT_PORT: u16 = 7879;
const MAX_BODY: usize = 1024 * 1024;
const INSTRUCTIONS: &str = include_str!("instructions.md");
/// Protocol versions this server can speak (it uses nothing that differs between them).
const VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub port: u16,
    /// The bearer token clients send; made when the server is first turned on.
    pub token: String,
    /// Any change at all; off means read only.
    pub write: bool,
    /// Changes to the global settings (needs `write`).
    pub write_global: bool,
    /// Starting sessions and single runs (they spend the subscription and boot VMs); cancelling is always allowed.
    pub runs: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { enabled: false, port: DEFAULT_PORT, token: String::new(), write: true, write_global: true, runs: true }
    }
}

#[derive(Default)]
pub struct Mcp {
    config: Mutex<Config>,
    task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Why it is not listening.
    error: Mutex<Option<String>>,
}

fn path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(crate::util::data_dir(app)?.join("mcp.json"))
}

fn new_token() -> String {
    let bytes: [u8; 32] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn store(app: &AppHandle, cfg: &Config) -> Result<(), String> {
    let p = path(app)?;
    crate::util::write_json(&p, cfg)?;
    // the token is a password
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn current(app: &AppHandle) -> Config {
    app.state::<Mcp>().config.lock().unwrap().clone()
}

/// Whether two byte strings are equal, without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn reply(status: StatusCode, body: Option<Value>) -> Response<Full<Bytes>> {
    let mut r = Response::new(Full::new(body.map(|b| Bytes::from(b.to_string())).unwrap_or_default()));
    *r.status_mut() = status;
    if status == StatusCode::METHOD_NOT_ALLOWED {
        r.headers_mut().insert(hyper::header::ALLOW, "POST".parse().unwrap());
    }
    r.headers_mut().insert(hyper::header::CONTENT_TYPE, "application/json".parse().unwrap());
    r
}

fn host_is_local(host: &str) -> bool {
    let name = if let Some(rest) = host.strip_prefix('[') { rest.split(']').next().unwrap_or("") } else { host.split(':').next().unwrap_or("") };
    matches!(name, "127.0.0.1" | "localhost" | "::1")
}

fn tools() -> Value {
    json!([
        {
            "name": "read_instructions",
            "description": "Call this first. How CC Agent Flows is organised (projects, global settings, agent profiles, images, variables, secrets, injections, workflows) and the JSON shapes `execute` takes.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "execute",
            "description": "Run a `ccaf` command that configures the app, e.g. `ccaf projects list` or `ccaf variables set API_HOST example.com --project demo`. `ccaf help` lists every command. Structured bodies go in `stdin` as JSON.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command line, with or without the leading `ccaf`." },
                    "stdin": { "type": "string", "description": "A JSON body (profile, image, workflow...) or a text value (a secret, a prompt) for the commands that read one." }
                },
                "required": ["command"],
                "additionalProperties": false
            }
        }
    ])
}

fn text_result(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

async fn call_tool(app: &AppHandle, params: &Value) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);
    match name {
        "read_instructions" => Ok(text_result(format!("{INSTRUCTIONS}\n{}", cli::HELP), false)),
        "execute" => {
            let Some(command) = args.get("command").and_then(Value::as_str).map(str::to_string) else {
                return Err((-32602, "`command` is required".into()));
            };
            let stdin = args.get("stdin").and_then(Value::as_str).map(str::to_string);
            let cfg = current(app);
            log::info!("mcp: {}", command.chars().take(200).collect::<String>());
            let app = app.clone();
            let out = tauri::async_runtime::spawn_blocking(move || cli::run(&app, &cfg, &command, stdin.as_deref())).await.map_err(|e| (-32603, e.to_string()))?;
            Ok(match out {
                Ok(t) => text_result(t, false),
                Err(e) => text_result(format!("error: {e}"), true),
            })
        }
        other => Err((-32602, format!("unknown tool {other:?}"))),
    }
}

/// Answers one JSON-RPC message; `None` for a notification (no answer).
async fn rpc(app: &AppHandle, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result: Result<Value, (i64, String)> = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
            let version = VERSIONS.iter().find(|v| **v == asked).unwrap_or(&VERSIONS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "ccaf", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "CC Agent Flows: configure projects, agents, images, secrets, workflows and runs. Call read_instructions first."
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call_tool(app, &params).await,
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    })
}

async fn handle(app: AppHandle, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let headers = req.headers();
    let host_ok = headers.get(hyper::header::HOST).and_then(|h| h.to_str().ok()).is_some_and(host_is_local);
    if !host_ok || headers.contains_key(hyper::header::ORIGIN) {
        return Ok(reply(StatusCode::FORBIDDEN, Some(json!({ "error": "forbidden" }))));
    }
    if req.uri().path() != "/mcp" {
        return Ok(reply(StatusCode::NOT_FOUND, Some(json!({ "error": "not found" }))));
    }
    let cfg = current(&app);
    let given = headers.get(hyper::header::AUTHORIZATION).and_then(|h| h.to_str().ok()).and_then(|h| h.strip_prefix("Bearer ")).unwrap_or("");
    if !cfg.enabled || cfg.token.is_empty() || !same(given.as_bytes(), cfg.token.as_bytes()) {
        let mut r = reply(StatusCode::UNAUTHORIZED, Some(json!({ "error": "missing or wrong bearer token" })));
        r.headers_mut().insert(hyper::header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
        return Ok(r);
    }
    if req.method() != Method::POST {
        return Ok(reply(StatusCode::METHOD_NOT_ALLOWED, None));
    }
    let bytes = match Limited::new(req.into_body(), MAX_BODY).collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => return Ok(reply(StatusCode::PAYLOAD_TOO_LARGE, Some(json!({ "error": "body too large" })))),
    };
    let Ok(msg) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(reply(StatusCode::BAD_REQUEST, Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" } }))));
    };
    let answer = match &msg {
        Value::Array(items) => {
            let mut out = vec![];
            for m in items {
                out.extend(rpc(&app, m).await);
            }
            (!out.is_empty()).then_some(Value::Array(out))
        }
        m => rpc(&app, m).await,
    };
    Ok(match answer {
        Some(a) => reply(StatusCode::OK, Some(a)),
        None => reply(StatusCode::ACCEPTED, None),
    })
}

/// Starts listening if the server is on (stops the listener that was running).
pub fn start(app: &AppHandle) {
    let state = app.state::<Mcp>();
    if let Some(t) = state.task.lock().unwrap().take() {
        t.abort();
    }
    *state.error.lock().unwrap() = None;
    let cfg = state.config.lock().unwrap().clone();
    if !cfg.enabled {
        return;
    }
    let addr = SocketAddr::from(([127, 0, 0, 1], cfg.port));
    let app2 = app.clone();
    let task = tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                *app2.state::<Mcp>().error.lock().unwrap() = Some(format!("cannot listen on {addr}: {e}"));
                return;
            }
        };
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let app = app2.clone();
            tauri::async_runtime::spawn(async move {
                let svc = service_fn(move |req| handle(app.clone(), req));
                let conn = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), svc);
                // a command can take a while (an image save), but a stuck client must not hold the connection
                let _ = tokio::time::timeout(std::time::Duration::from_secs(120), conn).await;
            });
        }
    });
    *state.task.lock().unwrap() = Some(task);
}

/// Loads the saved settings and starts the server if it is on. Call once at startup, after `app.manage(Mcp::default())`.
pub fn init(app: &AppHandle) {
    let cfg = path(app).ok().and_then(|p| crate::util::read_json::<Config>(&p)).unwrap_or_default();
    *app.state::<Mcp>().config.lock().unwrap() = cfg;
    start(app);
}

#[derive(Serialize)]
pub struct Status {
    config: Config,
    /// The address clients connect to.
    url: String,
    error: Option<String>,
}

fn status(app: &AppHandle) -> Status {
    let config = current(app);
    Status { url: format!("http://127.0.0.1:{}/mcp", config.port), error: app.state::<Mcp>().error.lock().unwrap().clone(), config }
}

#[tauri::command]
pub fn mcp_get(app: AppHandle) -> Status {
    status(&app)
}

/// Saves the settings and restarts the server. The token is kept as it is (a new one is made when
/// there is none); `mcp_new_token` changes it.
#[tauri::command]
pub fn mcp_set(app: AppHandle, enabled: bool, port: u16, write: bool, write_global: bool, runs: bool) -> Result<Status, String> {
    if port < 1024 {
        return Err("pick a port from 1024 up".into());
    }
    let mut cfg = current(&app);
    cfg = Config { enabled, port, write, write_global, runs, token: std::mem::take(&mut cfg.token) };
    if cfg.token.is_empty() {
        cfg.token = new_token();
    }
    store(&app, &cfg)?;
    *app.state::<Mcp>().config.lock().unwrap() = cfg;
    start(&app);
    Ok(status(&app))
}

/// Makes a new token: clients with the old one are refused from now on.
#[tauri::command]
pub fn mcp_new_token(app: AppHandle) -> Result<Status, String> {
    let mut cfg = current(&app);
    cfg.token = new_token();
    store(&app, &cfg)?;
    *app.state::<Mcp>().config.lock().unwrap() = cfg;
    Ok(status(&app))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_hosts_only() {
        for h in ["127.0.0.1:7879", "localhost", "localhost:1", "[::1]:7879"] {
            assert!(host_is_local(h), "{h}");
        }
        for h in ["evil.com", "127.0.0.1.evil.com:80", "10.0.0.1", ""] {
            assert!(!host_is_local(h), "{h}");
        }
    }

    #[test]
    fn tokens_compare_whole() {
        assert!(same(b"abc", b"abc"));
        assert!(!same(b"abc", b"abd") && !same(b"abc", b"ab"));
        assert_eq!(new_token().len(), 64);
    }

    #[test]
    fn two_tools_with_schemas() {
        let t = tools();
        let names: Vec<_> = t.as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["read_instructions", "execute"]);
        assert!(INSTRUCTIONS.contains("--global"));
    }
}
