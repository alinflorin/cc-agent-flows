//! Runs the real `agentd` against the app's side of the channel (`agent::drive`), first on a unix
//! socket with a stand-in for `claude`, then inside the real base image under QEMU. Unix only.

#[cfg(unix)]
mod e2e {
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    use protocol::{Bootstrap, Outbound, Status, Task};

    use crate::agent::drive;

    fn boot() -> Bootstrap {
        Bootstrap { id: "agent-1".into(), claude_token: "sk-test-token".into() }
    }

    fn task() -> Task {
        Task {
            prompt: "do the thing".into(),
            model: Some("sonnet".into()),
            effort: Some("low".into()),
            session_id: "00000000-0000-4000-8000-000000000000".into(),
            resume: false,
            bundle: None,
            schema: None,
            timeout_secs: 40,
        }
    }

    /// Pretends to be `claude`: checks how it was called and prints a stream-json run.
    const FAKE_CLAUDE: &str = r#"#!/bin/sh
case "$*" in *--dangerously-skip-permissions*--session-id*|*--session-id*--dangerously-skip-permissions*) ;; *) echo "bad args: $*" >&2; exit 2;; esac
[ "$CLAUDE_CODE_OAUTH_TOKEN" = "sk-test-token" ] || { echo "no token" >&2; exit 3; }
echo '{"type":"system","subtype":"init"}'
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"working"}]}}'
echo "hello" > made-by-claude.txt
echo '{"type":"result","is_error":false,"result":"free text","structured_output":{"status":"needs_input","summary":"half done","questions":["which db?"]},"total_cost_usd":0.25,"session_id":"sess-1"}'
"#;

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ccaf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn agentd() -> std::path::PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates");
        let built = Command::new("cargo").args(["build", "-q", "-p", "agentd"]).current_dir(&root).status().unwrap();
        assert!(built.success());
        root.join("target/debug/agentd")
    }

    #[tokio::test]
    async fn agentd_runs_a_task_end_to_end() {
        let dir = temp("agentd");
        let fake = dir.join("claude");
        std::fs::write(&fake, FAKE_CLAUDE).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let sock = dir.join("agent.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let ws = dir.join("work");
        let mut agent = Command::new(agentd())
            .args(["--channel", &format!("unix:{}", sock.display()), "--workspace", ws.to_str().unwrap(), "--claude", fake.to_str().unwrap()])
            .spawn()
            .unwrap();

        let (stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept()).await.unwrap().unwrap();
        let mut rx = drive(stream, boot(), task());
        let mut seen = vec![];
        let fin = loop {
            match tokio::time::timeout(Duration::from_secs(30), rx.recv()).await.expect("no final in time").expect("channel closed") {
                Outbound::Final(f) => break f,
                other => seen.push(other),
            }
        };
        assert!(agent.wait().unwrap().success());

        let events: Vec<_> = seen.iter().filter_map(|m| if let Outbound::Event(e) = m { Some(e) } else { None }).collect();
        assert_eq!(events.len(), 3);
        assert_eq!(events[1].seq, 2);
        assert_eq!(fin.status, Status::NeedsInput);
        assert_eq!(fin.result, "half done");
        assert_eq!(fin.questions, ["which db?"]);
        assert_eq!(fin.session_id.as_deref(), Some("sess-1"));
        assert_eq!(fin.cost_usd, Some(0.25));
        assert!(fin.workspace.is_some(), "work comes back as a git bundle");
        assert!(ws.join("made-by-claude.txt").is_file());

        let _ = std::fs::remove_dir_all(dir);
    }

    /// Boots a base image with `agentd` in it (the plain one from `pnpm images:fetch` has none, so this
    /// is skipped until the app builds that base) with the bundled QEMU and no
    /// network device at all, and runs a task through it over virtio-serial. Claude gets an invalid
    /// token and cannot reach its API, so the run is expected to fail, but the whole path is
    /// exercised: boot, bootstrap, ready, task, `claude` start, `final`, power off. Skipped without
    /// KVM, the image or QEMU.
    #[tokio::test]
    async fn base_image_boots_and_runs_a_task() {
        let res = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources");
        let qemu = res.join("qemu/bin/qemu-system-x86_64");
        let image = res.join("images/agent-base-x86_64.qcow2");
        if !(qemu.is_file() && image.is_file() && Path::new("/dev/kvm").exists()) {
            eprintln!("needs qemu, the base image and /dev/kvm, skipping");
            return;
        }
        let dir = temp("vm");
        let overlay = dir.join("overlay.qcow2");
        let made = Command::new(res.join("qemu/bin/qemu-img"))
            .args(["create", "-q", "-f", "qcow2", "-F", "qcow2", "-b"])
            .arg(&image)
            .arg(&overlay)
            .status()
            .unwrap();
        assert!(made.success());

        let sock = dir.join("agent.sock");
        let mut vm = Command::new(&qemu)
            .arg("-L")
            .arg(res.join("qemu/share/qemu"))
            .args(["-machine", "q35", "-accel", "kvm", "-cpu", "host", "-m", "1024", "-smp", "2", "-display", "none", "-nic", "none"])
            .arg("-drive")
            .arg(format!("file={},if=virtio", overlay.display()))
            .args(["-device", "virtio-serial-pci", "-chardev"])
            .arg(format!("socket,id=agent,path={},server=on,wait=off", sock.display()))
            .args(["-device", "virtserialport,chardev=agent,name=agent", "-serial"])
            .arg(format!("file:{}", dir.join("console.log").display()))
            .spawn()
            .unwrap();

        let mut stream = None;
        for _ in 0..50 {
            if let Ok(s) = tokio::net::UnixStream::connect(&sock).await {
                stream = Some(s);
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let mut rx = drive(stream.expect("agent socket"), boot(), task());

        let outcome = async {
            let mut kinds = vec![];
            loop {
                match tokio::time::timeout(Duration::from_secs(120), rx.recv()).await.ok()?? {
                    Outbound::Final(f) => return Some((kinds, f)),
                    Outbound::Heartbeat(_) => kinds.push("heartbeat"),
                    Outbound::Event(_) => kinds.push("event"),
                    Outbound::Ready => {}
                }
            }
        }
        .await;
        let _ = vm.kill();
        let _ = vm.wait();
        let console = std::fs::read_to_string(dir.join("console.log")).unwrap_or_default();
        let (kinds, fin) = outcome.unwrap_or_else(|| panic!("no final from the VM; console:\n{console}"));
        assert_eq!(kinds.first(), Some(&"heartbeat"));
        assert!(matches!(fin.status, Status::Done | Status::Failed | Status::NeedsInput));

        let _ = std::fs::remove_dir_all(dir);
    }
}
