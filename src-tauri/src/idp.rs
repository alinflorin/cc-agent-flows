//! The app as an OpenID Connect provider for its agents.
//!
//! An identity (see `Identity`) is registered here when `Broker::allocate` creates it. From then on
//! the agent can obtain tokens by itself, for any audience, and any service can verify them with
//! the standard discovery document and JWKS, with no knowledge of this code:
//!
//! - `GET /.well-known/openid-configuration` and `GET /jwks.json` publish the signing key.
//! - `POST /token` is an OAuth2 `client_credentials` grant with `private_key_jwt` client
//!   authentication: the agent signs a short JWT with the key it was allocated, naming the audience
//!   it wants a token for. Each proof is single-use.
//!
//! The signing key is generated on every start and the issuer is a loopback URL with a random port,
//! so tokens do not outlive the app run, like the rest of the identities.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use nkeys::KeyPair;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const TRUST_DOMAIN: &str = "ccagentflows.local";
/// Longest an access token lives; it is also cut to the identity's own expiry.
const TOKEN_TTL: Duration = Duration::from_secs(300);
/// Longest a client assertion may be valid for.
const ASSERTION_TTL: Duration = Duration::from_secs(300);
const ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

/// Who an agent is. Lives in memory only, for as long as the identity is valid.
#[derive(Clone, Serialize)]
pub struct Identity {
    /// A guid; also the key of the agent's NATS subjects and queue.
    pub id: String,
    /// `spiffe://<trust domain>/<project>/<session>/<profile>/agent-<id>`, the `sub` of its tokens.
    pub name: String,
    pub project_id: String,
    pub session_id: String,
    /// The image profile the agent runs on, e.g. `developer`.
    pub profile: String,
    pub expires_at: u64,
    /// The agent's public key (nkey encoding): what its client assertions are checked against.
    #[serde(skip)]
    pub public_key: String,
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Signs `claims` as a JWT with an ed25519 key.
pub fn sign_jwt(key: &KeyPair, alg: &str, kid: Option<&str>, claims: &Value) -> Result<String, String> {
    let mut header = json!({ "typ": "JWT", "alg": alg });
    if let Some(kid) = kid {
        header["kid"] = json!(kid);
    }
    let input = format!("{}.{}", B64.encode(header.to_string()), B64.encode(serde_json::to_vec(claims).map_err(|e| e.to_string())?));
    let sig = key.sign(input.as_bytes()).map_err(|e| e.to_string())?;
    Ok(format!("{input}.{}", B64.encode(sig)))
}

fn parts(token: &str) -> Result<(&str, &str, &str), String> {
    let mut it = token.split('.');
    match (it.next(), it.next(), it.next(), it.next()) {
        (Some(h), Some(p), Some(s), None) => Ok((h, p, s)),
        _ => Err("malformed token".into()),
    }
}

/// The claims of a token, without checking anything.
fn unverified_claims(token: &str) -> Result<Value, String> {
    let (_, p, _) = parts(token)?;
    serde_json::from_slice(&B64.decode(p).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// The claims of a token whose signature checks out against `public_key` (nkey encoding). The
/// caller still has to look at `exp`, `aud` and the rest.
fn verified_claims(public_key: &str, token: &str) -> Result<Value, String> {
    let (h, p, s) = parts(token)?;
    let sig = B64.decode(s).map_err(|e| e.to_string())?;
    KeyPair::from_public_key(public_key)
        .and_then(|k| k.verify(format!("{h}.{p}").as_bytes(), &sig))
        .map_err(|_| "bad signature".to_string())?;
    unverified_claims(token)
}

fn has_audience(claims: &Value, audience: &str) -> bool {
    match &claims["aud"] {
        Value::String(a) => a == audience,
        Value::Array(list) => list.iter().any(|a| a == audience),
        _ => false,
    }
}

#[derive(Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: u64,
}

pub struct OAuthError {
    status: StatusCode,
    error: &'static str,
    description: String,
}

impl OAuthError {
    fn new(status: StatusCode, error: &'static str, description: impl Into<String>) -> Self {
        OAuthError { status, error, description: description.into() }
    }
    fn client(description: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "invalid_client", description)
    }
}

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.error, "error_description": self.description }))).into_response()
    }
}

