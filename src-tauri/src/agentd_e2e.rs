//! Runs the real `agentd.sh` (`resources/agentd/agentd.sh`) against the app's side of the channel
//! (`agent::drive`), with a stand-in for `claude`. The script's channel is two fifos here instead
//! of the virtio port. Linux only; skipped without `jq`.

#[cfg(target_os = "linux")]
mod e2e {
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command};
    use std::time::Duration;

    use tokio::net::unix::pipe;

    use crate::agent::drive;
    use crate::protocol::{Bootstrap, Final, Outbound, Status, Task, TaskEnv};

    fn boot() -> Bootstrap {
        Bootstrap { id: "spiffe://ccaf.local/test/agent-1".into(), claude_token: "sk-test-token".into() }
    }

    fn task(timeout_secs: u64) -> Task {
        Task {
            prompt: "do the thing".into(),
            model: Some("sonnet".into()),
            effort: Some("low".into()),
            session_id: "00000000-0000-4000-8000-000000000000".into(),
            resume: false,
            schema: crate::protocol::RESULT_SCHEMA.into(),
            system_prompt: Some("be brief".into()),
            env: vec![
                TaskEnv { name: "TEAM".into(), value: "it's \"red\" $HOME".into() },
                TaskEnv { name: "PATH".into(), value: "/nowhere".into() },
                TaskEnv { name: "BAD NAME".into(), value: "x".into() },
            ],
            timeout_secs,
        }
    }

    /// Pretends to be `claude`: checks how it was called and prints a stream-json run.
    const FAKE_CLAUDE: &str = r#"#!/bin/sh
case "$*" in *--dangerously-skip-permissions*--session-id*|*--session-id*--dangerously-skip-permissions*) ;; *) echo "bad args: $*" >&2; exit 2;; esac
[ "$CLAUDE_CODE_OAUTH_TOKEN" = "sk-test-token" ] || { echo "no token" >&2; exit 3; }
case "$*" in *"--append-system-prompt be brief"*) ;; *) echo "no system prompt: $*" >&2; exit 4;; esac
[ "$TEAM" = 'it'"'"'s "red" $HOME' ] || { echo "bad env: $TEAM" >&2; exit 5; }
[ "$PATH" != /nowhere ] || { echo "PATH was overridden" >&2; exit 6; }
[ "$CCAF_AGENT_ID" = "spiffe://ccaf.local/test/agent-1" ] || { echo "no identity" >&2; exit 7; }
[ -z "$(env | grep -E '^(BAD|TOKEN=)')" ] || { echo "stray env" >&2; exit 8; }
echo '{"type":"system","subtype":"init"}'
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"working \"quoted\" ünï"}]}}'
echo 'not json at all'
echo "hello" > made-by-claude.txt
echo '{"type":"result","is_error":false,"result":"free text","structured_output":{"status":"needs_input","summary":"half done","questions":["which db?"]},"total_cost_usd":0.25,"session_id":"sess-1"}'
"#;

    /// Never finishes: the script has to kill it, and the process it leaves behind.
    const HANGING_CLAUDE: &str = "#!/bin/sh\necho '{\"type\":\"system\"}'\nsleep 600 &\nsleep 600\n";

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccaf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn have_jq() -> bool {
        Command::new("jq").arg("--version").output().is_ok_and(|o| o.status.success())
    }

    fn shell(script: &str) -> String {
        let out = Command::new("sh").arg("-c").arg(script).output().unwrap();
        assert!(out.status.success(), "{script}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    struct Run {
        dir: PathBuf,
        agent: Child,
        rx: tokio::sync::mpsc::Receiver<Outbound>,
    }

    /// Starts the script on two fifos and `drive`s the app's side.
    fn start(tag: &str, claude: &str, task: Task) -> Run {
        let dir = temp(tag);
        let fake = dir.join("claude");
        std::fs::write(&fake, claude).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let (to_agent, from_agent) = (dir.join("to-agent"), dir.join("from-agent"));
        shell(&format!("mkfifo '{}' '{}'", to_agent.display(), from_agent.display()));
        // Read-write on both ends, so neither open waits for the other side.
        let rx_pipe = pipe::OpenOptions::new().read_write(true).open_receiver(&from_agent).unwrap();
        let tx_pipe = pipe::OpenOptions::new().read_write(true).open_sender(&to_agent).unwrap();
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/agentd/agentd.sh");
        let agent = Command::new("sh")
            .arg(script)
            .args(["--channel", to_agent.to_str().unwrap(), "--out", from_agent.to_str().unwrap()])
            .args(["--workspace", dir.join("work").to_str().unwrap(), "--claude", fake.to_str().unwrap()])
            .spawn()
            .unwrap();
        let rx = drive(tokio::io::join(rx_pipe, tx_pipe), boot(), task);
        Run { dir, agent, rx }
    }

    impl Run {
        async fn finish(&mut self, secs: u64) -> (Vec<Outbound>, Final) {
            let mut seen = vec![];
            let fin = loop {
                match tokio::time::timeout(Duration::from_secs(secs), self.rx.recv()).await.expect("no final in time").expect("channel closed") {
                    Outbound::Final(f) => break f,
                    other => seen.push(other),
                }
            };
            assert!(self.agent.wait().unwrap().success());
            (seen, fin)
        }
    }

    #[tokio::test]
    async fn agentd_runs_a_task_end_to_end() {
        if !have_jq() {
            eprintln!("needs jq, skipping");
            return;
        }
        let mut run = start("agentd", FAKE_CLAUDE, task(40));
        let (seen, fin) = run.finish(30).await;

        let events: Vec<_> = seen.iter().filter_map(|m| if let Outbound::Event(e) = m { Some(e) } else { None }).collect();
        assert_eq!(events.len(), 4);
        assert_eq!(seen.len(), 4, "events are all there is before the final");
        assert_eq!(events[1].seq, 2);
        assert_eq!(events[1].event["message"]["content"][0]["text"], "working \"quoted\" ünï");
        assert_eq!(events[2].event["type"], "raw", "a line that is not JSON is wrapped");
        assert_eq!(fin.status, Status::NeedsInput);
        assert_eq!(fin.result, "half done");
        assert_eq!(fin.questions, ["which db?"]);
        assert_eq!(fin.session_id.as_deref(), Some("sess-1"));
        assert_eq!(fin.cost_usd, Some(0.25));
        assert_eq!(fin.exit_code, Some(0));
        assert_eq!(fin.output.as_ref().unwrap()["summary"], "half done", "the structured answer is passed on");
        assert!(run.dir.join("work/made-by-claude.txt").is_file(), "claude ran in the workspace");

        let _ = std::fs::remove_dir_all(run.dir);
    }

    #[tokio::test]
    async fn agentd_kills_a_run_that_takes_too_long() {
        if !have_jq() {
            eprintln!("needs jq, skipping");
            return;
        }
        let mut run = start("agentd-timeout", HANGING_CLAUDE, task(3));
        let (_, fin) = run.finish(30).await;
        assert_eq!(fin.status, Status::Failed);
        assert_eq!(fin.result, "timed out after 3s");
        let _ = std::fs::remove_dir_all(run.dir);
    }
}
