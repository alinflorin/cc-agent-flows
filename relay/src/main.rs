//! `ccaf-relay`: runs in the agent VM. Listens on 127.0.0.1:3128 (where `HTTPS_PROXY` points) and
//! carries each connection, as one stream of a yamux session, over the virtio-serial port to the app,
//! where the egress proxy answers it. Static, no dependencies in the image.
//!
//!     ccaf-relay [--port /dev/virtio-ports/net] [--listen 127.0.0.1:3128] [--ready <fifo>]
//!
//! `--ready` gets `0 <guest uptime>` once the local port is listening (the start-up script reads it
//! through a fifo), or `1 0` and a message on stderr when it cannot start.

#[cfg(unix)]
mod port;

#[cfg(unix)]
fn main() {
    use std::io::Write;

    let mut listen = format!("127.0.0.1:{}", ccaf_relay::PROXY_PORT);
    let mut path = "/dev/virtio-ports/net".to_string();
    let mut ready: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let v = args.next().unwrap_or_default();
        match a.as_str() {
            "--listen" => listen = v,
            "--port" => path = v,
            "--ready" => ready = Some(v),
            _ => {
                eprintln!("ccaf-relay: unknown argument {a}");
                std::process::exit(2);
            }
        }
    }
    let report = move |rc: u8| {
        if let Some(p) = ready.clone() {
            // the fifo blocks until the reader opens it: not on the relay's own thread
            std::thread::spawn(move || {
                let up = std::fs::read_to_string("/proc/uptime").ok().and_then(|t| t.split_whitespace().next().map(str::to_string)).unwrap_or_else(|| "0".into());
                if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(p) {
                    let _ = writeln!(f, "{rc} {up}");
                }
            });
        }
    };
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    let result = rt.block_on(port::run(&path, &listen, || report(0)));
    if let Err(e) = result {
        eprintln!("ccaf-relay: {e}");
        report(1);
        // let the reporting thread finish
        std::thread::sleep(std::time::Duration::from_millis(500));
        std::process::exit(1);
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("ccaf-relay runs in the Linux guest only");
}
