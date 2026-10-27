//! `agentd` runs inside an agent VM, once per boot: it reads the bootstrap message, gets a NATS
//! token from the app's identity provider, takes its task from its own queue, runs `claude` on it,
//! streams what `claude` prints and ends with one `final`. See CLAUDE.md for the design.
//!
//! Usage: `agentd [--bootstrap <path>] [--workspace <dir>] [--claude <program>] [--run-as <user>] [--poweroff]`

mod claude;
mod identity;
mod link;

use std::path::PathBuf;
use std::time::Duration;

use protocol::{Bootstrap, Final, Status};

pub struct Args {
    pub bootstrap: PathBuf,
    pub workspace: PathBuf,
    pub claude: String,
    pub run_as: Option<String>,
    pub poweroff: bool,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        bootstrap: "/dev/virtio-ports/bootstrap".into(),
        workspace: "/home/agent/work".into(),
        claude: "claude".into(),
        run_as: None,
        poweroff: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--bootstrap" => a.bootstrap = value()?.into(),
            "--workspace" => a.workspace = value()?.into(),
            "--claude" => a.claude = value()?,
            "--run-as" => a.run_as = Some(value()?),
            "--poweroff" => a.poweroff = true,
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    Ok(a)
}

/// Reads the bootstrap message: one JSON line, then the channel is closed.
async fn read_bootstrap(path: &PathBuf) -> Result<Bootstrap, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let file = tokio::fs::File::open(path).await.map_err(|e| format!("bootstrap {}: {e}", path.display()))?;
    let mut line = String::new();
    BufReader::new(file).read_line(&mut line).await.map_err(|e| e.to_string())?;
    let b: Bootstrap = serde_json::from_str(&line).map_err(|e| format!("bad bootstrap: {e}"))?;
    if b.protocol != protocol::PROTOCOL {
        return Err(format!("bootstrap speaks protocol {}, this agentd {}", b.protocol, protocol::PROTOCOL));
    }
    Ok(b)
}

#[tokio::main]
async fn main() {
    let code = match run().await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("agentd: {e}");
            1
        }
    };
    // Not on failures before there is a link: the app sees the missing heartbeat and kills the VM.
    std::process::exit(code);
}

async fn run() -> Result<(), String> {
    let args = args()?;
    let boot = read_bootstrap(&args.bootstrap).await?;
    let link = link::Link::connect(&boot).await?;
    link.start_heartbeat();

    let task = link.next_task(Duration::from_secs(300)).await?;
    let fin = match claude::run(&args, &link, task).await {
        Ok(f) => f,
        Err(reason) => Final {
            status: Status::Failed,
            result: reason,
            questions: vec![],
            session_id: None,
            workspace: None,
            workspace_error: None,
            cost_usd: None,
            exit_code: None,
        },
    };
    link.finish(fin).await?;
    if args.poweroff {
        let _ = std::process::Command::new("poweroff").status();
    }
    Ok(())
}
