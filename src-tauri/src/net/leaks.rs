//! Leak detection: the secrets an agent holds in its environment (`${secret:NAME}` in a profile or
//! integration) may only be sent to the hosts that profile names. A request to any other host that
//! carries one of those values (in the address, a header, or the body) is refused by the proxy,
//! which needs to read the request: the run's TLS inspection covers every host a protected value
//! must not reach (`Inspector::wants`).
//!
//! What it catches: the value as it is, percent-encoded, or inside a `Basic` header. What it does
//! not: a value the agent changes (reversed, split, encrypted) before sending it, or sent to a
//! host the profile does list. It is a net under the allowlist, not a replacement for it.

use base64::Engine;
use hyper::HeaderMap;

use crate::config::profiles::EnvVar;
use crate::net::egress::Rules;

/// Shorter values would match ordinary text.
const MIN_LEN: usize = 8;

#[derive(Clone)]
pub struct Watched {
    pub name: String,
    pub value: String,
}

impl std::fmt::Debug for Watched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watched").field("name", &self.name).finish_non_exhaustive()
    }
}

/// The secrets a profile's environment uses (`${secret:NAME}`), with their values. `secret` looks
/// a value up; names without a usable value are left out.
pub fn watched(vars: &[EnvVar], secret: &dyn Fn(&str) -> Option<String>) -> Vec<Watched> {
    let names: std::cell::RefCell<Vec<String>> = Default::default();
    for v in vars {
        let _ = crate::config::secrets::resolve(&v.value, "secret", &|n| {
            if !names.borrow().iter().any(|x| x == n) {
                names.borrow_mut().push(n.to_string());
            }
            Some(String::new())
        });
    }
    names
        .into_inner()
        .into_iter()
        .filter_map(|name| secret(&name).filter(|v| v.len() >= MIN_LEN && !v.chars().any(|c| c.is_control())).map(|value| Watched { name, value }))
        .collect()
}

#[derive(Clone)]
pub struct Leaks {
    /// Each secret with the byte patterns to look for.
    needles: Vec<(String, Vec<Vec<u8>>)>,
    /// Hosts allowed to receive the values: the ones the profile names.
    home: Rules,
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

impl Leaks {
    /// `None` when there is nothing to protect.
    pub fn new(watched: Vec<Watched>, home: Rules) -> Option<Leaks> {
        if watched.is_empty() {
            return None;
        }
        let needles = watched
            .into_iter()
            .map(|w| {
                let mut forms = vec![w.value.clone().into_bytes()];
                let encoded = percent_encode(&w.value);
                if encoded != w.value {
                    forms.push(encoded.into_bytes());
                }
                (w.name, forms)
            })
            .collect();
        Some(Leaks { needles, home })
    }

    /// Whether a request to `host` must be checked: the profile does not name it.
    pub fn guards(&self, host: &str) -> bool {
        !self.home.allows(host)
    }

    /// The name of the first secret found in `haystack`.
    pub fn find(&self, haystack: &[u8]) -> Option<&str> {
        self.needles.iter().find(|(_, forms)| forms.iter().any(|n| contains(haystack, n))).map(|(name, _)| name.as_str())
    }

    /// Looks in the request line's target and the header values (a `Basic` one also decoded).
    pub fn find_in_head(&self, target: &str, headers: &HeaderMap) -> Option<&str> {
        if let Some(n) = self.find(target.as_bytes()) {
            return Some(n);
        }
        for v in headers.values() {
            let bytes = v.as_bytes();
            if let Some(n) = self.find(bytes) {
                return Some(n);
            }
            let text = String::from_utf8_lossy(bytes);
            if let Some(b64) = text.strip_prefix("Basic ").or_else(|| text.strip_prefix("basic "))
                && let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(b64.trim())
                && let Some(n) = self.find(&decoded)
            {
                return Some(n);
            }
        }
        None
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaks() -> Leaks {
        let w = vec![Watched { name: "TOKEN".into(), value: "s3cret/value+1".into() }];
        Leaks::new(w, Rules::new([vec!["github.com".to_string()]])).unwrap()
    }

    #[test]
    fn finds_the_value_plain_encoded_and_in_basic_auth() {
        let l = leaks();
        assert_eq!(l.find(b"x=s3cret/value+1&y"), Some("TOKEN"));
        assert_eq!(l.find(b"x=s3cret%2Fvalue%2B1"), Some("TOKEN"));
        assert_eq!(l.find(b"nothing here"), None);
        let mut h = HeaderMap::new();
        let basic = base64::engine::general_purpose::STANDARD.encode("user:s3cret/value+1");
        h.insert("authorization", format!("Basic {basic}").parse().unwrap());
        assert_eq!(l.find_in_head("/", &h), Some("TOKEN"));
        let mut clean = HeaderMap::new();
        clean.insert("authorization", "Bearer placeholder".parse().unwrap());
        assert_eq!(l.find_in_head("/ok", &clean), None);
    }

    #[test]
    fn only_the_hosts_the_profile_names_may_carry_it() {
        let l = leaks();
        assert!(!l.guards("github.com"));
        assert!(l.guards("pypi.org"));
    }

    #[test]
    fn watches_only_secrets_the_environment_uses_and_skips_short_values() {
        let vars = vec![
            EnvVar { name: "A".into(), value: "x ${secret:LONG} ${secret:SHORT}".into() },
            EnvVar { name: "B".into(), value: "${injection:J}".into() },
        ];
        let lookup = |n: &str| match n {
            "LONG" => Some("a-long-secret-value".to_string()),
            "SHORT" => Some("dummy".to_string()),
            _ => Some("unrelated-secret-value".to_string()),
        };
        let w = watched(&vars, &lookup);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].name, "LONG");
    }
}
