//! Runs `claude` on the task as an unprivileged user and turns its output into events and a
//! `final`. Everything `claude` prints is untrusted data: parsed, wrapped, truncated, never run.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use protocol::{Bootstrap, Final, Status, Task};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

use crate::Args;
use crate::link::Link;

/// A workspace bigger than this (as base64) is not sent back; it would not fit in a message.
const MAX_BUNDLE: usize = 700 * 1024;
/// Longest single output line read; a longer one is dropped rather than buffered without limit.
const MAX_LINE: usize = 8 << 20;

struct User {
    uid: u32,
    gid: u32,
    home: String,
}

fn lookup(name: &str) -> Result<User, String> {
    let passwd = std::fs::read_to_string("/etc/passwd").map_err(|e| e.to_string())?;
    passwd
        .lines()
        .map(|l| l.split(':').collect::<Vec<_>>())
        .find(|f| f.first() == Some(&name) && f.len() >= 6)
        .and_then(|f| Some(User { uid: f[2].parse().ok()?, gid: f[3].parse().ok()?, home: f[5].to_string() }))
        .ok_or_else(|| format!("no user {name}"))
}

/// A command that runs as `user` (when given) with a clean environment and without any way to gain
/// privileges back.
fn command(program: &str, user: &Option<User>, name: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_clear().stdin(Stdio::null()).kill_on_drop(true);
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }
    // Set by the image so `claude` uses the system ripgrep (the bundled one does not run on musl).
    if let Some(v) = std::env::var_os("USE_BUILTIN_RIPGREP") {
        cmd.env("USE_BUILTIN_RIPGREP", v);
    }
    cmd.env("USER", name).env("LOGNAME", name);
    if let Some(u) = user {
        cmd.env("HOME", &u.home).uid(u.uid).gid(u.gid);
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    } else if let Some(home) = std::env::var_os("HOME") {
        cmd.env("HOME", home);
    }
    cmd
}

