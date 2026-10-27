//! Trades a proof signed with the agent's key for an access token at the identity provider.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use nkeys::KeyPair;
use protocol::Bootstrap;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn form(pairs: &[(&str, &str)]) -> String {
    let enc = |s: &str| {
        s.bytes()
            .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
            .collect::<String>()
    };
    pairs.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

/// A single-use proof for the token endpoint (`private_key_jwt`).
fn proof(boot: &Bootstrap, key: &KeyPair) -> Result<String, String> {
    let claims = json!({
        "iss": boot.name, "sub": boot.name, "aud": format!("{}/token", boot.issuer),
        "iat": now(), "exp": now() + 60, "jti": format!("{}-{}", boot.id, nuid()),
    });
    let input = format!("{}.{}", B64.encode(json!({"typ":"JWT","alg":"EdDSA"}).to_string()), B64.encode(claims.to_string()));
    let sig = key.sign(input.as_bytes()).map_err(|e| e.to_string())?;
    Ok(format!("{input}.{}", B64.encode(sig)))
}

/// Unique enough for a `jti`: time in nanoseconds plus a counter.
fn nuid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{t:x}-{:x}", N.fetch_add(1, Ordering::Relaxed))
}

/// A fresh access token for `audience`. The provider speaks plain HTTP on loopback, so this is a
/// minimal HTTP/1.1 client.
pub async fn token(boot: &Bootstrap, audience: &str) -> Result<String, String> {
    let key = KeyPair::from_seed(&boot.seed).map_err(|e| format!("bad seed: {e}"))?;
    let body = form(&[
        ("grant_type", "client_credentials"),
        ("client_assertion_type", ASSERTION_TYPE),
        ("client_assertion", &proof(boot, &key)?),
        ("audience", audience),
    ]);
    let req = format!(
        "POST /token HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
        boot.idp_addr,
        body.len()
    );
    let mut conn = tokio::net::TcpStream::connect(&boot.idp_addr).await.map_err(|e| format!("identity provider: {e}"))?;
    conn.write_all(req.as_bytes()).await.map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    conn.read_to_end(&mut raw).await.map_err(|e| e.to_string())?;
    let raw = String::from_utf8_lossy(&raw);
    let (head, body) = raw.split_once("\r\n\r\n").ok_or("bad response from identity provider")?;
    let status = head.split_whitespace().nth(1).unwrap_or("0");
    let json: Value = serde_json::from_str(body.trim()).map_err(|_| format!("identity provider answered {status}"))?;
    if status != "200" {
        return Err(format!("token refused: {}", json["error_description"].as_str().unwrap_or(status)));
    }
    json["access_token"].as_str().map(str::to_string).ok_or_else(|| "no access_token".to_string())
}
