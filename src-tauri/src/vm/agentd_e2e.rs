//! Runs the real `agentd.sh` (`resources/agentd/agentd.sh`) against the app's side of the channel
//! (`agent::drive`), with a stand-in for `claude`. The script's channel is two fifos here instead
//! of the virtio port. Linux only; skipped without `jq`.

#[cfg(target_os = "linux")]
mod e2e {
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command};
    use std::time::Duration;

    use tokio::net::unix::pipe;

    use crate::vm::agent::drive;
    use crate::vm::protocol::{Bootstrap, Final, Outbound, Status, Task, TaskEnv, TunnelKeys};

    fn boot() -> Bootstrap {
        Bootstrap { id: "spiffe://ccaf.local/test/agent-1".into(), claude_token: "sk-test-token".into(), tunnel: None, ca: None }
    }

    fn task(timeout_secs: u64) -> Task {
        Task {
            prompt: "do the thing".into(),
            model: Some("sonnet".into()),
            effort: Some("low".into()),
            session_id: "00000000-0000-4000-8000-000000000000".into(),
            resume: false,
            schema: crate::vm::protocol::RESULT_SCHEMA.into(),
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
case "$*" in *"--tools Bash,Read,Write,Edit,WebFetch,WebSearch"*) ;; *) echo "bad tools: $*" >&2; exit 9;; esac
case "$*" in *"--append-system-prompt be brief"*) ;; *) echo "no system prompt: $*" >&2; exit 4;; esac
[ "$TEAM" = 'it'"'"'s "red" $HOME' ] || { echo "bad env: $TEAM" >&2; exit 5; }
[ "$PATH" != /nowhere ] || { echo "PATH was overridden" >&2; exit 6; }
[ "$CCAF_AGENT_ID" = "spiffe://ccaf.local/test/agent-1" ] || { echo "no identity" >&2; exit 7; }
[ -z "$(env | grep -E '^(BAD|TOKEN=)')" ] || { echo "stray env" >&2; exit 8; }
echo '{"type":"system","subtype":"init"}'
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"working \"quoted\" ünï"}]}}'
echo 'not json at all'
echo "hello" > made-by-claude.txt
echo '{"type":"result","is_error":false,"result":"free text\\n{\"status\":\"needs_input\",\"summary\":\"half done\",\"questions\":[\"which db?\"]}","usage":{"input_tokens":100,"cache_creation_input_tokens":50,"cache_read_input_tokens":25,"output_tokens":75},"session_id":"sess-1"}'
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
        start_with(tag, claude, task, boot(), None)
    }

    fn executable(path: &Path, text: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// `ssh` is a stand-in script when given.
    fn start_with(tag: &str, claude: &str, task: Task, boot: Bootstrap, ssh: Option<&str>) -> Run {
        start_full(tag, claude, task, boot, ssh, &[])
    }

    /// `tools` are files of the image's tools directory.
    fn start_full(tag: &str, claude: &str, task: Task, boot: Bootstrap, ssh: Option<&str>, tools: &[(&str, &str)]) -> Run {
        let dir = temp(tag);
        std::fs::create_dir_all(dir.join("tools")).unwrap();
        for (name, text) in tools {
            std::fs::write(dir.join("tools").join(name), text).unwrap();
        }
        let fake = dir.join("claude");
        executable(&fake, claude);
        let fake_ssh = dir.join("ssh");
        executable(&fake_ssh, ssh.unwrap_or("#!/bin/sh\nexit 99\n"));
        let (to_agent, from_agent) = (dir.join("to-agent"), dir.join("from-agent"));
        shell(&format!("mkfifo '{}' '{}'", to_agent.display(), from_agent.display()));
        // Read-write on both ends, so neither open waits for the other side.
        let rx_pipe = pipe::OpenOptions::new().read_write(true).open_receiver(&from_agent).unwrap();
        let tx_pipe = pipe::OpenOptions::new().read_write(true).open_sender(&to_agent).unwrap();
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/agentd/agentd.sh");
        let agent = Command::new("sh")
            .arg(script)
            .args(["--channel", to_agent.to_str().unwrap(), "--out", from_agent.to_str().unwrap()])
            .args(["--workspace", dir.join("work").to_str().unwrap(), "--claude", fake.to_str().unwrap(), "--ssh", fake_ssh.to_str().unwrap()])
            .args(["--ca-bundle", dir.join("bundle.crt").to_str().unwrap(), "--ca-file", dir.join("ca.pem").to_str().unwrap()])
            .args(["--tools-dir", dir.join("tools").to_str().unwrap()])
            .spawn()
            .unwrap();
        let rx = drive(tokio::io::join(rx_pipe, tx_pipe), boot, task);
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

        // agentd's own timing marks (seq 0) are not claude's output
        let timing = seen.iter().filter(|m| matches!(m, Outbound::Event(e) if e.event["type"] == "timing")).count();
        assert!(timing >= 3, "agentd reports where the time goes");
        let events: Vec<_> = seen.iter().filter_map(|m| if let Outbound::Event(e) = m { Some(e) } else { None }).filter(|e| e.event["type"] != "timing").collect();
        assert_eq!(events.len(), 4);
        assert_eq!(seen.len(), 4 + timing, "events are all there is before the final");
        assert_eq!(events[1].seq, 2);
        assert_eq!(events[1].event["message"]["content"][0]["text"], "working \"quoted\" ünï");
        assert_eq!(events[2].event["type"], "raw", "a line that is not JSON is wrapped");
        assert_eq!(events[2].event["text"], "not json at all", "and keeps its text");
        assert_eq!(fin.status, Status::NeedsInput);
        assert_eq!(fin.result, "half done");
        assert_eq!(fin.questions, ["which db?"]);
        assert_eq!(fin.session_id.as_deref(), Some("sess-1"));
        assert_eq!(fin.tokens, Some(250));
        assert_eq!(fin.exit_code, Some(0));
        assert_eq!(fin.output.as_ref().unwrap()["summary"], "half done", "the structured answer is passed on");
        assert!(run.dir.join("work/made-by-claude.txt").is_file(), "claude ran in the workspace");

        let _ = std::fs::remove_dir_all(run.dir);
    }

    /// `claude` that checks the tool notes come last in the system prompt, in file order.
    const TOOLS_CLAUDE: &str = r#"#!/bin/sh
while [ $# -gt 0 ]; do [ "$1" = --append-system-prompt ] && sp=$2; shift; done
expected="be brief

# Command-line tools in this environment

## jira
first

## second
second"
# agentd ends the system prompt with the instruction to answer in JSON, after the notes.
case "$sp" in "$expected"*Final?answer*) ;; *) echo "bad prompt: $sp" >&2; exit 2;; esac
true
echo '{"type":"result","is_error":false,"result":"ok","session_id":"s"}'
"#;

    #[tokio::test]
    async fn agentd_appends_the_images_tool_notes_to_the_system_prompt() {
        if !have_jq() {
            eprintln!("needs jq, skipping");
            return;
        }
        let tools = [("002-b.md", "## second\nsecond\n\n"), ("001-a.md", "## jira\nfirst\n\n"), ("x.txt", "not a note")];
        let mut run = start_full("agentd-tools", TOOLS_CLAUDE, task(40), boot(), None, &tools);
        let (_, fin) = run.finish(30).await;
        assert_eq!(fin.status, Status::Done, "{}", fin.result);
        let _ = std::fs::remove_dir_all(run.dir);
    }

    /// Stands in for `ssh`: checks how the tunnel is asked for and that the key is there while it
    /// is read, leaves a note of the arguments.
    const FAKE_SSH: &str = r#"#!/bin/sh
