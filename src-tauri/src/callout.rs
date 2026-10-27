//! NATS auth callout: the server asks us who may connect, and we answer from the identity provider.
//!
//! Every connection except the app's own arrives here with an OIDC access token for the audience
//! `nats` as its auth token. If the provider vouches for it, the answer is a NATS user limited to
//! that agent's subjects, for at most `SESSION_TTL`; otherwise the connection is refused.
//!
//! NATS signs each request with the server's key. We do not check it: the only clients that can
//! publish on the callout subject are the app and the agents it admitted, and agents have no
//! permission for it.

use std::sync::Arc;

use futures::StreamExt;
use nkeys::KeyPair;
use serde_json::{Value, json};

use crate::broker::{ACCOUNT, AUDIENCE, SESSION_TTL, agent_permissions};
use crate::idp::{Idp, now, sign_jwt, unverified_claims};

pub async fn serve(client: async_nats::Client, idp: Arc<Idp>, issuer: KeyPair) {
    let Ok(mut requests) = client.subscribe("$SYS.REQ.USER.AUTH").await else { return };
    while let Some(request) = requests.next().await {
        let (Some(reply), Ok(request)) = (request.reply, std::str::from_utf8(&request.payload)) else { continue };
        if let Some(response) = answer(&idp, &issuer, request) {
            let _ = client.publish(reply, response.into()).await;
        }
    }
}

/// The signed response to one authorization request (itself a JWT), or `None` if it is unreadable.
fn answer(idp: &Idp, issuer: &KeyPair, request: &str) -> Option<String> {
    let claims = unverified_claims(request).ok()?;
    let nats = &claims["nats"];
    let user_nkey = nats["user_nkey"].as_str()?;
    let server = nats["server_id"]["id"].as_str()?;
    let token = nats["connect_opts"]["auth_token"].as_str().unwrap_or_default();

    let outcome = idp.verify(token, AUDIENCE).and_then(|identity| {
        let (publish, subscribe) = agent_permissions(&identity.id);
        let iat = now();
        let exp = (iat + SESSION_TTL.as_secs()).min(identity.expires_at);
        let user = json!({
            "iat": iat, "exp": exp, "iss": issuer.public_key(), "sub": user_nkey, "aud": ACCOUNT, "name": identity.name,
            "nats": {
                "type": "user", "version": 2,
                "pub": { "allow": publish }, "sub": { "allow": subscribe },
                "subs": -1, "data": -1, "payload": -1,
            },
        });
        sign_jwt(issuer, "ed25519-nkey", None, &user)
    });
    let mut response = json!({ "type": "authorization_response", "version": 2 });
    match outcome {
        Ok(jwt) => response["jwt"] = Value::String(jwt),
        Err(why) => response["error"] = Value::String(why),
    }
    let claims = json!({ "iat": now(), "iss": issuer.public_key(), "sub": user_nkey, "aud": server, "nats": response });
    sign_jwt(issuer, "ed25519-nkey", None, &claims).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreadable_requests_get_no_answer() {
        assert!(answer(&Idp::new(), &KeyPair::new_account(), "not a jwt").is_none());
    }
}
