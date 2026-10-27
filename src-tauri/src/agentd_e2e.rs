//! Runs the real `agentd` binary against the real broker and identity provider, with a stand-in
//! for `claude`. Skipped if `nats-server` was not fetched (`pnpm nats:fetch`), and on Windows.

/// Needs a Unix shell for the stand-in `claude` and Unix file modes.
#[cfg(unix)]
mod e2e {
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    use protocol::{Bootstrap, Inbound, Task};
    use serde_json::json;

    use crate::broker::{Broker, Message};

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
        let nats = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/nats/nats-server");
        if !nats.is_file() {
            eprintln!("nats-server not fetched, skipping (pnpm nats:fetch)");
            return;
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates");
        let built = Command::new("cargo")
            .args(["build", "-q", "-p", "agentd"])
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(built.success());

        let dir = std::env::temp_dir().join(format!("ccaf-agentd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let broker = Broker::start(Command::new(nats), &dir.join("nats"))
            .await
            .unwrap();
        let issued = broker
            .allocate("proj", "sess", "developer", Duration::from_secs(120))
            .await
            .unwrap();

        let fake = dir.join("claude");
        std::fs::write(&fake, FAKE_CLAUDE).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let boot = Bootstrap {
            id: issued.identity.id.clone(),
            name: issued.identity.name.clone(),
            seed: issued.seed.clone(),
            consumer: issued.consumer.clone(),
            issuer: broker.idp.issuer().to_string(),
            idp_addr: broker
                .idp
                .issuer()
                .trim_start_matches("http://")
                .to_string(),
            nats_addr: format!("127.0.0.1:{}", broker.port),
        };
        let boot_file = dir.join("bootstrap");
        std::fs::write(
            &boot_file,
            format!("{}\n", serde_json::to_string(&boot).unwrap()),
        )
        .unwrap();

        // Queued before the agent starts, as when the VM is still booting.
        let task = Inbound::Task(Task {
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
            .send(
                &issued.identity.id,
                &Message {
                    from: "app".into(),
                    kind: "task".into(),
                    body: task["body"].clone(),
                },
            )
            .await
            .unwrap();

        let ws = dir.join("work");
        let mut agent = Command::new(root.join("target/debug/agentd"))
            .args([
                "--bootstrap",
                boot_file.to_str().unwrap(),
                "--workspace",
                ws.to_str().unwrap(),
                "--claude",
                fake.to_str().unwrap(),
            ])
            .spawn()
            .unwrap();

        let mut seen: Vec<Message> = vec![];
        let fin = loop {
            let m = tokio::time::timeout(Duration::from_secs(30), rx.recv())
                .await
                .expect("no final in time")
                .unwrap();
            assert_eq!(m.from, issued.identity.id);
            if m.kind == "final" {
                break m;
            }
            seen.push(m);
        };
        assert!(agent.wait().unwrap().success());

        assert_eq!(seen[0].kind, "heartbeat");
        let events: Vec<_> = seen.iter().filter(|m| m.kind == "event").collect();
        assert_eq!(events.len(), 3);
        assert_eq!(events[1].body["seq"], json!(2));
        assert_eq!(fin.body["status"], "needs_input");
        assert_eq!(fin.body["result"], "half done");
        assert_eq!(fin.body["questions"], json!(["which db?"]));
        assert_eq!(fin.body["session_id"], "sess-1");
        assert_eq!(fin.body["cost_usd"], json!(0.25));
        assert!(
            fin.body["workspace"].is_string(),
            "work comes back as a git bundle: {}",
            fin.body
        );
        assert!(ws.join("made-by-claude.txt").is_file());

        drop(broker);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Boots the real base image (fetched by `pnpm images:fetch`) with the bundled QEMU and runs a
    /// task through it. Claude gets an invalid token, so the run is expected to fail, but the
    /// whole path is exercised: boot, bootstrap over virtio-serial, identity provider and NATS
    /// through the guest's `10.0.2.2`, `claude` start, `final`, power off. Skipped without KVM, the
    /// image or QEMU.
    #[tokio::test]
    async fn base_image_boots_and_runs_a_task() {
        let res = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources");
        let qemu = res.join("qemu/bin/qemu-system-x86_64");
        let nats = res.join("nats/nats-server");
        let image = res.join("images/agent-base-x86_64.qcow2");
        if !(qemu.is_file() && nats.is_file() && image.is_file() && Path::new("/dev/kvm").exists()) {
            eprintln!("needs qemu, nats-server, the base image and /dev/kvm, skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ccaf-vm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let overlay = dir.join("overlay.qcow2");
        let made = Command::new(res.join("qemu/bin/qemu-img"))
            .args(["create", "-q", "-f", "qcow2", "-F", "qcow2", "-b"])
            .arg(&image)
            .arg(&overlay)
            .status()
            .unwrap();
        assert!(made.success());

        let broker = Broker::start(Command::new(nats), &dir.join("nats")).await.unwrap();
        let issued = broker.allocate("proj", "sess", "base", Duration::from_secs(600)).await.unwrap();
        let idp_port = broker.idp.issuer().rsplit(':').next().unwrap().to_string();
        let boot = Bootstrap {
            id: issued.identity.id.clone(),
            name: issued.identity.name.clone(),
            seed: issued.seed.clone(),
            consumer: issued.consumer.clone(),
            issuer: broker.idp.issuer().to_string(),
            idp_addr: format!("10.0.2.2:{idp_port}"),
            nats_addr: format!("10.0.2.2:{}", broker.port),
        };
        let task = Inbound::Task(Task {
            prompt: "say hi".into(),
            model: None,
            effort: None,
            claude_token: "invalid".into(),
            session_id: "00000000-0000-4000-8000-000000000001".into(),
            resume: false,
            bundle: None,
            schema: None,
            timeout_secs: 120,
        });
        let task = serde_json::to_value(task).unwrap();
        let mut rx = broker.inbox.subscribe();
        broker
            .send(&issued.identity.id, &Message { from: "app".into(), kind: "task".into(), body: task["body"].clone() })
            .await
            .unwrap();

        let sock = dir.join("bootstrap.sock");
        let mut vm = Command::new(&qemu)
            .arg("-L")
            .arg(res.join("qemu/share/qemu"))
            .args(["-machine", "q35", "-accel", "kvm", "-cpu", "host", "-m", "1024", "-smp", "2", "-display", "none"])
            .arg("-drive")
            .arg(format!("file={},if=virtio", overlay.display()))
            .args(["-nic", "user", "-device", "virtio-serial-pci", "-chardev"])
            .arg(format!("socket,id=bs,path={},server=on,wait=off", sock.display()))
            .args(["-device", "virtserialport,chardev=bs,name=bootstrap", "-serial"])
            .arg(format!("file:{}", dir.join("console.log").display()))
            .spawn()
            .unwrap();

        // The app's end of the one-shot channel: write the bootstrap message once QEMU listens.
        let line = format!("{}\n", serde_json::to_string(&boot).unwrap());
        let mut conn = None;
        for _ in 0..50 {
            if let Ok(c) = std::os::unix::net::UnixStream::connect(&sock) {
                conn = Some(c);
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        std::io::Write::write_all(&mut conn.expect("bootstrap socket"), line.as_bytes()).unwrap();

        let outcome = async {
            let mut kinds = vec![];
            loop {
                let m = tokio::time::timeout(Duration::from_secs(180), rx.recv()).await.ok()?.ok()?;
                assert_eq!(m.from, issued.identity.id);
                if m.kind == "final" {
                    return Some((kinds, m));
                }
                kinds.push(m.kind);
            }
        }
        .await;
        let _ = vm.kill();
        let _ = vm.wait();
        let console = std::fs::read_to_string(dir.join("console.log")).unwrap_or_default();
        let (kinds, fin) = outcome.unwrap_or_else(|| panic!("no final from the VM; console:\n{console}"));
        assert_eq!(kinds[0], "heartbeat");
        assert!(["done", "failed", "needs_input"].contains(&fin.body["status"].as_str().unwrap()), "{}", fin.body);

        drop(broker);
        let _ = std::fs::remove_dir_all(dir);
    }
}
