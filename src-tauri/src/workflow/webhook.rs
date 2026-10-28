//! The HTTP listener behind the workflow's webhook nodes: a request of any method to `/hook/<token>`
//! starts a session whose request is the body exactly as it arrived (headers are not passed on). For
//! GET, DELETE and HEAD the request line `METHOD http://host/hook?query` comes first, without the
//! token, followed by the body if there is one (after the node's own text, if it has any),
//! for the agent the node points to. It listens on the loopback address only, so nothing outside
//! this machine reaches it unless the user forwards a port or runs a tunnel; the secret in the
//! URL is what authorizes a call, so treat the URL like a password. Calls are limited in size
//! and rate, and the sessions they start fall under the app's usual limits.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Mutex;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Manager};

use crate::workflow::triggers;
use crate::workflow::{self, NodeKind};

pub const DEFAULT_PORT: u16 = 7878;
/// Largest body taken, in bytes: a session's request is limited to 64 KB, and the node's own text comes first.
const MAX_BODY: usize = 48 * 1024;
/// Calls per minute, for all webhooks together.
const MAX_PER_MINUTE: u32 = 60;

#[derive(Serialize, Deserialize)]
struct Config {
    port: u16,
}

#[derive(Default)]
pub struct Webhook {
    task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Why it is not listening.
    error: Mutex<Option<String>>,
}

fn config_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    crate::util::data_dir(app).ok().map(|d| d.join("webhook.json"))
}

pub fn port(app: &AppHandle) -> u16 {
    config_path(app).and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str::<Config>(&t).ok()).map_or(DEFAULT_PORT, |c| c.port)
}

fn reply(status: StatusCode, body: serde_json::Value) -> Response<Full<Bytes>> {
    let mut r = Response::new(Full::new(Bytes::from(body.to_string())));
    *r.status_mut() = status;
    r.headers_mut().insert(hyper::header::CONTENT_TYPE, "application/json".parse().unwrap());
    r
}

/// Calls in the current minute, to cap them.
static RATE: Mutex<(u64, u32)> = Mutex::new((0, 0));

fn allowed_now() -> bool {
    let minute = crate::util::unix_secs() / 60;
    let mut r = RATE.lock().unwrap();
    if r.0 != minute {
        *r = (minute, 0);
    }
    r.1 += 1;
    r.1 <= MAX_PER_MINUTE
}

async fn handle(app: AppHandle, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let token = req.uri().path().strip_prefix("/hook/").map(str::to_string);
    let Some(token) = token.filter(|t| !t.is_empty() && !t.contains('/')) else {
        return Ok(reply(StatusCode::NOT_FOUND, json!({"error": "not found"})));
    };
    // GET, DELETE and HEAD carry their information in the URL: pass it on, without the secret token
    let url_line = matches!(req.method().as_str(), "GET" | "DELETE" | "HEAD").then(|| {
        let host = req.headers().get(hyper::header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost");
        let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
        format!("{} http://{host}/hook{query}", req.method())
    });
    if !allowed_now() {
        return Ok(reply(StatusCode::TOO_MANY_REQUESTS, json!({"error": "too many calls, try again in a minute"})));
    }
    let body = match Limited::new(req.into_body(), MAX_BODY).collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => return Ok(reply(StatusCode::PAYLOAD_TOO_LARGE, json!({"error": format!("the body is limited to {MAX_BODY} bytes")}))),
    };
    // taken as it is; bytes that are not UTF-8 become U+FFFD
    let mut payload = String::from_utf8_lossy(&body).into_owned();
    if let Some(line) = url_line {
        payload = if payload.is_empty() { line } else { format!("{line}\n\n{payload}") };
    }
    let started = tauri::async_runtime::spawn_blocking(move || {
        let root = workflow::root(&app).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        let Some(f) = triggers::find_all(&root, NodeKind::Webhook).into_iter().find(|f| f.node.trigger.token == token) else {
            return Err((StatusCode::NOT_FOUND, "not found".to_string()));
        };
        triggers::fire(&app, &f, &payload).map_err(|e| (if e.contains("running already") || e.contains("sessions are running") { StatusCode::TOO_MANY_REQUESTS } else { StatusCode::BAD_REQUEST }, e))
    })
    .await;
    Ok(match started {
        Ok(Ok(sid)) => reply(StatusCode::ACCEPTED, json!({"session": sid})),
        Ok(Err((status, e))) => reply(status, json!({"error": e})),
        Err(e) => reply(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e.to_string()})),
    })
}

/// Starts listening on the configured port (stops the listener that was running).
pub fn start(app: &AppHandle) {
    let state = app.state::<Webhook>();
    if let Some(t) = state.task.lock().unwrap().take() {
        t.abort();
    }
    *state.error.lock().unwrap() = None;
    let addr = SocketAddr::from(([127, 0, 0, 1], port(app)));
    let app2 = app.clone();
    let task = tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                *app2.state::<Webhook>().error.lock().unwrap() = Some(format!("cannot listen on {addr}: {e}"));
                return;
            }
        };
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let app = app2.clone();
            tauri::async_runtime::spawn(async move {
                let svc = service_fn(move |req| handle(app.clone(), req));
                let conn = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), svc);
                // a slow client must not hold the connection forever
                let _ = tokio::time::timeout(std::time::Duration::from_secs(30), conn).await;
            });
        }
    });
    *state.task.lock().unwrap() = Some(task);
}

#[derive(Serialize)]
pub struct Status {
    port: u16,
    error: Option<String>,
}

#[tauri::command]
pub fn webhook_get(app: AppHandle) -> Status {
    // the bind happens a moment after start: give it a chance to report a busy port
    Status { port: port(&app), error: app.state::<Webhook>().error.lock().unwrap().clone() }
}

#[tauri::command]
pub fn webhook_set(app: AppHandle, port: u16) -> Result<Status, String> {
    if port < 1024 {
        return Err("pick a port from 1024 up".into());
    }
    let path = config_path(&app).ok_or("no data directory")?;
    std::fs::write(path, serde_json::to_string(&Config { port }).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    start(&app);
    Ok(Status { port, error: None })
}
