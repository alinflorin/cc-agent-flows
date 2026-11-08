//! Remote control: an HTTP server inside the app that serves the same UI to a browser and runs the
//! same commands. Started by `--serve` (window stays) or `--headless` (window hidden, implies
//! `--serve`); off otherwise.
//!
//! - `POST /api/<cmd>` with a JSON object of the arguments runs the Tauri command `<cmd>` through
//!   the main window's own IPC handler (`WebviewWindow::on_message`), so every command, its
//!   capability check and its state work unchanged. 200 with the result, 400 with `{"error": ...}`.
//! - `GET /events` is a server-sent-events stream of the app events the UI listens to
//!   (`session-log`, `sessions-changed`, `config-changed`).
//! - Everything else is the frontend: the bundled assets, or in dev mode a proxy to the Vite server.
//!   `index.html` gets `window.__CCAF_REMOTE__` so the page knows it talks to a server.
//!
//! When it runs: always with `--headless` or `--serve`, otherwise when Settings > Remote control
//! says so (`remote.json` in the app data dir, off by default). Address and auth come from that
//! file, and the flags override it: `--bind <ip>` (default 127.0.0.1), `--port <n>` (default 7880),
//! `--auth user:pass` or the `CCAF_AUTH` variable. HTTP basic auth is off by default. Without it
//! nothing stops whoever can reach the port, so bind to a non-loopback address only on a network
//! you trust (or with auth). Independent of auth, the server refuses what a web page in the user's
//! own browser could try against a local port: a `Host` that is not loopback (DNS rebinding, only
//! when bound to loopback), an `Origin` that is not the server itself, and a body that is not
//! `application/json`.

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full, Limited};
use hyper::body::{Frame, Incoming};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponse, InvokeResponseBody};
use tauri::webview::InvokeRequest;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Listener, Manager};
use tokio::sync::{broadcast, mpsc};

const DEFAULT_PORT: u16 = 7880;
/// Largest `/api` body accepted.
const MAX_BODY: usize = 32 * 1024 * 1024;
/// The events the frontend listens to (all emitted from Rust).
const EVENTS: [&str; 3] = ["session-log", "sessions-changed", "config-changed"];

type Body = BoxBody<Bytes, Infallible>;

fn flag(name: &str) -> bool {
    std::env::args().any(|a| a == name)
}

fn value(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let prefix = format!("{name}=");
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned()).or_else(|| args.iter().find_map(|a| a.strip_prefix(&prefix).map(str::to_string)))
}

/// `--headless`: the window is created hidden and never shown.
pub fn headless() -> bool {
    flag("--headless")
}

/// Whether to run the server at all.
pub fn enabled() -> bool {
    flag("--serve") || headless()
}

/// What Settings > Remote control saves (`remote.json`, mode 0600 because of the password).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    /// Start the server when the app opens (the flags start it regardless).
    pub enabled: bool,
    pub bind: String,
    pub port: u16,
    /// HTTP basic auth.
    pub auth: bool,
    pub user: String,
    pub password: String,
}

impl Default for Config {
    fn default() -> Self {
        Config { enabled: false, bind: "127.0.0.1".into(), port: DEFAULT_PORT, auth: false, user: String::new(), password: String::new() }
    }
}

#[derive(Default)]
pub struct Remote {
    config: Mutex<Config>,
    task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Why it is not listening.
    error: Mutex<Option<String>>,
}

fn path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(crate::util::data_dir(app)?.join("remote.json"))
}

fn store(app: &AppHandle, cfg: &Config) -> Result<(), String> {
    let p = path(app)?;
    crate::util::write_json(&p, cfg)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn current(app: &AppHandle) -> Config {
    app.state::<Remote>().config.lock().unwrap().clone()
}

struct Opts {
    addr: SocketAddr,
    /// The expected `Authorization` header value, when auth is on.
    auth: Option<String>,
}

/// The settings with the flags laid over them.
fn opts(cfg: &Config) -> Result<Opts, String> {
    let ip: IpAddr = value("--bind").unwrap_or_else(|| cfg.bind.clone()).parse().map_err(|_| "the address must be an IP address".to_string())?;
    let port: u16 = match value("--port") {
        Some(p) => p.parse().map_err(|_| "--port must be a number".to_string())?,
        None => cfg.port,
    };
    let from_settings = (cfg.auth && !cfg.user.is_empty() && !cfg.password.is_empty()).then(|| format!("{}:{}", cfg.user, cfg.password));
    let auth = value("--auth").or_else(|| std::env::var("CCAF_AUTH").ok()).filter(|a| !a.is_empty()).or(from_settings);
    if cfg.auth && auth.is_none() {
        return Err("authentication is on but there is no user or password".into());
    }
    if let Some(a) = &auth
        && !a.contains(':')
    {
        return Err("the auth must be user:password".into());
    }
    Ok(Opts { addr: SocketAddr::new(ip, port), auth: auth.map(|a| format!("Basic {}", B64.encode(a))) })
}

/// Events from the app to every connected browser: (name, JSON payload).
fn events() -> &'static broadcast::Sender<(String, String)> {
    static TX: OnceLock<broadcast::Sender<(String, String)>> = OnceLock::new();
    TX.get_or_init(|| broadcast::channel(256).0)
}