pub struct Idp {
    authority: KeyPair,
    identities: Mutex<HashMap<String, Identity>>,
    issuer: OnceLock<String>,
    /// Client assertions already used, by `jti`, until they would have expired anyway.
    seen: Mutex<HashMap<String, u64>>,
}

impl Idp {
    pub fn new() -> Arc<Idp> {
        Arc::new(Idp {
            authority: KeyPair::new_server(),
            identities: Mutex::default(),
            issuer: OnceLock::new(),
            seen: Mutex::default(),
        })
    }

    /// The issuer URL, known once the HTTP server is up.
    pub fn issuer(&self) -> &str {
        self.issuer.get().map_or("", String::as_str)
    }

    fn kid(&self) -> String {
        self.authority.public_key()
    }

    pub fn register(&self, identity: Identity) {
        let mut all = self.identities.lock().unwrap();
        all.retain(|_, i| i.expires_at > now());
        all.insert(identity.id.clone(), identity);
    }

    pub fn remove(&self, id: &str) {
        self.identities.lock().unwrap().remove(id);
    }

    pub fn live(&self, id: &str) -> Option<Identity> {
        self.identities.lock().unwrap().get(id).filter(|i| i.expires_at > now()).cloned()
    }

    fn live_by_name(&self, name: &str) -> Option<Identity> {
        self.identities.lock().unwrap().values().find(|i| i.name == name && i.expires_at > now()).cloned()
    }

    /// Mints an access token for `audience`: a standard JWT any service can verify from the JWKS.
    pub fn mint(&self, id: &str, audience: &str) -> Result<TokenResponse, String> {
        let identity = self.live(id).ok_or("unknown or expired identity")?;
        let iat = now();
        let exp = (iat + TOKEN_TTL.as_secs()).min(identity.expires_at);
        let claims = json!({
            "iss": self.issuer(), "sub": identity.name, "aud": audience,
            "iat": iat, "exp": exp, "jti": uuid::Uuid::new_v4().to_string(),
            "agent": identity.id, "project": identity.project_id,
            "session": identity.session_id, "profile": identity.profile,
        });
        Ok(TokenResponse {
            access_token: sign_jwt(&self.authority, "EdDSA", Some(&self.kid()), &claims)?,
            token_type: "Bearer",
            expires_in: exp.saturating_sub(iat),
        })
    }

    /// Checks a token issued here for `audience` and that its identity is still live. Services in
    /// the app use this; others verify the same token from the JWKS.
    #[allow(dead_code)] // no in-app service verifies yet
    pub fn verify(&self, token: &str, audience: &str) -> Result<Identity, String> {
        let claims = verified_claims(&self.kid(), token)?;
        if claims["iss"] != self.issuer() {
            return Err("wrong issuer".into());
        }
        if !has_audience(&claims, audience) {
            return Err("wrong audience".into());
        }
        if claims["exp"].as_u64().is_none_or(|e| e <= now()) {
            return Err("expired".into());
        }
        let id = claims["agent"].as_str().ok_or("no agent")?;
        self.live(id).ok_or_else(|| "identity released or expired".to_string())
    }

