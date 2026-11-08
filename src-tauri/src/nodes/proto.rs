//! The wire format between the app that runs a session (the hub) and a computer that runs its VMs
//! (a node). Every connection is TLS (`tls.rs`) and starts with one JSON line, the `Hello`, which
//! carries the node's key and the operation; the node answers with one JSON line, a `Reply`. What
//! follows depends on the operation:
//!
//! - `vm`: the connection stays open as the VM's control channel, JSON lines both ways (`Ctl`).
//! - `attach`: after the reply the connection is the raw byte stream of one of the VM's virtio-serial
//!   ports (`agent`, `net`, `shared`).
//! - `image`: if the reply says `need`, the hub sends exactly `size` bytes, the node answers again.
//! - `ping` and `drop`: one reply, nothing more.
//!
//! Lines are read a byte at a time (no read-ahead), because raw bytes may follow a line.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Bumped when the format changes; a node and a hub with different numbers refuse each other.
pub const PROTO: u32 = 1;
const MAX_LINE: usize = 64 * 1024;

#[derive(Serialize, Deserialize)]
pub struct Hello {
    pub proto: u32,
    pub key: String,
    #[serde(flatten)]
    pub op: Op,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Ping,
    /// Make sure the node has this image (a file of the hub), see `ImageRef`.
    Image { image: ImageRef },
    /// Start a VM; the connection becomes its control channel.
    Vm { spec: WireSpec },
    /// Connect to a port of a VM that was started.
    Attach { vm: String, port: String },
    /// Delete the work disks whose key starts with `prefix` (a session or project that was deleted).
    Drop { prefix: String },
}

/// An image of the hub. `key` names it (its path on the hub), `stamp` its version.
#[derive(Clone, Serialize, Deserialize)]
pub struct ImageRef {
    pub key: String,
    pub size: u64,
    pub stamp: String,
}

#[derive(Serialize, Deserialize)]
pub struct WireSpec {
    pub image: ImageRef,
    pub cpus: Option<u32>,
    pub memory_mb: u32,
    pub work_disk: Option<WireDisk>,
    pub shared: bool,
    pub timeout_secs: u64,
    pub grace_secs: u64,
}

/// The agent's disk for a session: the node keeps it under `key` between runs.
#[derive(Serialize, Deserialize)]
pub struct WireDisk {
    pub key: String,
    pub size_mb: u32,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// With an error from `vm`: `failed` (QEMU started, a port did not come up) or `setup`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vm: Option<String>,
    /// From `image`: the node does not have it, send it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub need: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<Info>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Reply {
    pub fn ok() -> Self {
        Reply { ok: true, ..Default::default() }
    }

    pub fn err(e: impl Into<String>) -> Self {
        Reply { ok: false, error: Some(e.into()), ..Default::default() }
    }
}

/// What a node says about itself.
#[derive(Clone, Serialize, Deserialize)]
pub struct Info {
    pub version: String,
    pub arch: String,
    pub os: String,
}

/// The control channel of a VM.
#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Ctl {
    /// Node to hub, when there is nothing else to say.
    Beat,
    Stats { cpu: f32, mem_mb: u64, cpus: u32 },
    /// The VM is gone (`status`: how QEMU ended) with the end of its error output.
    Exit { status: Option<String>, tail: String },
    /// Hub to node: stop the VM.
    Kill,
}

pub async fn read_line<R: AsyncRead + Unpin>(r: &mut R) -> Result<String, String> {
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    loop {
        if r.read(&mut b).await.map_err(|e| e.to_string())? == 0 {
            return Err("the connection closed".into());
        }
        if b[0] == b'\n' {
            break;
        }
        line.push(b[0]);
        if line.len() > MAX_LINE {
            return Err("a line is too long".into());
        }
    }
    String::from_utf8(line).map_err(|_| "a line is not text".to_string())
}

pub async fn read_json<T: for<'a> Deserialize<'a>, R: AsyncRead + Unpin>(r: &mut R) -> Result<T, String> {
    serde_json::from_str(&read_line(r).await?).map_err(|e| format!("bad message: {e}"))
}

pub async fn write_json<T: Serialize, W: AsyncWrite + Unpin>(w: &mut W, v: &T) -> Result<(), String> {
    let mut text = serde_json::to_string(v).map_err(|e| e.to_string())?;
    text.push('\n');
    w.write_all(text.as_bytes()).await.map_err(|e| e.to_string())?;
    w.flush().await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lines_leave_the_bytes_behind_them() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_json(&mut a, &Hello { proto: PROTO, key: "k".into(), op: Op::Attach { vm: "v".into(), port: "net".into() } }).await.unwrap();
        a.write_all(b"RAW").await.unwrap();
        let h: Hello = read_json(&mut b).await.unwrap();
        assert!(matches!(h.op, Op::Attach { ref port, .. } if port == "net"));
        let mut rest = [0u8; 3];
        b.read_exact(&mut rest).await.unwrap();
        assert_eq!(&rest, b"RAW");
    }

    #[test]
    fn the_hello_is_one_flat_object() {
        let text = serde_json::to_string(&Hello { proto: 1, key: "k".into(), op: Op::Ping }).unwrap();
        assert_eq!(text, r#"{"proto":1,"key":"k","op":"ping"}"#);
    }
}