fn respond(status: StatusCode, content_type: &str, body: impl Into<Bytes>) -> Response<Body> {
    let mut r = Response::new(Full::new(body.into()).boxed());
    *r.status_mut() = status;
    r.headers_mut().insert(hyper::header::CONTENT_TYPE, content_type.parse().unwrap_or_else(|_| "application/octet-stream".parse().unwrap()));
    r
}

fn json(status: StatusCode, v: serde_json::Value) -> Response<Body> {
    respond(status, "application/json", v.to_string())
}

/// Equal without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// The body of an event stream: whatever arrives on the channel.
struct Sse(mpsc::Receiver<Bytes>);

impl hyper::body::Body for Sse {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.0.poll_recv(cx).map(|o| o.map(|b| Ok(Frame::data(b))))
    }
}

fn event_stream() -> Response<Body> {
    let mut rx = events().subscribe();
    let (tx, out) = mpsc::channel::<Bytes>(64);
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        if tx.send(Bytes::from_static(b": ok\n\n")).await.is_err() {
            return;
        }
        loop {
            let msg = tokio::select! {
                m = rx.recv() => match m {
                    Ok((name, data)) => format!("event: {name}\ndata: {data}\n\n"),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                },
                _ = tick.tick() => ": ping\n\n".to_string(),
            };
            // a closed browser tab shows up here, at the latest on the next ping
            if tx.send(Bytes::from(msg)).await.is_err() {
                break;
            }
        }
    });
    let mut r = Response::new(Sse(out).boxed());
    let h = r.headers_mut();
    h.insert(hyper::header::CONTENT_TYPE, "text/event-stream".parse().unwrap());
    h.insert(hyper::header::CACHE_CONTROL, "no-cache".parse().unwrap());
    h.insert("x-accel-buffering", "no".parse().unwrap());
    r
}

/// Runs a command through the main window and waits for its answer.
async fn command(app: &AppHandle, cmd: String, args: serde_json::Value) -> Response<Body> {
    let Some(window) = app.get_webview_window("main") else {
        return json(StatusCode::INTERNAL_SERVER_ERROR, serde_json::json!({"error": "the main window is gone"}));
    };
    let Ok(url) = window.url() else {
        return json(StatusCode::INTERNAL_SERVER_ERROR, serde_json::json!({"error": "the main window has no url"}));
    };
    let request = InvokeRequest { cmd, callback: CallbackFn(0), error: CallbackFn(1), url, body: InvokeBody::Json(args), headers: Default::default(), invoke_key: app.invoke_key().to_string() };
    let (tx, rx) = tokio::sync::oneshot::channel();
    window.on_message(
        request,
        Box::new(move |_w, _cmd, response, _cb, _err| {
            let _ = tx.send(response);
        }),
    );
    match rx.await {
        Ok(InvokeResponse::Ok(InvokeResponseBody::Json(s))) => respond(StatusCode::OK, "application/json", s),
        Ok(InvokeResponse::Ok(InvokeResponseBody::Raw(_))) => respond(StatusCode::OK, "application/json", "null"),
        Ok(InvokeResponse::Err(e)) => json(StatusCode::BAD_REQUEST, serde_json::json!({"error": e.0})),
        Err(e) => json(StatusCode::INTERNAL_SERVER_ERROR, serde_json::json!({"error": e.to_string()})),
    }
}

/// The page tells itself apart from the desktop window by this flag.
fn mark_remote(html: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(html);
    text.replacen("<head>", "<head><script>window.__CCAF_REMOTE__=true</script>", 1).into_bytes()
}

fn page(content_type: &str, bytes: Vec<u8>) -> Response<Body> {
    let bytes = if content_type.starts_with("text/html") { mark_remote(&bytes) } else { bytes };
    respond(StatusCode::OK, content_type, bytes)
}

