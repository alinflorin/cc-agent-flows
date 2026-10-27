//! The agent's connection to the app: NATS with tokens from the identity provider, the task queue
//! and everything `agentd` publishes.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_nats::jetstream::{self, consumer};
use async_nats::{Auth, AuthError, ConnectOptions};
use futures::StreamExt;
use protocol::{Bootstrap, Event, Final, Heartbeat, Inbound, MAX_MESSAGE, Outbound, Task};
use serde_json::{Value, json};

use crate::identity;

const STREAM: &str = "AGENTS";

pub struct Link {
    client: async_nats::Client,
    boot: Arc<Bootstrap>,
    seq: AtomicU64,
}

impl Link {
    pub async fn connect(boot: &Bootstrap) -> Result<Arc<Link>, String> {
        let boot = Arc::new(boot.clone());
        let for_auth = boot.clone();
        // Called on every (re)connect, so a session that ended after 15 minutes gets a fresh token.
        let opts = ConnectOptions::with_auth_callback(move |_nonce| {
            let boot = for_auth.clone();
            async move {
                let mut auth = Auth::new();
                auth.token = Some(identity::token(&boot, "nats").await.map_err(AuthError::new)?);
                Ok(auth)
            }
        })
        .retry_on_initial_connect();
        // Early on the VM may be up before its network is: retry for a while.
        let mut last = String::new();
        for _ in 0..30 {
            match opts.clone().connect(boot.nats_addr.as_str()).await {
                Ok(client) => return Ok(Arc::new(Link { client, boot, seq: AtomicU64::new(0) })),
                Err(e) => last = e.to_string(),
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Err(format!("could not reach NATS: {last}"))
    }

    fn subject(&self) -> String {
        format!("agent.{}.out", self.boot.id)
    }

    pub async fn send(&self, msg: &Outbound) -> Result<(), String> {
        let payload = serde_json::to_vec(msg).map_err(|e| e.to_string())?;
        if payload.len() > MAX_MESSAGE {
            return Err("message too large".into());
        }
        self.client.publish(self.subject(), payload.into()).await.map_err(|e| e.to_string())
    }

    /// Publishes one line of `claude` output. Too large for a message, it is cut to a stub.
    pub async fn event(&self, seq: u64, event: Value) -> Result<(), String> {
        let msg = Outbound::Event(Event { seq, event });
        let size = serde_json::to_vec(&msg).map(|v| v.len()).unwrap_or(usize::MAX);
        if size < MAX_MESSAGE / 2 {
            return self.send(&msg).await;
        }
        let head: String = serde_json::to_string(&match &msg {
            Outbound::Event(e) => &e.event,
            _ => unreachable!(),
        })
        .unwrap_or_default()
        .chars()
        .take(4096)
        .collect();
        self.send(&Outbound::Event(Event { seq, event: json!({ "truncated": true, "bytes": size, "head": head }) })).await
    }

    /// The first heartbeat goes out at once (it carries the protocol), then one every 10 seconds.
    pub fn start_heartbeat(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                let seq = me.seq.fetch_add(1, Ordering::Relaxed);
                let _ = me.send(&Outbound::Heartbeat(Heartbeat { protocol: protocol::PROTOCOL, seq })).await;
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        });
    }

    /// Waits for the task on the agent's own queue and acknowledges it. The agent cannot look up the
    /// stream, so the consumer is fetched directly.
    pub async fn next_task(&self, wait: Duration) -> Result<Task, String> {
        let js = jetstream::new(self.client.clone());
        let cons: consumer::PullConsumer =
            js.get_consumer_from_stream(&self.boot.consumer, STREAM).await.map_err(|e| format!("no task queue: {e}"))?;
        let deadline = tokio::time::Instant::now() + wait;
        while tokio::time::Instant::now() < deadline {
            let mut batch =
                cons.fetch().max_messages(1).expires(Duration::from_secs(5)).messages().await.map_err(|e| e.to_string())?;
            let Some(msg) = batch.next().await else { continue };
            let msg = msg.map_err(|e| e.to_string())?;
            msg.ack().await.map_err(|e| e.to_string())?;
            match serde_json::from_slice::<Inbound>(&msg.payload).map_err(|e| format!("bad task: {e}"))? {
                Inbound::Task(t) if t.protocol == protocol::PROTOCOL => return Ok(t),
                Inbound::Task(t) => return Err(format!("task speaks protocol {}, this agentd {}", t.protocol, protocol::PROTOCOL)),
                Inbound::Error { reason } => eprintln!("agentd: the app refused a message: {reason}"),
            }
        }
        Err("no task arrived".into())
    }

    /// Sends the final message and waits until it has left the connection.
    pub async fn finish(&self, fin: Final) -> Result<(), String> {
        self.send(&Outbound::Final(fin)).await?;
        self.client.flush().await.map_err(|e| e.to_string())
    }
}
