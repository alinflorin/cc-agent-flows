//! The NATS broker the app runs for agent communication (see "Agent communication" in CLAUDE.md).
//!
//! Agents authenticate to NATS with the OIDC access token the app's identity provider (`idp`)
//! gave them, not with NATS credentials of their own. NATS hands every connection that is not the
//! app itself to an auth callout (`callout`), which verifies the token and answers with a NATS
//! user whose permissions are that agent's alone and whose session lasts `SESSION_TTL`. So
//! `release` stops reconnects at once, and the agent needs nothing but its key.
//!
//! Agents never talk to each other. Every message an agent sends goes to the app (`Broker::inbox`),
//! and the app alone decides the flow by sending messages to agents (`Broker::send`).
//!
//! Keys are generated on each start and the JetStream store is wiped with them: nothing here
//! outlives the app run.

// Nothing drives runs yet; the pieces below are exercised by the tests only.
#![allow(dead_code)]

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::{self, consumer, stream};
use async_nats::ConnectOptions;
use futures::StreamExt;
use nkeys::KeyPair;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::Manager;

use crate::callout;
use crate::idp::{self, Identity, Idp, TRUST_DOMAIN, now};

pub(crate) const STREAM: &str = "AGENTS";
/// The NATS account everything runs in.
pub(crate) const ACCOUNT: &str = "APP";
/// Audience of the access tokens agents present to NATS.
pub const AUDIENCE: &str = "nats";
/// Default lifetime of an identity. A run is shorter; this bounds a leaked credential.
pub const DEFAULT_TTL: Duration = Duration::from_secs(3600);
/// How long one NATS connection lasts before the agent has to reconnect with a fresh token.
pub const SESSION_TTL: Duration = Duration::from_secs(15 * 60);
/// Largest message the router will forward.
const MAX_MESSAGE: usize = 1 << 20;

/// A newly allocated identity with what the agent needs besides its tokens.
#[derive(Serialize)]
pub struct Issued {
    #[serde(flatten)]
    pub identity: Identity,
    /// The agent's private key (nkey seed). It signs the proof that gets tokens from `idp`.
    pub seed: String,
    /// The pull consumer to read tasks from, on stream `AGENTS`.
    pub consumer: String,
}

/// Kills the server when dropped, including when `Broker::start` bails out half way.
struct ServerProcess(Child);

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// What an agent sends on `agent.<id>.out`. The sender is not in it: the router takes it from the
/// subject, which NATS only lets that agent publish on.
#[derive(Deserialize)]
struct Outgoing {
    kind: String,
    #[serde(default)]
    body: Value,
}

/// What the recipient gets.
#[derive(Clone, Serialize, Deserialize)]
pub struct Message {
    pub from: String,
    pub kind: String,
    pub body: Value,
}