/// Dev mode: the frontend comes from the Vite server (its hot reload socket is not forwarded).
async fn dev_proxy(base: &tauri::Url, path_and_query: &str) -> Result<Response<Body>, String> {
    let host = base.host_str().ok_or("the dev url has no host")?.to_string();
    let port = base.port_or_known_default().ok_or("the dev url has no port")?;
    let stream = tokio::net::TcpStream::connect((host.as_str(), port)).await.map_err(|e| format!("the dev server is not reachable: {e}"))?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn(conn);
    let req = Request::builder().uri(path_and_query).header(hyper::header::HOST, format!("{host}:{port}")).body(Empty::<Bytes>::new()).map_err(|e| e.to_string())?;
    let resp = sender.send_request(req).await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let content_type = resp.headers().get(hyper::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("application/octet-stream").to_string();
    let bytes = resp.into_body().collect().await.map_err(|e| e.to_string())?.to_bytes().to_vec();
    if !status.is_success() {
        return Ok(respond(status, &content_type, bytes));
    }
    Ok(page(&content_type, bytes))
}

async fn frontend(app: &AppHandle, uri: &hyper::Uri) -> Response<Body> {
    if tauri::is_dev() {
        let Some(base) = app.config().build.dev_url.clone() else {
            return json(StatusCode::NOT_FOUND, serde_json::json!({"error": "no dev url"}));
        };
        let pq = uri.path_and_query().map_or("/", |p| p.as_str());
        return dev_proxy(&base, pq).await.unwrap_or_else(|e| json(StatusCode::BAD_GATEWAY, serde_json::json!({"error": e})));
    }
    match app.asset_resolver().get(uri.path().to_string()) {
        Some(a) => page(&a.mime_type, a.bytes),
        None => json(StatusCode::NOT_FOUND, serde_json::json!({"error": "not found"})),
    }
}

fn loopback_name(host: &str) -> bool {
    let name = host.rsplit_once(':').filter(|(h, p)| !h.ends_with(']') || p.chars().all(|c| c.is_ascii_digit())).map_or(host, |(h, _)| h);
    matches!(name, "localhost" | "127.0.0.1" | "[::1]")
}

async fn handle(app: AppHandle, opts: std::sync::Arc<Opts>, req: Request<Incoming>) -> Result<Response<Body>, Infallible> {
    let header = |n: &str| req.headers().get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
    let host = header("host").unwrap_or_default();
    if opts.addr.ip().is_loopback() && !loopback_name(&host) {
        return Ok(json(StatusCode::FORBIDDEN, serde_json::json!({"error": "forbidden host"})));
    }
    if let Some(expected) = &opts.auth
        && !header("authorization").is_some_and(|a| same(&a, expected))
    {
        let mut r = json(StatusCode::UNAUTHORIZED, serde_json::json!({"error": "authentication required"}));
        r.headers_mut().insert(hyper::header::WWW_AUTHENTICATE, "Basic realm=\"CC Agent Flows\", charset=\"UTF-8\"".parse().unwrap());
        return Ok(r);
    }
    // a page on another origin must not drive this server with the user's browser
    if let Some(origin) = header("origin")
        && origin.split_once("://").map(|(_, a)| a) != Some(host.as_str())
    {
        return Ok(json(StatusCode::FORBIDDEN, serde_json::json!({"error": "forbidden origin"})));
    }
    let path = req.uri().path().to_string();
    if let Some(cmd) = path.strip_prefix("/api/") {
        if req.method() != Method::POST {
            return Ok(json(StatusCode::METHOD_NOT_ALLOWED, serde_json::json!({"error": "use POST"})));
        }
        if !header("content-type").is_some_and(|c| c.to_ascii_lowercase().starts_with("application/json")) {
            return Ok(json(StatusCode::UNSUPPORTED_MEDIA_TYPE, serde_json::json!({"error": "the body must be application/json"})));
        }
        let cmd = cmd.to_string();
        let bytes = match Limited::new(req.into_body(), MAX_BODY).collect().await {
            Ok(b) => b.to_bytes(),
            Err(_) => return Ok(json(StatusCode::PAYLOAD_TOO_LARGE, serde_json::json!({"error": "the body is too large"}))),
        };
        let args = if bytes.is_empty() {
            serde_json::json!({})
        } else {
            match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(e) => return Ok(json(StatusCode::BAD_REQUEST, serde_json::json!({"error": format!("invalid JSON: {e}")}))),
            }
        };
        return Ok(command(&app, cmd, args).await);
    }
    if req.method() != Method::GET && req.method() != Method::HEAD {
        return Ok(json(StatusCode::METHOD_NOT_ALLOWED, serde_json::json!({"error": "not found"})));
    }
    if path == "/events" {
        return Ok(event_stream());
    }
    Ok(frontend(&app, req.uri()).await)
}

