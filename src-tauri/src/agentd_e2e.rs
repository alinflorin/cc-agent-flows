//! Runs the real `agentd` binary against the real broker and identity provider, with a stand-in
//! for `claude`. Skipped if `nats-server` was not fetched (`pnpm nats:fetch`), and on Windows.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use protocol::{Bootstrap, Inbound, PROTOCOL, Task};
use serde_json::{Value, json};

use crate::broker::{Broker, Message};

#[test]
fn protocol_matches_the_image_version() {
    let raw = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/images/version.json")).unwrap();
    let v: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["protocol"].as_u64(), Some(PROTOCOL as u64), "bump scripts/images/version.json together with PROTOCOL");
}

/// Pretends to be `claude`: checks how it was called and prints a stream-json run.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
case "$*" in *--dangerously-skip-permissions*--session-id*|*--session-id*--dangerously-skip-permissions*) ;; *) echo "bad args: $*" >&2; exit 2;; esac
[ "$CLAUDE_CODE_OAUTH_TOKEN" = "sk-test-token" ] || { echo "no token" >&2; exit 3; }
[ -z "$NATS_TOKEN" ] || exit 4
echo '{"type":"system","subtype":"init"}'
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"working"}]}}'
echo "hello" > made-by-claude.txt
echo '{"type":"result","is_error":false,"result":"free text","structured_output":{"status":"needs_input","summary":"half done","questions":["which db?"]},"total_cost_usd":0.25,"session_id":"sess-1"}'
"#;

#[tokio::test]
async fn agentd_runs_a_task_end_to_end() {
    if cfg!(windows) {
        return;
    }
    let nats = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/nats/nats-server");
    if !nats.is_file() {
        eprintln!("nats-server not fetched, skipping (pnpm nats:fetch)");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates");
    let built = Command::new("cargo").args(["build", "-q", "-p", "agentd"]).current_dir(&root).status().unwrap();
    assert!(built.success());

    let dir = std::env::temp_dir().join(format!("ccaf-agentd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let broker = Broker::start(Command::new(nats), &dir.join("nats")).await.unwrap();
    let issued = broker.allocate("proj", "sess", "developer", Duration::from_secs(120)).await.unwrap();

    let fake = dir.join("claude");
    std::fs::write(&fake, FAKE_CLAUDE).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let boot = Bootstrap {
        protocol: PROTOCOL,
        id: issued.identity.id.clone(),
        name: issued.identity.name.clone(),
        seed: issued.seed.clone(),
        consumer: issued.consumer.clone(),
        issuer: broker.idp.issuer().to_string(),
        idp_addr: broker.idp.issuer().trim_start_matches("http://").to_string(),
        nats_addr: format!("127.0.0.1:{}", broker.port),
    };
    let boot_file = dir.join("bootstrap");
    std::fs::write(&boot_file, format!("{}\n", serde_json::to_string(&boot).unwrap())).unwrap();

    // Queued before the agent starts, as when the VM is still booting.
    let task = Inbound::Task(Task {
        protocol: PROTOCOL,
        prompt: "do the thing".into(),
        model: Some("sonnet".into()),
        effort: Some("low".into()),
        claude_token: "sk-test-token".into(),
        session_id: "00000000-0000-4000-8000-000000000000".into(),
        resume: false,
        bundle: None,
        schema: None,
        timeout_secs: 30,
    });
    let task = serde_json::to_value(task).unwrap();
    let mut rx = broker.inbox.subscribe();
    broker
        .send(&issued.identity.id, &Message { from: "app".into(), kind: "task".into(), body: task["body"].clone() })
        .await
        .unwrap();

    let ws = dir.join("work");
    let mut agent = Command::new(root.join("target/debug/agentd"))
        .args(["--bootstrap", boot_file.to_str().unwrap(), "--workspace", ws.to_str().unwrap(), "--claude", fake.to_str().unwrap()])
        .spawn()
        .unwrap();

    let mut seen: Vec<Message> = vec![];
    let fin = loop {
        let m = tokio::time::timeout(Duration::from_secs(30), rx.recv()).await.expect("no final in time").unwrap();
        assert_eq!(m.from, issued.identity.id);
        if m.kind == "final" {
            break m;
        }
        seen.push(m);
    };
    assert!(agent.wait().unwrap().success());

    assert_eq!(seen[0].kind, "heartbeat");
    assert_eq!(seen[0].body["protocol"], json!(PROTOCOL), "first message reports the protocol");
    let events: Vec<_> = seen.iter().filter(|m| m.kind == "event").collect();
    assert_eq!(events.len(), 3);
    assert_eq!(events[1].body["seq"], json!(2));
    assert_eq!(fin.body["status"], "needs_input");
    assert_eq!(fin.body["result"], "half done");
    assert_eq!(fin.body["questions"], json!(["which db?"]));
    assert_eq!(fin.body["session_id"], "sess-1");
    assert_eq!(fin.body["cost_usd"], json!(0.25));
    assert!(fin.body["workspace"].is_string(), "work comes back as a git bundle: {}", fin.body);
    assert!(ws.join("made-by-claude.txt").is_file());

    drop(broker);
    let _ = std::fs::remove_dir_all(dir);
}
