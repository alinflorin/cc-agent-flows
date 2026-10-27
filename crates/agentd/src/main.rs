//! `agentd` runs inside an agent VM, once per boot: it reads the bootstrap message from the app on
//! its virtio-serial port, asks for its task, runs `claude` on it, streams what `claude` prints and
//! ends with one `final`. See CLAUDE.md for the design.
//!
//! Usage: `agentd [--channel <device|unix:path>] [--workspace <dir>] [--claude <program>] [--run-as <user>] [--poweroff]`

mod claude;
mod link;

use std::path::PathBuf;
use std::time::Duration;

use protocol::{Final, Status};

pub struct Args {
    /// The virtio-serial device, or `unix:<socket>`.
    pub channel: String,
    pub workspace: PathBuf,
    pub claude: String,
    pub run_as: Option<String>,
    pub poweroff: bool,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        channel: "/dev/virtio-ports/agent".into(),
        workspace: "/home/agent/work".into(),
        claude: "claude".into(),
        run_as: None,
        poweroff: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--channel" => a.channel = value()?,
            "--workspace" => a.workspace = value()?.into(),
            "--claude" => a.claude = value()?,
            "--run-as" => a.run_as = Some(value()?),
            "--poweroff" => a.poweroff = true,
            "--version" => {
                println!("agentd {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    Ok(a)
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
    // The VM boots before the app connects its end, so wait generously for the first message.
    let link = link::Link::open(&args.channel).await?;
    let boot = link.bootstrap(Duration::from_secs(300)).await?;
    link.start_heartbeat();
    let task = link.next_task(Duration::from_secs(300)).await?;
    let fin = match claude::run(&args, &link, &boot, task).await {
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
