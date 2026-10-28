//! Builds images in a builder VM: boots a disk (an overlay of the plain Alpine image), logs in as
//! root on its serial console (the plain image has no root password and no SSH), runs a script
//! and flattens the result into a standalone qcow2. The console is the only way in, which is also
//! how progress is streamed.

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::time::Duration;

use base64::Engine;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::{Instant, sleep, timeout};

pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

/// The guest's serial console.
pub struct Console {
    io: Box<dyn Stream>,
    buf: Vec<u8>,
    nonce: String,
}

impl Console {
    pub fn new(io: Box<dyn Stream>) -> Self {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let nonce = format!("{:08x}", std::process::id() ^ nanos);
        Console { io, buf: vec![], nonce }
    }

    async fn read_more(&mut self) -> Result<(), String> {
        let mut chunk = [0u8; 4096];
        let n = self.io.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("the VM closed its console".into());
        }
        self.buf.extend_from_slice(&chunk[..n]);
        Ok(())
    }

    /// Reads until `needle` appeared; drops everything up to and including it.
    async fn expect(&mut self, needle: &str) -> Result<(), String> {
        loop {
            if let Some(at) = find(&self.buf, needle.as_bytes()) {
                self.buf.drain(..at + needle.len());
                return Ok(());
            }
            self.read_more().await?;
        }
    }

    fn take_line(&mut self) -> Option<String> {
        let at = self.buf.iter().position(|&b| b == b'\n')?;
        // XON/XOFF bytes can leak in when the terminal throttled a large write
        let line: Vec<u8> = self.buf.drain(..=at).filter(|b| !matches!(b, 0x11 | 0x13)).collect();
        Some(clean(&String::from_utf8_lossy(&line)))
    }

    pub async fn send(&mut self, text: &str) -> Result<(), String> {
        self.io.write_all(text.as_bytes()).await.map_err(|e| e.to_string())?;
        self.io.flush().await.map_err(|e| e.to_string())
    }

    /// Waits for the login prompt, logs in as root and quiets the shell (no echo, no prompts, no
    /// kernel messages), so that what follows is only command output.
    pub async fn login(&mut self) -> Result<(), String> {
        self.expect("login: ").await?;
        self.send("root\n").await?;
        self.expect("# ").await?;
        let sync = format!("SYNC-{}", self.nonce);
        self.send(&format!("stty -echo -ixon -ixoff; PS1=; PS2=; dmesg -n 1; echo {sync}\n")).await?;
        // The typed line (echoed before stty took effect) has the text in it too; only a line
        // that is exactly the marker is the answer.
        loop {
            while let Some(line) = self.take_line() {
                if line == sync {
                    return Ok(());
                }
            }
            self.read_more().await?;
        }
    }

    /// Writes a file in the guest. The content travels as base64 in a here-document.
    pub async fn put(&mut self, path: &str, content: &[u8]) -> Result<(), String> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(content);
        let mut cmd = format!("base64 -d >'{path}' <<'EOF_B64'\n");
        for chunk in b64.as_bytes().chunks(76) {
            cmd.push_str(std::str::from_utf8(chunk).unwrap());
            cmd.push('\n');
        }
        cmd.push_str("EOF_B64\n");
        let code = self.run(&cmd, |_| {}).await?;
        if code != 0 {
            return Err(format!("could not write {path} in the guest"));
        }
        Ok(())
    }

    /// Runs shell text in the guest and returns its exit code, passing each output line on.
    pub async fn run(&mut self, cmd: &str, mut on_line: impl FnMut(&str)) -> Result<i32, String> {
        let mark = format!("DONE-{}", self.nonce);
        self.send(&format!("{cmd}\nprintf '%s %s\\n' {mark} $?\n")).await?;
        loop {
            while let Some(line) = self.take_line() {
                if let Some(code) = line.strip_prefix(&mark) {
                    return code.trim().parse().map_err(|_| format!("bad exit status line {line:?}"));
                }
                on_line(&line);
            }
            self.read_more().await?;
        }
    }
}

/// Drops terminal escape sequences and keeps what a progress bar left after its last carriage
/// return, so logs read as text.
fn clean(line: &str) -> String {
    let last = line.trim_end_matches(['\r', '\n']).rsplit('\r').find(|p| !p.is_empty()).unwrap_or("");
    let mut out = String::new();
    let mut chars = last.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                if chars.next() == Some('[') {
                    for n in chars.by_ref() {
                        if n.is_ascii_alphabetic() || n == '~' {
                            break;
                        }
                    }
                }
            }
            c if c.is_control() && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// The shell text that runs a script file in the guest. Its output goes through a pipe so tools