echo "$@" > "$(dirname "$0")/ssh-args"
for a in "$@"; do case $a in -i) next=1 ;; *) [ "${next:-0}" = 1 ] && key=$a; next=0 ;; esac; done
[ "$(head -n1 "$key")" = "-----BEGIN FAKE KEY-----" ] || { echo "no key at $key" >&2; exit 11; }
[ "$(stat -c %a "$key")" = 600 ] || { echo "key is not private" >&2; exit 12; }
hosts=$(echo "$@" | sed -n 's/.*UserKnownHostsFile=\([^ ]*\).*/\1/p')
[ "$(cat "$hosts")" = "10.0.2.100 ssh-ed25519 AAAAFAKE" ] || { echo "bad known hosts: $(cat "$hosts")" >&2; exit 13; }
echo "$key" > "$(dirname "$0")/ssh-key-path"
exit 0
"#;

    /// `claude` that must see the proxy, and must not find the key anywhere.
    const PROXY_CLAUDE: &str = r#"#!/bin/sh
[ "$HTTPS_PROXY" = "http://127.0.0.1:3128" ] && [ "$https_proxy" = "$HTTPS_PROXY" ] || { echo "no proxy: $HTTPS_PROXY" >&2; exit 2; }
case "$NO_PROXY" in *127.0.0.1*) ;; *) echo "no NO_PROXY" >&2; exit 3;; esac
[ -z "$(env | grep -i 'FAKE KEY')" ] || { echo "key in env" >&2; exit 4; }
echo '{"type":"result","is_error":false,"result":"ok","session_id":"s"}'
"#;

    #[tokio::test]
    async fn agentd_opens_the_tunnel_and_points_claude_at_it() {
        if !have_jq() {
            eprintln!("needs jq, skipping");
            return;
        }
        let mut b = boot();
        b.claude_token = "sk-test-token".into();
        b.tunnel = Some(TunnelKeys { key: "-----BEGIN FAKE KEY-----\nabc\n-----END FAKE KEY-----".into(), host_key: "ssh-ed25519 AAAAFAKE".into() });
        let mut run = start_with("agentd-tunnel", PROXY_CLAUDE, task(40), b, Some(FAKE_SSH));
        let (_, fin) = run.finish(30).await;
        assert_eq!(fin.status, Status::Done, "{}", fin.result);
        let args = std::fs::read_to_string(run.dir.join("ssh-args")).unwrap();
        assert!(args.contains("-L 127.0.0.1:3128:egress:3128"), "{args}");
        assert!(args.contains("agent@10.0.2.100"), "{args}");
        assert!(args.contains("StrictHostKeyChecking=yes"), "{args}");
        let key = std::fs::read_to_string(run.dir.join("ssh-key-path")).unwrap();
        assert!(!Path::new(key.trim()).exists(), "the key file is removed once ssh has it");
        let _ = std::fs::remove_dir_all(run.dir);
    }

    /// `claude` that must trust the injection CA through the variables agentd sets.
    const CA_CLAUDE: &str = r#"#!/bin/sh