    /// The token endpoint: trades a client assertion signed by an agent's own key for an access
    /// token for `audience`.
    fn exchange(&self, assertion: &str, audience: &str) -> Result<TokenResponse, OAuthError> {
        if audience.is_empty() {
            return Err(OAuthError::new(StatusCode::BAD_REQUEST, "invalid_request", "audience is required"));
        }
        let sub = unverified_claims(assertion)
            .ok()
            .and_then(|c| c["sub"].as_str().map(str::to_string))
            .ok_or_else(|| OAuthError::client("unreadable client assertion"))?;
        let identity = self.live_by_name(&sub).ok_or_else(|| OAuthError::client("unknown client"))?;
        let claims = verified_claims(&identity.public_key, assertion).map_err(OAuthError::client)?;

        let token_endpoint = format!("{}/token", self.issuer());
        let now = now();
        let exp = claims["exp"].as_u64().unwrap_or(0);
        if claims["iss"] != sub || !has_audience(&claims, &token_endpoint) {
            return Err(OAuthError::client("iss must be the client and aud the token endpoint"));
        }
        if exp <= now || exp > now + ASSERTION_TTL.as_secs() {
            return Err(OAuthError::client("assertion expired or valid for too long"));
        }
        let jti = claims["jti"].as_str().filter(|j| !j.is_empty()).ok_or_else(|| OAuthError::client("jti is required"))?;
        {
            let mut seen = self.seen.lock().unwrap();
            seen.retain(|_, e| *e > now);
            if seen.insert(jti.to_string(), exp).is_some() {
                return Err(OAuthError::client("assertion already used"));
            }
        }
        self.mint(&identity.id, audience).map_err(|e| OAuthError::new(StatusCode::BAD_REQUEST, "invalid_request", e))
    }

