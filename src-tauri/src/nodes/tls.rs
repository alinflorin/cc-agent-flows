//! TLS between hub and node. A node has a self-signed certificate made once (`node-cert.json` in
//! the app data dir, mode 0600); the hub keeps the fingerprint of the certificate of each node it
//! was told to trust and refuses any other. No certificate authority, no names: the fingerprint is
//! what identifies the node, and the node's key (checked after the handshake) what authorizes the hub.

use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::AppHandle;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

#[derive(Serialize, Deserialize)]
struct Stored {
    cert: String,
    key: String,
}

pub struct Identity {
    cert: Vec<u8>,
    key: Vec<u8>,
}

impl Identity {
    /// The saved certificate, made the first time.
    pub fn load(app: &AppHandle) -> Result<Identity, String> {
        let path = crate::util::data_dir(app)?.join("node-cert.json");
        if let Some(s) = crate::util::read_json::<Stored>(&path)
            && let (Ok(cert), Ok(key)) = (B64.decode(&s.cert), B64.decode(&s.key))
        {
            return Ok(Identity { cert, key });
        }
        let key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
        let params = rcgen::CertificateParams::new(vec!["ccaf-node".to_string()]).map_err(|e| e.to_string())?;
        let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
        let id = Identity { cert: cert.der().to_vec(), key: key.serialize_der() };
        crate::util::write_json(&path, &Stored { cert: B64.encode(&id.cert), key: B64.encode(&id.key) })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(id)
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.cert)
    }

    pub fn acceptor(&self) -> Result<TlsAcceptor, String> {
        let config = ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(self.cert.clone())], PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone())))
            .map_err(|e| e.to_string())?;
        Ok(TlsAcceptor::from(Arc::new(config)))
    }
}

/// SHA-256 of a certificate as `AB:CD:...`.
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

fn same_fingerprint(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.chars().filter(char::is_ascii_hexdigit).collect::<String>().to_ascii_lowercase();
    norm(a) == norm(b)
}

/// Accepts the one certificate it was given (or any, to learn a node's fingerprint) and records what it saw.
#[derive(Debug)]
struct Pin {
    want: Option<String>,
    seen: Arc<Mutex<Option<String>>>,
    algs: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for Pin {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _intermediates: &[CertificateDer<'_>], _name: &ServerName<'_>, _ocsp: &[u8], _now: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        let fp = fingerprint(end_entity.as_ref());
        *self.seen.lock().unwrap() = Some(fp.clone());
        match &self.want {
            Some(w) if !same_fingerprint(w, &fp) => Err(rustls::Error::General(format!("the certificate of this node is not the one that was trusted ({fp})"))),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// Connects to `addr` (`host:port`). With `pin` the node's certificate must have that fingerprint;
/// without, any certificate is taken (to read its fingerprint, before anything secret is sent).
/// Returns the stream and the fingerprint the node showed.
pub async fn dial(addr: &str, pin: Option<&str>) -> Result<(TlsStream<TcpStream>, String), String> {
    let seen = Arc::new(Mutex::new(None));
    let verifier = Pin { want: pin.map(str::to_string), seen: seen.clone(), algs: provider().signature_verification_algorithms };
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    let tcp = tokio::time::timeout(std::time::Duration::from_secs(8), TcpStream::connect(addr))
        .await
        .map_err(|_| format!("{addr} did not answer in time"))?
        .map_err(|e| format!("cannot reach {addr}: {e}"))?;
    let _ = tcp.set_nodelay(true);
    let name = ServerName::try_from("ccaf-node").map_err(|e| e.to_string())?;
    let tls = tokio::time::timeout(std::time::Duration::from_secs(8), TlsConnector::from(Arc::new(config)).connect(name, tcp))
        .await
        .map_err(|_| format!("{addr}: the TLS handshake timed out"))?
        .map_err(|e| format!("{addr}: {e}"))?;
    let fp = seen.lock().unwrap().clone().unwrap_or_default();
    Ok((tls, fp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn identity() -> Identity {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["ccaf-node".to_string()]).unwrap().self_signed(&key).unwrap();
        Identity { cert: cert.der().to_vec(), key: key.serialize_der() }
    }

    #[tokio::test]
    async fn only_the_pinned_certificate_is_accepted() {
        let id = identity();
        let acceptor = id.acceptor().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            loop {
                let (s, _) = listener.accept().await.unwrap();
                let a = acceptor.clone();
                tokio::spawn(async move {
                    if let Ok(mut t) = a.accept(s).await {
                        let mut b = [0u8; 4];
                        if t.read_exact(&mut b).await.is_ok() {
                            let _ = t.write_all(&b).await;
                        }
                    }
                });
            }
        });
        // learning the fingerprint
        let (mut c, fp) = dial(&addr, None).await.unwrap();
        assert_eq!(fp, id.fingerprint());
        c.write_all(b"ping").await.unwrap();
        let mut b = [0u8; 4];
        c.read_exact(&mut b).await.unwrap();
        assert_eq!(&b, b"ping");
        // the right pin (any notation), then a wrong one
        assert!(dial(&addr, Some(&fp.to_lowercase().replace(':', ""))).await.is_ok());
        assert!(dial(&addr, Some(&identity().fingerprint())).await.is_err());
    }
}
