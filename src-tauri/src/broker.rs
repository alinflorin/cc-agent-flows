//! The NATS broker the app runs for agent communication (see "Agent communication" in CLAUDE.md).
//!
//! Auth is NATS operator mode with JWTs minted in memory, so agent credentials can be created and
//! scoped per agent without ever reloading the server. The operator and account keys are generated
//! on each start and the JetStream store is wiped with them: nothing here outlives the app run.

// Nothing drives runs yet; the pieces below are exercised by the tests only.
#![allow(dead_code)]

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_nats::jetstream::{self, consumer, stream};
use async_nats::{AuthError, ConnectOptions};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use nkeys::KeyPair;
use serde::Serialize;
use serde_json::{Value, json};
use tauri::Manager;

const STREAM: &str = "AGENTS";
/// How long an agent's credentials stay valid. A run is far shorter; this only bounds a leak.
const AGENT_TTL: Duration = Duration::from_secs(24 * 3600);

/// What an agent needs to connect, besides the broker address.
#[derive(Serialize)]
pub struct AgentCreds {
    pub jwt: String,
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

pub struct Broker {
    _server: ServerProcess,
    pub port: u16,
    account: KeyPair,
    pub client: async_nats::Client,
    pub jetstream: jetstream::Context,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn encode_jwt(issuer: &KeyPair, subject: &str, name: &str, nats: Value, ttl: Option<Duration>) -> Result<String, String> {
    let iat = now();
    let mut claims = json!({ "iat": iat, "iss": issuer.public_key(), "name": name, "sub": subject, "nats": nats });
    if let Some(ttl) = ttl {
        claims["exp"] = json!(iat + ttl.as_secs());
    }
    let header = B64.encode(r#"{"typ":"JWT","alg":"ed25519-nkey"}"#);
    let payload = B64.encode(serde_json::to_vec(&claims).map_err(|e| e.to_string())?);
    let input = format!("{header}.{payload}");
    let sig = issuer.sign(input.as_bytes()).map_err(|e| e.to_string())?;
    Ok(format!("{input}.{}", B64.encode(sig)))
}

fn user_jwt(account: &KeyPair, user: &KeyPair, name: &str, publish: &[String], subscribe: &[String]) -> Result<String, String> {
    let nats = json!({
        "type": "user", "version": 2,
        "pub": { "allow": publish }, "sub": { "allow": subscribe },
        "subs": -1, "data": -1, "payload": -1,
    });
    encode_jwt(account, &user.public_key(), name, nats, Some(AGENT_TTL))
}

/// Agent ids end up in subjects and consumer names, so keep them to a safe alphabet.
fn check_id(id: &str) -> Result<(), String> {
    let ok = !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok { Ok(()) } else { Err(format!("invalid agent id: {id:?}")) }
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
        let operator = KeyPair::new_operator();
        let account = KeyPair::new_account();
        // JetStream refuses to start without a system account, though nothing connects to it.
        let system = KeyPair::new_account();

        let operator_jwt = encode_jwt(&operator, &operator.public_key(), "app", json!({ "type": "operator", "version": 2 }), None)?;
        let system_jwt = encode_jwt(&operator, &system.public_key(), "system", json!({ "type": "account", "version": 2 }), None)?;
        let account_jwt = encode_jwt(
            &operator,
            &account.public_key(),
            "agents",
            json!({
                "type": "account", "version": 2,
                "limits": {
                    "subs": -1, "data": -1, "payload": -1, "imports": -1, "exports": -1, "wildcards": true,
                    "conn": -1, "leaf": -1, "mem_storage": -1, "disk_storage": -1, "streams": -1, "consumer": -1,
                },
            }),
            None,
        )?;

        let _ = std::fs::remove_dir_all(dir);
        let store = dir.join("jetstream");
        std::fs::create_dir_all(&store).map_err(|e| e.to_string())?;
        let conf = format!(
            "listen: 127.0.0.1:{port}\noperator: {}\nresolver: MEMORY\nsystem_account: {}\nresolver_preload: {{ {}: {}, {}: {} }}\njetstream: {{ store_dir: {} }}\n",
            quoted(&operator_jwt),
            system.public_key(),
            account.public_key(),
            quoted(&account_jwt),
            system.public_key(),
            quoted(&system_jwt),
            quoted(&store.to_string_lossy()),
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

        let app_user = Arc::new(KeyPair::new_user());
        let everything = vec![">".to_string()];
        let jwt = user_jwt(&account, &app_user, "app", &everything, &everything)?;
        let mut last = String::new();
        for _ in 0..50 {
            let signer = app_user.clone();
            let opts = ConnectOptions::with_jwt(jwt.clone(), move |nonce| {
                let signer = signer.clone();
                async move { signer.sign(&nonce).map_err(AuthError::new) }
            });
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
                    return Ok(Broker { _server: server, port, account, client, jetstream: js });
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

    /// Creates the agent's task queue and credentials that can touch nothing else: it reads
    /// `agent.<id>.in` through its own consumer and writes `agent.<id>.out`.
    pub async fn prepare_agent(&self, id: &str) -> Result<AgentCreds, String> {
        check_id(id)?;
        let name = format!("in-{id}");
        self.jetstream
            .get_stream(STREAM)
            .await
            .map_err(|e| e.to_string())?
            .get_or_create_consumer(
                &name,
                consumer::pull::Config {
                    durable_name: Some(name.clone()),
                    filter_subject: format!("agent.{id}.in"),
                    ack_policy: consumer::AckPolicy::Explicit,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| format!("could not create consumer: {e}"))?;

        let user = KeyPair::new_user();
        let publish = vec![
            format!("agent.{id}.out"),
            format!("$JS.API.CONSUMER.INFO.{STREAM}.{name}"),
            format!("$JS.API.CONSUMER.MSG.NEXT.{STREAM}.{name}"),
            format!("$JS.ACK.{STREAM}.{name}.>"),
        ];
        let jwt = user_jwt(&self.account, &user, &format!("agent-{id}"), &publish, &["_INBOX.>".to_string()])?;
        Ok(AgentCreds { jwt, seed: user.seed().map_err(|e| e.to_string())?, consumer: name })
    }

    /// Queues a task for the agent; it waits in the stream until the agent consumes it.
    pub async fn publish_task(&self, id: &str, payload: Vec<u8>) -> Result<(), String> {
        check_id(id)?;
        self.jetstream
            .publish(format!("agent.{id}.in"), payload.into())
            .await
            .map_err(|e| e.to_string())?
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
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
    use futures::StreamExt;

    fn server() -> Option<Command> {
        let exe = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/nats/nats-server");
        exe.is_file().then(|| Command::new(exe))
    }

    async fn connect(port: u16, c: &AgentCreds) -> async_nats::Client {
        let kp = Arc::new(KeyPair::from_seed(&c.seed).unwrap());
        ConnectOptions::with_jwt(c.jwt.clone(), move |nonce| {
            let kp = kp.clone();
            async move { kp.sign(&nonce).map_err(AuthError::new) }
        })
        .connect(format!("127.0.0.1:{port}"))
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn agent_gets_its_task_and_nothing_else() {
        let Some(cmd) = server() else {
            eprintln!("nats-server not fetched, skipping (pnpm nats:fetch)");
            return;
        };
        let dir = std::env::temp_dir().join(format!("ccaf-broker-test-{}", std::process::id()));
        let broker = Broker::start(cmd, &dir).await.unwrap();

        // The task is queued before the agent exists, as when the VM is still booting.
        let a = broker.prepare_agent("a1").await.unwrap();
        let b = broker.prepare_agent("b2").await.unwrap();
        broker.publish_task("a1", b"do it".to_vec()).await.unwrap();
        broker.publish_task("b2", b"secret".to_vec()).await.unwrap();

        let client = connect(broker.port, &a).await;
        // An agent may only touch its own consumer, so it cannot look the stream up first.
        let js = jetstream::new(client.clone());
        let cons: consumer::PullConsumer = js.get_consumer_from_stream(&a.consumer, STREAM).await.unwrap();
        let mut batch = cons.fetch().max_messages(5).expires(Duration::from_secs(2)).messages().await.unwrap();
        let msg = batch.next().await.unwrap().unwrap();
        assert_eq!(&msg.payload[..], b"do it");
        msg.ack().await.unwrap();
        assert!(batch.next().await.is_none(), "agent a1 must only see its own queue");

        // Replying on its own subject works and reaches the app.
        let mut out = broker.client.subscribe("agent.a1.out").await.unwrap();
        broker.client.flush().await.unwrap();
        client.publish("agent.a1.out", "done".into()).await.unwrap();
        client.flush().await.unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(2), out.next()).await.unwrap().unwrap();
        assert_eq!(&reply.payload[..], b"done");

        // Another agent's queue is off limits.
        let other = js.get_consumer_from_stream::<consumer::pull::Config, _, _>(&b.consumer, STREAM).await;
        assert!(other.is_err(), "agent a1 must not reach b2's consumer");

        drop(broker);
        let _ = std::fs::remove_dir_all(dir);
    }
}