/// Starts listening if the server is on (stops the listener that was running, and its connections).
pub fn start(app: &AppHandle) {
    let state = app.state::<Remote>();
    if let Some(t) = state.task.lock().unwrap().take() {
        t.abort();
    }
    *state.error.lock().unwrap() = None;
    let cfg = state.config.lock().unwrap().clone();
    if !(enabled() || cfg.enabled) {
        return;
    }
    let opts = match opts(&cfg) {
        Ok(o) => Arc::new(o),
        Err(e) => {
            log::error!("remote: {e}");
            *state.error.lock().unwrap() = Some(e);
            return;
        }
    };
    if !opts.addr.ip().is_loopback() && opts.auth.is_none() {
        log::warn!("remote: listening beyond this machine without authentication");
    }
    let app2 = app.clone();
    let task = tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(opts.addr).await {
            Ok(l) => l,
            Err(e) => {
                log::error!("remote: cannot listen on {}: {e}", opts.addr);
                *app2.state::<Remote>().error.lock().unwrap() = Some(format!("cannot listen on {}: {e}", opts.addr));
                return;
            }
        };
        log::info!("remote: open http://{}/ in a browser", opts.addr);
        // the connections belong to the listener: stopping it ends them (and so a changed login applies at once)
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            while connections.try_join_next().is_some() {}
            let app = app2.clone();
            let opts = opts.clone();
            connections.spawn(async move {
                let svc = service_fn(move |req| handle(app.clone(), opts.clone(), req));
                let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), svc).await;
            });
        }
    });
    *state.task.lock().unwrap() = Some(task);
}

/// Loads the saved settings, forwards the app's events to the browsers and starts the server if it is on.
/// Call once at startup, after `app.manage(Remote::default())`.
pub fn init(app: &AppHandle) {
    let cfg = path(app).ok().and_then(|p| crate::util::read_json::<Config>(&p)).unwrap_or_default();
    *app.state::<Remote>().config.lock().unwrap() = cfg;
    for name in EVENTS {
        app.listen_any(name, move |e| {
            let _ = events().send((name.to_string(), e.payload().to_string()));
        });
    }
    start(app);
}

#[derive(Serialize)]
pub struct Status {
    enabled: bool,
    bind: String,
    port: u16,
    auth: bool,
    user: String,
    /// The password is never sent back.
    has_password: bool,
    /// Running because of `--serve` or `--headless`, whatever `enabled` says.
    forced: bool,
    /// The address to open.
    url: String,
    error: Option<String>,
}

fn status(app: &AppHandle) -> Status {
    let c = current(app);
    let shown = opts(&c).map(|o| o.addr).unwrap_or_else(|_| SocketAddr::new(IpAddr::from([127, 0, 0, 1]), c.port));
    let host = if shown.ip().is_unspecified() { "localhost".to_string() } else { shown.ip().to_string() };
    let host = if host.contains(':') { format!("[{host}]") } else { host };
    Status {
        enabled: c.enabled,
        bind: c.bind,
        port: c.port,
        auth: c.auth,
        user: c.user,
        has_password: !c.password.is_empty(),
        forced: enabled(),
        url: format!("http://{host}:{}/", shown.port()),
        error: app.state::<Remote>().error.lock().unwrap().clone(),
    }
}

#[tauri::command]
pub fn remote_get(app: AppHandle) -> Status {
    status(&app)
}

/// Saves the settings and restarts the server a moment later (so the answer still reaches a browser
/// that is connected through it). An empty `password` keeps the saved one.
#[tauri::command]
pub fn remote_set(app: AppHandle, enabled: bool, bind: String, port: u16, auth: bool, user: String, password: Option<String>) -> Result<Status, String> {
    if port < 1024 {
        return Err("pick a port from 1024 up".into());
    }
    if bind.parse::<IpAddr>().is_err() {
        return Err("the address must be an IP address, like 127.0.0.1 or 0.0.0.0".into());
    }
    let old = current(&app);
    let password = password.filter(|p| !p.is_empty()).unwrap_or(old.password);
    let user = user.trim().to_string();
    if auth && (user.is_empty() || password.is_empty()) {
        return Err("authentication needs a user and a password".into());
    }
    if user.contains(':') {
        return Err("the user cannot contain a colon".into());
    }
    let cfg = Config { enabled, bind, port, auth, user, password };
    store(&app, &cfg)?;
    *app.state::<Remote>().config.lock().unwrap() = cfg;
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        start(&app2);
    });
    Ok(status(&app))
}