/// see no terminal and print plain lines (progress bars and full-screen installers would otherwise
/// paint with cursor movements that a log cannot show); the exit status is the script's.
pub fn script_cmd(path: &str) -> String {
    format!("(set -o pipefail; TERM=dumb NO_COLOR=1 sh {path} </dev/null 2>&1 | cat)")
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Where the builder VM's console is exposed: a unix socket in the work directory, or on Windows
/// a named pipe (reachable only by the current user). QEMU creates it and waits for us.
pub struct Serial {
    /// For `-serial`.
    pub qemu_arg: String,
    /// For `-chardev` (add `,id=...`): the same socket or pipe as a chardev.
    pub chardev: String,
    #[cfg(unix)]
    path: PathBuf,
    /// The private directory made for a socket whose path in `dir` was too long, removed on drop.
    #[cfg(unix)]
    short_dir: Option<PathBuf>,
    #[cfg(windows)]
    name: String,
}

/// Longest socket path that fits `sockaddr_un.sun_path` everywhere (104 bytes on macOS, 108 on
/// Linux, both with the terminating NUL).
#[cfg(unix)]
const MAX_SOCKET_PATH: usize = 100;

#[cfg(unix)]
impl Drop for Serial {
    fn drop(&mut self) {
        if let Some(d) = &self.short_dir {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

impl Serial {
    pub fn new(dir: &Path) -> Self {
        Self::named(dir, "console")
    }

    /// A socket `<name>.sock` in `dir`, or, when that path is too long for a socket (the app data
    /// dir on macOS is), in a new directory only the current user can open under the temp dir.
    #[cfg(unix)]
    pub fn named(dir: &Path, name: &str) -> Self {
        let file = format!("{name}.sock");
        let mut path = dir.join(&file);
        let mut short_dir = None;
        if path.as_os_str().len() > MAX_SOCKET_PATH {
            use std::os::unix::fs::DirBuilderExt;
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
            let d = std::env::temp_dir().join(format!("ccaf-{}-{nanos:08x}", std::process::id()));
            // if this fails QEMU reports the path, which says enough
            let _ = std::fs::DirBuilder::new().mode(0o700).create(&d);
            path = d.join(&file);
            short_dir = Some(d);
        }
        Serial {
            qemu_arg: format!("unix:{},server=on,wait=off", path.display()),
            chardev: format!("socket,path={},server=on,wait=off", path.display()),
            path,
            short_dir,
        }
    }

    /// A named pipe `ccaf-<name>-<pid>-<nanos>`; `dir` is unused here.
    #[cfg(windows)]
    pub fn named(_dir: &Path, name: &str) -> Self {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        // QEMU adds `\\.\pipe\` itself (char-pipe.c), so it gets the bare name; we open the full path.
        let short = format!("ccaf-{name}-{}-{nanos:08x}", std::process::id());
        Serial { qemu_arg: format!("pipe:{short}"), chardev: format!("pipe,path={short}"), name: format!(r"\\.\pipe\{short}") }
    }

    /// Connects once QEMU has created the socket or pipe.
    #[cfg(unix)]
    pub async fn connect(&self) -> Result<Box<dyn Stream>, String> {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            match tokio::net::UnixStream::connect(&self.path).await {
                Ok(s) => return Ok(Box::new(s)),
                Err(e) if Instant::now() > end => return Err(format!("cannot reach the VM console: {e}")),
                Err(_) => sleep(Duration::from_millis(100)).await,
            }
        }
    }

    #[cfg(windows)]
    pub async fn connect(&self) -> Result<Box<dyn Stream>, String> {
        use tokio::net::windows::named_pipe::ClientOptions;
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            match ClientOptions::new().open(&self.name) {
                Ok(p) => return Ok(Box::new(p)),
                Err(e) if Instant::now() > end => return Err(format!("cannot reach the VM console: {e}")),
                Err(_) => sleep(Duration::from_millis(100)).await,
            }
        }
    }
}

/// Runs a future with a deadline.
pub async fn within<T>(secs: u64, what: &str, f: impl Future<Output = Result<T, String>>) -> Result<T, String> {
    timeout(Duration::from_secs(secs), f).await.map_err(|_| format!("timed out: {what}"))?
}

#[cfg(all(test, unix))]
mod socket_tests {
    use super::*;

    #[test]
    fn long_socket_paths_move_to_a_private_dir() {
        let short = Serial::named(Path::new("/tmp/x"), "agent");
        assert_eq!(short.path, Path::new("/tmp/x/agent.sock"));
        let long = Path::new("/home").join("d".repeat(120));
        let moved = Serial::named(&long, "agent");
        let dir = moved.short_dir.clone().unwrap();
        assert!(moved.path.starts_with(&dir) && moved.path.as_os_str().len() <= MAX_SOCKET_PATH);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        drop(moved);
        assert!(!dir.exists());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn cleans_terminal_noise() {
        assert_eq!(clean("\x1b[31mred\x1b[0m text\r\n"), "red text");
        assert_eq!(clean("10%\r50%\r100% done\r\n"), "100% done");
        assert_eq!(clean("a\x1b7b\x1b8"), "ab");
    }

    use std::process::Stdio;

    fn res() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("resources")
    }

    fn can_run() -> bool {
        std::path::Path::new("/dev/kvm").exists()
            && res().join("qemu/bin/qemu-system-x86_64").exists()
            && res().join("images/alpine-x86_64.qcow2").exists()
            && res().join("images/alpine-x86_64.vmlinuz").exists()
    }

    /// Boots the plain image (x86_64, KVM) and returns its console.
    async fn boot(tag: &str) -> (tokio::process::Child, Console, PathBuf) {
        let work = std::env::temp_dir().join(format!("ccaf-builder-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).unwrap();
        let q = res().join("qemu");
        let disk = work.join("disk.qcow2");
        let ok = std::process::Command::new(q.join("bin/qemu-img"))
            .args(["create", "-q", "-f", "qcow2", "-b"])
            .arg(res().join("images/alpine-x86_64.qcow2"))
            .args(["-F", "qcow2"])
            .arg(&disk)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let serial = Serial::new(&work);
        let vm = tokio::process::Command::new(q.join("bin/qemu-system-x86_64"))
            .arg("-L")
            .arg(q.join("share/qemu"))
            .args(["-accel", "kvm", "-cpu", "max", "-m", "1024", "-display", "none", "-monitor", "none", "-no-reboot", "-nic", "user,model=virtio-net-pci"])
            .args(crate::vm::machine::boot_args(&res().join("images"), "x86_64", false))
            .arg("-drive")
            .arg(format!("file={},if=virtio", disk.display()))
            .arg("-serial")
            .arg(&serial.qemu_arg)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut con = Console::new(serial.connect().await.unwrap());
        within(120, "boot", con.login()).await.unwrap();
        (vm, con, work)
    }

    #[tokio::test]
    async fn drives_the_plain_image_over_its_console() {
        if !can_run() {
            eprintln!("needs KVM, the bundled QEMU and the plain image, skipping");
            return;
        }
        let (_vm, mut con, work) = boot("console").await;
        let mut seen = vec![];
        let code = con.run("echo one; echo two >&2; (exit 3)", |l| seen.push(l.to_string())).await.unwrap();
        assert_eq!(code, 3);
        assert_eq!(seen, ["one", "two"]);
        let payload: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
        con.put("/tmp/blob", &payload).await.unwrap();
        let mut sum = String::new();
        con.run("wc -c </tmp/blob; cksum </tmp/blob", |l| sum.push_str(&format!("{l} "))).await.unwrap();
        assert!(sum.starts_with("20000 "), "{sum}");
        let _ = std::fs::remove_dir_all(work);
    }

    /// The real setup script: downloads packages and Claude Code, so it needs the network and
    /// takes minutes. `cargo test -- --ignored builds_the_base_image`.
    #[tokio::test]
    #[ignore]
    async fn builds_the_base_image() {
        if !can_run() {
            return;
        }
        let (mut vm, mut con, work) = boot("setup").await;
        con.run("mkdir -p /tmp/setup", |_| {}).await.unwrap();
        for f in ["setup.sh", "agentd.sh", "agentd.initd", "ccaf-init.sh"] {
            con.put(&format!("/tmp/setup/{f}"), &std::fs::read(res().join("agentd").join(f)).unwrap()).await.unwrap();
        }
        let code = within(1500, "setup", con.run(&script_cmd("/tmp/setup/setup.sh"), |l| eprintln!("{l}"))).await.unwrap();
        assert_eq!(code, 0);
        // CCAF_KEEP_BASE=<path> keeps the result there, to boot it in other tests.
        if let Ok(out) = std::env::var("CCAF_KEEP_BASE") {
            con.run("sync", |_| {}).await.unwrap();
            con.send("poweroff\n").await.unwrap();
            within(120, "poweroff", async { vm.wait().await.map(|_| ()).map_err(|e| e.to_string()) }).await.unwrap();
            let ok = std::process::Command::new(res().join("qemu/bin/qemu-img"))
                .args(["convert", "-c", "-O", "qcow2"])
                .arg(work.join("disk.qcow2"))
                .arg(&out)
                .status()
                .unwrap()
                .success();
            assert!(ok);
        }
        let _ = std::fs::remove_dir_all(work);
    }
}