pub struct Broker {
    _server: ServerProcess,
    _router: AbortOnDrop,
    _callout: AbortOnDrop,
    _oidc: AbortOnDrop,
    pub port: u16,
    /// The OIDC provider the agents' identities live in.
    pub idp: Arc<Idp>,
    /// Everything agents send. The orchestrator subscribes and decides what happens next.
    pub inbox: tokio::sync::broadcast::Sender<Message>,
    pub client: async_nats::Client,
    pub jetstream: jetstream::Context,
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Agent ids end up in subjects and consumer names, so keep them to a safe alphabet.
fn check_id(id: &str) -> Result<(), String> {
    let ok = !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok { Ok(()) } else { Err(format!("invalid agent id: {id:?}")) }
}

/// The subjects an agent may use: publish on its own `out`, read its own consumer, nothing else.
pub(crate) fn agent_permissions(id: &str) -> (Vec<String>, Vec<String>) {
    let consumer = format!("in-{id}");
    let publish = vec![
        format!("agent.{id}.out"),
        format!("$JS.API.CONSUMER.INFO.{STREAM}.{consumer}"),
        format!("$JS.API.CONSUMER.MSG.NEXT.{STREAM}.{consumer}"),
        format!("$JS.ACK.{STREAM}.{consumer}.>"),
    ];
    (publish, vec!["_INBOX.>".to_string()])
}

const PID_FILE: &str = "nats.pid";

/// A server outlives the app when the app is killed rather than closed (`tauri dev` does that on
/// every rebuild), and would keep its binary busy. Stop the one a previous run left behind.
fn kill_stale_server(dir: &Path) {
    let Some(pid) = std::fs::read_to_string(dir.join(PID_FILE)).ok().and_then(|s| s.trim().parse::<u32>().ok()) else {
        return;
    };
    let pid = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    // The pid may have been reused by something else since: only kill a nats-server.
    if let Some(p) = sys.process(pid).filter(|p| p.name().to_string_lossy().contains("nats-server")) {
        p.kill();
        p.wait();
    }
}

fn free_port() -> Result<u16, String> {
    let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    l.local_addr().map(|a| a.port()).map_err(|e| e.to_string())
}

fn quoted(s: &str) -> String {
    format!("{s:?}")
}

impl Broker {
    /// Starts `nats-server` (built by the caller, see `nats::command`) with its state under `dir`.
    pub async fn start(mut server: Command, dir: &Path) -> Result<Broker, String> {
        let port = free_port()?;
        // The app's own connection: a key pair, so no secret ever sits in the config file.
        let app_user = Arc::new(KeyPair::new_user());
        // Signs the callout's answers. Only its public half goes in the config.
        let issuer = KeyPair::new_account();

        kill_stale_server(dir);
        let _ = std::fs::remove_dir_all(dir);
        let store = dir.join("jetstream");
        std::fs::create_dir_all(&store).map_err(|e| e.to_string())?;
        let conf = format!(
            "listen: 127.0.0.1:{port}\n\
             jetstream: {{ store_dir: {store} }}\n\
             system_account: SYS\n\
             accounts: {{\n  SYS: {{}}\n  {ACCOUNT}: {{ jetstream: enabled, users: [ {{ nkey: {app} }} ] }}\n}}\n\
             authorization: {{ auth_callout: {{ issuer: {issuer}, account: {ACCOUNT}, auth_users: [ {app} ] }} }}\n",
            store = quoted(&store.to_string_lossy()),
            app = app_user.public_key(),
            issuer = issuer.public_key(),
        );
        let conf_path = dir.join("nats.conf");
        std::fs::write(&conf_path, conf).map_err(|e| e.to_string())?;

        let child = server
            .arg("-c")
            .arg(&conf_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start nats-server: {e}"))?;
        let mut server = ServerProcess(child);
        let _ = std::fs::write(dir.join(PID_FILE), server.0.id().to_string());

        let mut last = String::new();
        for _ in 0..50 {
            let opts = ConnectOptions::with_nkey(app_user.seed().map_err(|e| e.to_string())?);
            match tokio::time::timeout(Duration::from_millis(200), opts.connect(format!("127.0.0.1:{port}"))).await {
                Ok(Ok(client)) => {
                    let js = jetstream::new(client.clone());
                    js.get_or_create_stream(stream::Config {
                        name: STREAM.into(),
                        subjects: vec!["agent.*.in".into(), "agent.*.out".into()],
                        storage: stream::StorageType::File,
                        max_age: Duration::from_secs(7 * 24 * 3600),
                        ..Default::default()
                    })
                    .await
                    .map_err(|e| format!("could not create stream: {e}"))?;

                    let idp = Idp::new();
                    let oidc = idp::serve(idp.clone()).await?;
                    let (inbox, _) = tokio::sync::broadcast::channel(256);
                    let router = tokio::spawn(route(client.clone(), js.clone(), idp.clone(), inbox.clone()));
                    let callout = tokio::spawn(callout::serve(client.clone(), idp.clone(), issuer));
                    // Make sure both are subscribed before anyone can connect or publish.
                    client.flush().await.map_err(|e| e.to_string())?;
                    return Ok(Broker {
                        _server: server,
                        _router: AbortOnDrop(router),
                        _callout: AbortOnDrop(callout),
                        _oidc: AbortOnDrop(oidc),
                        port,
                        idp,
                        inbox,
                        client,
                        jetstream: js,
                    });
                }
                Ok(Err(e)) => last = e.to_string(),
                Err(_) => last = "timed out".into(),
            }
            if let Ok(Some(status)) = server.0.try_wait() {
                return Err(format!("nats-server exited: {status}"));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(format!("nats-server did not come up: {last}"))
    }

    /// Allocates an identity for a new agent: a guid, its task queue and a key pair. It registers
    /// the identity with the identity provider, where the agent trades a proof signed with that key
    /// for tokens, including the one NATS accepts.
    pub async fn allocate(&self, project_id: &str, session_id: &str, profile: &str, ttl: Duration) -> Result<Issued, String> {
        // These become path segments of the SPIFFE name, so they must not smuggle in a `/`.
        for segment in [project_id, session_id, profile] {
            check_id(segment)?;
        }
        let key = KeyPair::new_user();
        let id = uuid::Uuid::new_v4().to_string();
        let identity = Identity {
            id: id.clone(),
            name: format!("spiffe://{TRUST_DOMAIN}/{project_id}/{session_id}/{profile}/agent-{id}"),
            project_id: project_id.into(),
            session_id: session_id.into(),
            profile: profile.into(),
            expires_at: now() + ttl.as_secs(),
            public_key: key.public_key(),
        };

        let consumer = format!("in-{id}");
        self.jetstream
            .get_stream(STREAM)
            .await
            .map_err(|e| e.to_string())?
            .create_consumer(consumer::pull::Config {
                durable_name: Some(consumer.clone()),
                filter_subject: format!("agent.{id}.in"),
                ack_policy: consumer::AckPolicy::Explicit,
                ..Default::default()
            })
            .await
            .map_err(|e| format!("could not create consumer: {e}"))?;

        self.idp.register(identity.clone());
        Ok(Issued { identity, seed: key.seed().map_err(|e| e.to_string())?, consumer })
    }

    /// Withdraws an identity: the router stops forwarding for it, its tokens stop verifying, the
    /// callout refuses its reconnects and its queue is deleted. A connection already open lasts
    /// until its session ends (at most `SESSION_TTL`).
    pub async fn release(&self, id: &str) -> Result<(), String> {
        check_id(id)?;
        self.idp.remove(id);
        let stream = self.jetstream.get_stream(STREAM).await.map_err(|e| e.to_string())?;
        let _ = stream.delete_consumer(&format!("in-{id}")).await;
        Ok(())
    }

    /// Queues a message for an agent; it waits in the stream until the agent consumes it.
    pub async fn send(&self, id: &str, msg: &Message) -> Result<(), String> {
        deliver(&self.jetstream, id, msg).await
    }
}

async fn deliver(js: &jetstream::Context, id: &str, msg: &Message) -> Result<(), String> {
    check_id(id)?;
    let payload = serde_json::to_vec(msg).map_err(|e| e.to_string())?;
    js.publish(format!("agent.{id}.in"), payload.into())
        .await
        .map_err(|e| e.to_string())?
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Collects everything agents publish into `inbox`, stamping each message with its sender. The
/// sender comes from the subject, which NATS only lets that agent publish on, so it cannot be
/// forged. Agents can publish nowhere else, so they cannot reach each other.
async fn route(client: async_nats::Client, js: jetstream::Context, idp: Arc<Idp>, inbox: tokio::sync::broadcast::Sender<Message>) {
    let Ok(mut sub) = client.subscribe("agent.*.out").await else { return };
    while let Some(raw) = sub.next().await {
        let Some(from) = raw.subject.split('.').nth(1).map(str::to_string) else { continue };
        if idp.live(&from).is_none() {
            continue;
        }
        let parsed = if raw.payload.len() > MAX_MESSAGE {
            Err("message too large".to_string())
        } else {
            serde_json::from_slice::<Outgoing>(&raw.payload).map_err(|e| format!("bad message: {e}"))
        };
        match parsed {
            Ok(out) => {
                let _ = inbox.send(Message { from, kind: out.kind, body: out.body });
            }
            Err(reason) => {
                let err = Message { from: "app".into(), kind: "error".into(), body: serde_json::json!({ "reason": reason }) };
                let _ = deliver(&js, &from, &err).await;
            }
        }
    }
}

/// The broker as Tauri state: `Starting` until `State::start` finishes, then running or failed.
#[derive(Default)]
pub struct State(std::sync::Mutex<Slot>);

#[derive(Default)]
enum Slot {
    #[default]
    Starting,
    Running(Arc<Broker>),
    Failed(String),
}

impl State {
    pub async fn start(&self, app: &tauri::AppHandle) {
        let dir = match app.path().app_local_data_dir() {
            Ok(d) => d.join("nats"),
            Err(e) => return self.set(Slot::Failed(e.to_string())),
        };
        let slot = match Broker::start(crate::nats::command(app), &dir).await {
            Ok(b) => Slot::Running(Arc::new(b)),
            Err(e) => {
                eprintln!("broker: {e}");
                Slot::Failed(e)
            }
        };
        self.set(slot);
    }

    fn set(&self, slot: Slot) {
        *self.0.lock().unwrap() = slot;
    }

    /// The running broker, for the rest of the backend.
    #[allow(dead_code)]
    pub fn get(&self) -> Result<Arc<Broker>, String> {
        match &*self.0.lock().unwrap() {
            Slot::Running(b) => Ok(b.clone()),
            Slot::Starting => Err("broker is still starting".into()),
            Slot::Failed(e) => Err(e.clone()),
        }
    }

    pub fn stop(&self) {
        self.set(Slot::Failed("stopped".into()));
    }
}

#[derive(Serialize)]
pub struct Status {
    state: &'static str,
    port: Option<u16>,
    error: Option<String>,
}

impl State {
    fn status(&self) -> Status {
        match &*self.0.lock().unwrap() {
            Slot::Starting => Status { state: "starting", port: None, error: None },
            Slot::Running(b) => Status { state: "running", port: Some(b.port), error: None },
            Slot::Failed(e) => Status { state: "failed", port: None, error: Some(e.clone()) },
        }
    }
}

#[tauri::command]
pub fn broker_status(state: tauri::State<State>) -> Status {
    state.status()
}

/// Stops the broker (dropping it kills `nats-server`) and starts a fresh one. Everything queued in
/// the old one is lost, as are the credentials handed out.
#[tauri::command]
pub async fn broker_restart(app: tauri::AppHandle, state: tauri::State<'_, State>) -> Result<Status, String> {
    state.set(Slot::Starting);
    state.start(&app).await;
    Ok(state.status())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TTL: Duration = Duration::from_secs(60);

    fn server() -> Option<Command> {
        let exe = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/nats/nats-server");
        exe.is_file().then(|| Command::new(exe))
    }

    async fn start(tag: &str) -> Option<(Broker, std::path::PathBuf)> {
        let Some(cmd) = server() else {
            eprintln!("nats-server not fetched, skipping (pnpm nats:fetch)");
            return None;
        };
        let dir = std::env::temp_dir().join(format!("ccaf-broker-{tag}-{}", std::process::id()));
        Some((Broker::start(cmd, &dir).await.unwrap(), dir))
    }

    /// Connects as the agent would: with the OIDC access token for NATS as its auth token.
    async fn connect(broker: &Broker, issued: &Issued) -> async_nats::Client {
        let token = broker.idp.mint(&issued.identity.id, AUDIENCE).unwrap().access_token;
        ConnectOptions::with_token(token).connect(format!("127.0.0.1:{}", broker.port)).await.unwrap()
    }

    /// Reads the next message from the agent's own queue, as `agentd` would.
    async fn next(client: &async_nats::Client, issued: &Issued) -> Option<Message> {
        let js = jetstream::new(client.clone());
        // An agent may only touch its own consumer, so it cannot look the stream up first.
        let cons: consumer::PullConsumer = js.get_consumer_from_stream(&issued.consumer, STREAM).await.unwrap();
        let mut batch = cons.fetch().max_messages(1).expires(Duration::from_secs(2)).messages().await.unwrap();
        let msg = batch.next().await?.unwrap();
        msg.ack().await.unwrap();
        Some(serde_json::from_slice(&msg.payload).unwrap())
    }

    async fn recv(rx: &mut tokio::sync::broadcast::Receiver<Message>) -> Option<Message> {
        tokio::time::timeout(Duration::from_secs(2), rx.recv()).await.ok()?.ok()
    }

    fn out(kind: &str) -> bytes::Bytes {
        json!({ "kind": kind, "body": { "n": 1 } }).to_string().into()
    }

    #[tokio::test]
    async fn agent_is_confined_to_its_own_queue() {
        let Some((broker, dir)) = start("confined").await else { return };
        let a = broker.allocate("proj", "sess", "developer", TTL).await.unwrap();
        let b = broker.allocate("proj", "sess", "developer", TTL).await.unwrap();

        // Queued before the agent connects, as when the VM is still booting.
        let task = Message { from: "app".into(), kind: "task".into(), body: json!("do it") };
        broker.send(&a.identity.id, &task).await.unwrap();
        broker.send(&b.identity.id, &task).await.unwrap();

        let client = connect(&broker, &a).await;
        assert_eq!(
            a.identity.name,
            format!("spiffe://ccagentflows.local/proj/sess/developer/agent-{}", a.identity.id)
        );
        assert!(broker.allocate("a/b", "sess", "developer", TTL).await.is_err(), "no path injection");
        let got = next(&client, &a).await.unwrap();
        assert_eq!((got.from.as_str(), got.kind.as_str()), ("app", "task"));
        assert!(next(&client, &a).await.is_none(), "must only see its own queue");

        // Another agent's consumer is off limits.
        let js = jetstream::new(client.clone());
        let other = js.get_consumer_from_stream::<consumer::pull::Config, _, _>(&b.consumer, STREAM).await;
        assert!(other.is_err());

        drop(broker);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn agents_only_reach_the_app() {
        let Some((broker, dir)) = start("inbox").await else { return };
        let a = broker.allocate("proj", "sess", "developer", TTL).await.unwrap();
        let b = broker.allocate("proj", "sess", "developer", TTL).await.unwrap();
        let a_c = connect(&broker, &a).await;
        let b_c = connect(&broker, &b).await;
        let mut inbox = broker.inbox.subscribe();
        // The sender is stamped by the router, not claimed by the agent.
        a_c.publish(format!("agent.{}.out", a.identity.id), out("report")).await.unwrap();
        let m = recv(&mut inbox).await.unwrap();
        assert_eq!((m.from, m.kind), (a.identity.id.clone(), "report".to_string()));

        // The app forwards it: the flow is the app's call.
        broker.send(&b.identity.id, &Message { from: "app".into(), kind: "handoff".into(), body: m.body }).await.unwrap();
        assert_eq!(next(&b_c, &b).await.unwrap().kind, "handoff");

        // Directly to another agent: refused by NATS, nothing arrives.
        a_c.publish(format!("agent.{}.in", b.identity.id), out("direct")).await.unwrap();
        a_c.flush().await.unwrap();
        assert!(next(&b_c, &b).await.is_none());

        // Speaking as another agent: refused too.
        a_c.publish(format!("agent.{}.out", b.identity.id), out("forged")).await.unwrap();
        a_c.flush().await.unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(500), inbox.recv()).await.is_err());

        // Garbage is answered with an error from the app.
        a_c.publish(format!("agent.{}.out", a.identity.id), "not json".into()).await.unwrap();
        let err = next(&a_c, &a).await.unwrap();
        assert_eq!((err.from.as_str(), err.kind.as_str()), ("app", "error"));

        drop(broker);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn only_valid_agent_tokens_get_in() {
        let Some((broker, dir)) = start("callout").await else { return };
        let a = broker.allocate("proj", "sess", "developer", TTL).await.unwrap();
        let url = format!("127.0.0.1:{}", broker.port);
        let attempt = |token: String| {
            let url = url.clone();
            async move { tokio::time::timeout(Duration::from_secs(3), ConnectOptions::with_token(token).connect(url)).await }
        };

        // A real token works, and its NATS session is no longer than SESSION_TTL.
        let token = broker.idp.mint(&a.identity.id, AUDIENCE).unwrap().access_token;
        assert!(matches!(attempt(token.clone()).await, Ok(Ok(_))));

        assert!(!matches!(attempt("nonsense".into()).await, Ok(Ok(_))), "garbage token");
        assert!(!matches!(attempt(String::new()).await, Ok(Ok(_))), "no token");
        let other_audience = broker.idp.mint(&a.identity.id, "git").unwrap().access_token;
        assert!(!matches!(attempt(other_audience).await, Ok(Ok(_))), "token meant for another service");

        // After release even a token that was still valid is refused.
        broker.release(&a.identity.id).await.unwrap();
        assert!(!matches!(attempt(token).await, Ok(Ok(_))), "released identity");

        drop(broker);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn release_withdraws_the_identity_everywhere() {
        let Some((broker, dir)) = start("release").await else { return };
        let a = broker.allocate("proj", "sess", "developer", TTL).await.unwrap();
        let token = broker.idp.mint(&a.identity.id, "proxy").unwrap().access_token;
        assert_eq!(broker.idp.verify(&token, "proxy").unwrap().id, a.identity.id);

        broker.release(&a.identity.id).await.unwrap();
        assert!(broker.idp.verify(&token, "proxy").is_err(), "released");
        assert!(broker.idp.mint(&a.identity.id, "proxy").is_err());

        drop(broker);
        let _ = std::fs::remove_dir_all(dir);
    }
}