    fn discovery(&self) -> Value {
        let issuer = self.issuer();
        json!({
            "issuer": issuer,
            "token_endpoint": format!("{issuer}/token"),
            "jwks_uri": format!("{issuer}/jwks.json"),
            "grant_types_supported": ["client_credentials"],
            "token_endpoint_auth_methods_supported": ["private_key_jwt"],
            "token_endpoint_auth_signing_alg_values_supported": ["EdDSA"],
            "response_types_supported": ["token"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["EdDSA"],
        })
    }

    fn jwks(&self) -> Value {
        let kid = self.kid();
        let x = nkeys::from_public_key(&kid).map(|(_, raw)| B64.encode(raw)).unwrap_or_default();
        json!({ "keys": [{ "kty": "OKP", "crv": "Ed25519", "alg": "EdDSA", "use": "sig", "kid": kid, "x": x }] })
    }
}

#[derive(Deserialize)]
struct TokenRequest {
    grant_type: String,
    client_assertion_type: String,
    client_assertion: String,
    #[serde(default)]
    audience: String,
}

async fn token(State(idp): State<Arc<Idp>>, Form(req): Form<TokenRequest>) -> Result<Json<TokenResponse>, OAuthError> {
    if req.grant_type != "client_credentials" {
        return Err(OAuthError::new(StatusCode::BAD_REQUEST, "unsupported_grant_type", "use client_credentials"));
    }
    if req.client_assertion_type != ASSERTION_TYPE {
        return Err(OAuthError::client("client_assertion_type must be the jwt-bearer type"));
    }
    idp.exchange(&req.client_assertion, &req.audience).map(Json)
}

/// Serves the provider on a random loopback port and fixes the issuer to it.
pub async fn serve(idp: Arc<Idp>) -> Result<tokio::task::JoinHandle<()>, String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    idp.issuer.set(format!("http://127.0.0.1:{port}")).map_err(|_| "issuer already set".to_string())?;
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(|State(i): State<Arc<Idp>>| async move { Json(i.discovery()) }))
        .route("/jwks.json", get(|State(i): State<Arc<Idp>>| async move { Json(i.jwks()) }))
        .route("/token", post(token))
        .with_state(idp);
    Ok(tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::jwk::JwkSet;
    use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};

    struct Agent {
        key: KeyPair,
        identity: Identity,
    }

    fn agent(idp: &Idp, id: &str) -> Agent {
        let key = KeyPair::new_user();
        let identity = Identity {
            id: id.into(),
            name: format!("spiffe://{TRUST_DOMAIN}/p/s/developer/agent-{id}"),
            project_id: "p".into(),
            session_id: "s".into(),
            profile: "developer".into(),
            expires_at: now() + 600,
            public_key: key.public_key(),
        };
        idp.register(identity.clone());
        Agent { key, identity }
    }

    /// What an agent does to authenticate: sign a short-lived proof naming the token endpoint.
    fn proof(a: &Agent, issuer: &str, jti: &str) -> String {
        let claims = json!({
            "iss": a.identity.name, "sub": a.identity.name, "aud": format!("{issuer}/token"),
            "iat": now(), "exp": now() + 60, "jti": jti,
        });
        sign_jwt(&a.key, "EdDSA", None, &claims).unwrap()
    }

    /// A minimal HTTP/1.1 client: one request per connection, body read to the end.
    async fn http(method: &str, url: &str, form: &[(&str, &str)]) -> (u16, Value) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let rest = url.strip_prefix("http://").unwrap();
        let (host, path) = rest.split_once('/').unwrap();
        let body = serde_urlencoded::to_string(form).unwrap();
        let req = format!(
            "{method} /{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let mut conn = tokio::net::TcpStream::connect(host).await.unwrap();
        conn.write_all(req.as_bytes()).await.unwrap();
        let mut raw = String::new();
        conn.read_to_string(&mut raw).await.unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, serde_json::from_str(body.trim()).unwrap_or(Value::Null))
    }

    async fn get_token(issuer: &str, assertion: &str, audience: &str) -> (u16, Value) {
        http(
            "POST",
            &format!("{issuer}/token"),
            &[
                ("grant_type", "client_credentials"),
                ("client_assertion_type", ASSERTION_TYPE),
                ("client_assertion", assertion),
                ("audience", audience),
            ],
        )
        .await
    }

    #[tokio::test]
    async fn agents_get_tokens_any_oidc_client_can_verify() {
        let idp = Idp::new();
        let _server = serve(idp.clone()).await.unwrap();
        let issuer = idp.issuer().to_string();
        let a = agent(&idp, "a1");
        let b = agent(&idp, "b2");

        // Discovery points at the endpoints; the JWKS key verifies tokens with a stock JWT library.
        let (_, doc) = http("GET", &format!("{issuer}/.well-known/openid-configuration"), &[]).await;
        assert_eq!(doc["issuer"], issuer.as_str());
        let (_, jwks) = http("GET", doc["jwks_uri"].as_str().unwrap(), &[]).await;
        let jwks: JwkSet = serde_json::from_value(jwks).unwrap();

        let (status, body) = get_token(&issuer, &proof(&a, &issuer, "j1"), "git").await;
        assert_eq!(status, 200);
        let token = body["access_token"].as_str().unwrap();

        let header = decode_header(token).unwrap();
        let jwk = jwks.find(header.kid.as_deref().unwrap()).unwrap();
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_audience(&["git"]);
        validation.set_issuer(&[&issuer]);
        let claims = decode::<Value>(token, &DecodingKey::from_jwk(jwk).unwrap(), &validation).unwrap().claims;
        assert_eq!(claims["sub"], a.identity.name.as_str());
        assert_eq!(claims["profile"], "developer");
        // ...and the app's own check agrees.
        assert_eq!(idp.verify(token, "git").unwrap().id, "a1");
        assert!(idp.verify(token, "proxy").is_err(), "audience");

        // A proof is single-use.
        assert_eq!(get_token(&issuer, &proof(&a, &issuer, "j1"), "git").await.0, 401);
        // An agent cannot get a token as another: the proof must be signed by the right key.
        let forged = proof(&Agent { key: KeyPair::new_user(), identity: b.identity.clone() }, &issuer, "j2");
        assert_eq!(get_token(&issuer, &forged, "git").await.0, 401);
        // A proof made for another endpoint is no good here.
        let elsewhere = proof(&a, "http://127.0.0.1:1", "j3");
        assert_eq!(get_token(&issuer, &elsewhere, "git").await.0, 401);

        // Releasing the identity kills new tokens and the ones already out.
        idp.remove("a1");
        assert!(idp.verify(token, "git").is_err());
        assert_eq!(get_token(&issuer, &proof(&a, &issuer, "j4"), "git").await.0, 401);
    }
}