[ "$(head -n1 "$NODE_EXTRA_CA_CERTS")" = "-----BEGIN CERTIFICATE-----" ] || { echo "no CA file: $NODE_EXTRA_CA_CERTS" >&2; exit 2; }
[ "$REQUESTS_CA_BUNDLE" = "$(dirname "$NODE_EXTRA_CA_CERTS")/bundle.crt" ] || { echo "no bundle var" >&2; exit 3; }
echo '{"type":"result","is_error":false,"result":"ok","session_id":"s"}'
"#;

    #[tokio::test]
    async fn agentd_installs_the_injection_ca() {
        if !have_jq() {
            eprintln!("needs jq, skipping");
            return;
        }
        let pem = "-----BEGIN CERTIFICATE-----\nQUJD\n-----END CERTIFICATE-----";
        let mut b = boot();
        b.ca = Some(pem.into());
        let mut run = start_with("agentd-ca", CA_CLAUDE, task(40), b, None);
        let (_, fin) = run.finish(30).await;
        assert_eq!(fin.status, Status::Done, "{}", fin.result);
        let bundle = std::fs::read_to_string(run.dir.join("bundle.crt")).unwrap();
        assert_eq!(bundle.trim_end(), pem);
        let _ = std::fs::remove_dir_all(run.dir);
    }

    #[tokio::test]
    async fn agentd_fails_the_run_when_the_tunnel_does_not_open() {
        if !have_jq() {
            eprintln!("needs jq, skipping");
            return;
        }
        let mut b = boot();
        b.tunnel = Some(TunnelKeys { key: "k".into(), host_key: "ssh-ed25519 AAAAFAKE".into() });
        // the default stand-in exits 99
        let mut run = start_with("agentd-notunnel", FAKE_CLAUDE, task(40), b, None);
        let (_, fin) = run.finish(30).await;
        assert_eq!(fin.status, Status::Failed);
        assert!(fin.result.starts_with("tunnel: cannot reach the network proxy (ssh exit 99)"), "{}", fin.result);
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
