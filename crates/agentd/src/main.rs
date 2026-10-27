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
            "--version" => {
                println!("agentd {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    Ok(a)
}

/// Reads the bootstrap message: one JSON line, then the channel is closed. The VM usually boots
/// before the app connects its end of the channel, and until then a read returns end of file, so
/// keep trying for a while.
async fn read_bootstrap(path: &PathBuf) -> Result<Bootstrap, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    loop {
        if let Ok(file) = tokio::fs::File::open(path).await {
            let mut line = String::new();
            if BufReader::new(file).read_line(&mut line).await.is_ok() && !line.trim().is_empty() {
                return serde_json::from_str(&line).map_err(|e| format!("bad bootstrap: {e}"));
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("no bootstrap message on {}", path.display()));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
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