async fn git(args: &[&str], dir: &Path, user: &Option<User>, name: &str) -> Result<String, String> {
    let out = command("git", user, name)
        .args(["-c", "user.name=agent", "-c", "user.email=agent@ccagentflows.local", "-c", "safe.directory=*"])
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("git {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn chown(path: &Path, user: &Option<User>) {
    if let Some(u) = user {
        let _ = std::os::unix::fs::chown(path, Some(u.uid), Some(u.gid));
    }
}

/// Puts the task's code in the workspace: a clone of the bundle, or an empty repository.
async fn prepare(args: &Args, task: &Task, user: &Option<User>, name: &str) -> Result<(), String> {
    let ws = &args.workspace;
    tokio::fs::create_dir_all(ws).await.map_err(|e| format!("workspace: {e}"))?;
    chown(ws, user);
    if let Some(bundle) = &task.bundle {
        let bytes = B64.decode(bundle).map_err(|e| format!("bad bundle: {e}"))?;
        let file = ws.with_extension("bundle");
        tokio::fs::write(&file, bytes).await.map_err(|e| e.to_string())?;
        chown(&file, user);
        let file_str = file.to_string_lossy().into_owned();
        let res = git(&["clone", "-q", &file_str, "."], ws, user, name).await;
        let _ = tokio::fs::remove_file(&file).await;
        res?;
    } else if !ws.join(".git").exists() {
        // Best effort: without git the work just cannot be sent back.
        let _ = git(&["init", "-q"], ws, user, name).await;
    }
    Ok(())
}

/// The workspace as a base64 git bundle, with uncommitted work committed first.
async fn bundle(args: &Args, user: &Option<User>, name: &str) -> (Option<String>, Option<String>) {
    let ws = &args.workspace;
    if !ws.join(".git").exists() {
        return (None, Some("not a git repository".into()));
    }
    let _ = git(&["add", "-A"], ws, user, name).await;
    let _ = git(&["commit", "-q", "-m", "agent work"], ws, user, name).await;
    let file = ws.with_extension("out.bundle");
    let file_str = file.to_string_lossy().into_owned();
    let res = git(&["bundle", "create", &file_str, "--all"], ws, user, name).await;
    let bytes = tokio::fs::read(&file).await;
    let _ = tokio::fs::remove_file(&file).await;
    match (res, bytes) {
        (Ok(_), Ok(b)) => {
            let enc = B64.encode(b);
            if enc.len() > MAX_BUNDLE { (None, Some(format!("workspace too large ({} bytes)", enc.len()))) } else { (Some(enc), None) }
        }
        (Err(e), _) => (None, Some(e)),
        (_, Err(e)) => (None, Some(e.to_string())),
    }
}

/// What `claude` printed as its last line (`type: result`).
#[derive(Default)]
struct Outcome {
    is_error: bool,
    text: String,
    structured: Option<Value>,
    cost: Option<f64>,
    session: Option<String>,
}

pub async fn run(args: &Args, link: &Link, boot: &Bootstrap, task: Task) -> Result<Final, String> {
    let name = args.run_as.clone().unwrap_or_else(|| "agent".into());
    let user = args.run_as.as_deref().map(lookup).transpose()?;
    prepare(args, &task, &user, &name).await?;

    let schema = task.schema.clone().unwrap_or_else(|| protocol::RESULT_SCHEMA.to_string());
    let mut cmd = command(&args.claude, &user, &name);
    cmd.current_dir(&args.workspace)
        .args(["-p", &task.prompt, "--output-format", "stream-json", "--verbose", "--include-partial-messages"])
        .args(["--json-schema", &schema, "--dangerously-skip-permissions"])
        .args([if task.resume { "--resume" } else { "--session-id" }, &task.session_id])
        .env("CLAUDE_CODE_OAUTH_TOKEN", &boot.claude_token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(m) = &task.model {
        cmd.args(["--model", m]);
    }
    if let Some(e) = &task.effort {
        cmd.args(["--effort", e]);
    }
    let mut child = cmd.spawn().map_err(|e| format!("could not start {}: {e}", args.claude))?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let mut stderr = child.stderr.take().ok_or("no stderr")?;
    let stderr_tail = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = (&mut stderr).take(64 * 1024).read_to_end(&mut buf).await;
        // Keep draining so `claude` never blocks on a full pipe.
        let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
        String::from_utf8_lossy(&buf).into_owned()
    });

    let mut outcome = None;
    let mut seq = 0u64;
    let mut lines = BufReader::new(stdout).lines();
    let reading = async {
        while let Ok(Some(line)) = lines.next_line().await {
            if line.len() > MAX_LINE || line.trim().is_empty() {
                continue;
            }
            seq += 1;
            let event: Value = serde_json::from_str(&line).unwrap_or_else(|_| serde_json::json!({ "type": "raw", "text": line }));
            if event["type"] == "result" {
                outcome = Some(Outcome {
                    is_error: event["is_error"].as_bool().unwrap_or(false),
                    text: event["result"].as_str().unwrap_or_default().to_string(),
                    structured: event.get("structured_output").filter(|v| !v.is_null()).cloned(),
                    cost: event["total_cost_usd"].as_f64(),
                    session: event["session_id"].as_str().map(str::to_string),
                });
            }
            if let Err(e) = link.event(seq, event) {
                eprintln!("agentd: event {seq} not sent: {e}");
            }
        }
    };
    let timed_out = tokio::time::timeout(Duration::from_secs(task.timeout_secs), reading).await.is_err();
    if timed_out {
        let _ = child.kill().await;
    }
    let exit = child.wait().await.ok().and_then(|s| s.code());
    let stderr = stderr_tail.await.unwrap_or_default();

    let (workspace, workspace_error) = bundle(args, &user, &name).await;
    let mut fin = Final {
        status: Status::Failed,
        result: String::new(),
        questions: vec![],
        session_id: Some(task.session_id.clone()),
        workspace,
        workspace_error,
        cost_usd: None,
        exit_code: exit,
    };
    match outcome {
        Some(o) if !timed_out => {
            fin.cost_usd = o.cost;
            fin.session_id = o.session.or(fin.session_id);
            let s = o.structured.unwrap_or(Value::Null);
            let reported = match s["status"].as_str() {
                Some("done") => Some(Status::Done),
                Some("needs_input") => Some(Status::NeedsInput),
                Some("failed") => Some(Status::Failed),
                _ => None,
            };
            fin.result = s["summary"].as_str().map(str::to_string).unwrap_or(o.text);
            fin.questions = s["questions"]
                .as_array()
                .map(|q| q.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            // `claude` failing outranks what the answer claims.
            fin.status = if o.is_error { Status::Failed } else { reported.unwrap_or(Status::Done) };
        }
        _ if timed_out => fin.result = format!("timed out after {}s", task.timeout_secs),
        _ => {
            let tail: String = stderr.chars().rev().take(2000).collect::<Vec<_>>().into_iter().rev().collect();
            fin.result = format!("claude ended without a result (exit {exit:?}): {}", tail.trim());
        }
    }
    Ok(fin)
}
