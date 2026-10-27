//! Credential injection: named rules that tell the egress proxy to put a secret into the requests
//! an agent sends to certain URLs, so the secret itself never enters the VM. Stored as
//! `<app data>/injections.json`, edited in Settings > Injections.
//!
//! An injection says which requests it applies to (host, path pattern, methods), how the header is
//! built (`Bearer`, `Basic` with a user name, or the raw secret in a header of your choice) and
//! which named secret (Settings > Secrets) supplies the value. A profile's environment variable
//! `${injection:NAME}` expands to the injection's placeholder, which is all the VM sees, and turns
//! the injection on for that run. The proxy (`inspect.rs`) terminates TLS for the injection's
//! hosts and sets the header on matching requests, replacing whatever the client sent.

use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::profiles::{EnvVar, clean_host};
use crate::protocol::TaskEnv;

const MAX_INJECTIONS: usize = 100;
const MAX_RULES: usize = 20;
/// What the VM sees when an injection has no placeholder of its own.
pub const DEFAULT_PLACEHOLDER: &str = "ccaf-injected";
const METHODS: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];
/// Headers that frame the message or the connection: never set by an injection.
const FORBIDDEN_HEADERS: &[&str] = &["host", "content-length", "transfer-encoding", "connection", "upgrade", "te", "trailer", "expect"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rule {
    /// `host` or `*.host` (subdomains only), like the network allowlist. HTTPS port 443.
    pub host: String,
    /// Glob over the request path without the query: `*` matches any run of characters,
    /// slashes included. Empty or `*` matches every path.
    #[serde(default)]
    pub path: String,
    /// Upper-case HTTP methods; empty matches all.
    #[serde(default)]
    pub methods: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// `Bearer <secret>`
    Bearer,
    /// `Basic base64(<username>:<secret>)`
    Basic,
    /// The secret as it is.
    Raw,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Injection {
    pub name: String,
    pub rules: Vec<Rule>,
    #[serde(default = "authorization")]
    pub header: String,
    pub kind: Kind,
    /// For `Basic`.
    #[serde(default)]
    pub username: String,
    /// The name of the secret (Settings > Secrets).
    pub secret: String,
    /// What `${injection:NAME}` expands to in the VM; empty means [`DEFAULT_PLACEHOLDER`].
    #[serde(default)]
    pub placeholder: String,
}

fn authorization() -> String {
    "Authorization".into()
}

impl Injection {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::secrets::valid_name(&self.name) {
            return Err(format!("{:?} is not a valid injection name (letters, digits, underscore)", self.name));
        }
        if self.rules.is_empty() || self.rules.len() > MAX_RULES {
            return Err(format!("{}: between 1 and {MAX_RULES} rules", self.name));
        }
        for r in &self.rules {
            clean_host(&r.host).map_err(|e| format!("{}: {e}", self.name))?;
            if r.path.len() > 200 || r.path.chars().any(|c| c.is_control() || c == ' ' || c == '?' || c == '#') {
                return Err(format!("{}: the path pattern is not valid", self.name));
            }
            if !r.path.is_empty() && !r.path.starts_with('/') && !r.path.starts_with('*') {
                return Err(format!("{}: the path pattern must start with / or *", self.name));
            }
            if let Some(m) = r.methods.iter().find(|m| !METHODS.contains(&m.as_str())) {
                return Err(format!("{}: {m:?} is not a method", self.name));
            }
        }
        let h = &self.header;
        if h.is_empty()
            || h.len() > 100
            || !h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            || FORBIDDEN_HEADERS.contains(&h.to_ascii_lowercase().as_str())
            || h.to_ascii_lowercase().starts_with("proxy-")
        {
            return Err(format!("{}: {h:?} cannot be used as the header", self.name));
        }
        if self.kind == Kind::Basic && (self.username.is_empty() || self.username.contains(':') || self.username.chars().any(|c| c.is_control())) {
            return Err(format!("{}: Basic needs a user name without a colon", self.name));
        }
        if !crate::secrets::valid_name(&self.secret) {
            return Err(format!("{}: pick a secret", self.name));
        }
        if self.placeholder.len() > 300 || self.placeholder.chars().any(|c| c.is_control()) {
            return Err(format!("{}: the placeholder is not valid", self.name));
        }
        Ok(())
    }

    fn placeholder(&self) -> &str {
        if self.placeholder.is_empty() { DEFAULT_PLACEHOLDER } else { &self.placeholder }
    }
}

/// Checks a list and normalizes the hosts and methods.
fn cleaned(list: &[Injection]) -> Result<Vec<Injection>, String> {
    if list.len() > MAX_INJECTIONS {
        return Err(format!("at most {MAX_INJECTIONS} injections"));
    }
    let mut out: Vec<Injection> = vec![];
    for i in list {
        let mut i = i.clone();
        for r in &mut i.rules {
            r.host = clean_host(&r.host)?;
            r.path = r.path.trim().to_string();
            r.methods = r.methods.iter().map(|m| m.trim().to_ascii_uppercase()).filter(|m| !m.is_empty()).collect();
            r.methods.dedup();
        }
        i.header = i.header.trim().to_string();
        i.validate()?;
        if out.iter().any(|o| o.name == i.name) {
            return Err(format!("{} is defined twice", i.name));
        }
        out.push(i);
    }
    Ok(out)
}

fn file(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("injections.json"))
}

pub fn load(path: &Path) -> Vec<Injection> {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Vec<Injection>>(&t).ok()).unwrap_or_default()
}

#[tauri::command]
pub fn injections_get(app: tauri::AppHandle) -> Result<Vec<Injection>, String> {
    Ok(load(&file(&app)?))
}

#[tauri::command]
pub fn injections_save(app: tauri::AppHandle, injections: Vec<Injection>) -> Result<Vec<Injection>, String> {
    let list = cleaned(&injections)?;
    let path = file(&app)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&list).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(list)
}

/// The injections of the demo content, one per demo secret (`secrets::DEMO_SECRETS`), in a
/// read-write and a read-only variant. The variants differ only in the secret: the token's own
/// scopes make it read-only (the proxy cannot tell reads from writes by method alone: git fetches
/// with POST, `gh` and Jira search read with POST). GitHub takes `Basic` because git over HTTPS
/// needs it and the API accepts a token as the password too; the Atlassian ones name a placeholder
/// site and e-mail the user must replace (a wildcard over `atlassian.net` would hand the token to
/// anyone's site).
fn demo() -> Vec<Injection> {
    let rule = |host: &str, path: &str| Rule { host: host.into(), path: path.into(), methods: vec![] };
    let basic = |name: &str, rules: Vec<Rule>, username: &str, secret: &str| Injection {
        name: name.into(),
        rules,
        header: authorization(),
        kind: Kind::Basic,
        username: username.into(),
        secret: secret.into(),
        placeholder: String::new(),
    };
    let mut out = vec![];
    for (suffix, secret_infix) in [("", ""), ("_READONLY", "_READONLY")] {
        out.push(basic(&format!("GITHUB{suffix}"), vec![rule("github.com", ""), rule("api.github.com", "")], "x-access-token", &format!("GITHUB{secret_infix}_TOKEN")));
        out.push(basic(&format!("JIRA{suffix}"), vec![rule("your-site.atlassian.net", "/rest/*")], "you@example.com", &format!("JIRA{secret_infix}_TOKEN")));
        out.push(basic(&format!("CONFLUENCE{suffix}"), vec![rule("your-site.atlassian.net", "/wiki/*")], "you@example.com", &format!("CONFLUENCE{secret_infix}_TOKEN")));
    }
    out
}

/// Part of the demo content: adds the demo injections that do not exist yet.
pub fn provision_demo(app: &tauri::AppHandle) -> Result<(), String> {
    let path = file(app)?;
    let mut list = load(&path);
    for i in demo() {
        if !list.iter().any(|o| o.name == i.name) {
            list.push(i);
        }
    }
    let list = cleaned(&list)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&list).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// An injection switched on for a run, with its header value built from the secret. It lives in
/// the app's memory (the proxy) and nowhere else.
#[derive(Clone)]
pub struct Active {
    pub name: String,
    pub rules: Vec<Rule>,
    pub header: String,
    pub value: String,
}

impl std::fmt::Debug for Active {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Active").field("name", &self.name).field("header", &self.header).finish_non_exhaustive()
    }
}

impl Active {
    /// Whether this injection applies to a request. `host` is lowercase without a port.
    pub fn applies(&self, host: &str, method: &str, path: &str) -> bool {
        self.rules.iter().any(|r| {
            crate::egress::host_matches(&r.host, host)
                && (r.methods.is_empty() || r.methods.iter().any(|m| m == method))
                && path_matches(&r.path, path)
        })
    }

    pub fn covers_host(&self, host: &str) -> bool {
        self.rules.iter().any(|r| crate::egress::host_matches(&r.host, host))
    }
}

/// `*` matches any run of characters (slashes included); an empty pattern matches everything. A
/// path that could mean something else after the server normalizes it (dot segments, encoded
/// slashes or dots, backslashes) never matches, so a rule for `/repos/a/*` cannot be stretched to
/// `/repos/a/../b`.
pub fn path_matches(pattern: &str, path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let odd = path.contains('\\')
        || lower.contains("%2e")
        || lower.contains("%2f")
        || lower.contains("%5c")
        || path.split('/').any(|s| s == "." || s == "..");
    if odd {
        return false;
    }
    let pattern = if pattern.is_empty() { "*" } else { pattern };
    glob(pattern.as_bytes(), path.as_bytes())
}

fn glob(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, si));
            pi += 1;
        } else if pi < p.len() && p[pi] == s[si] {
            pi += 1;
            si += 1;
        } else if let Some((sp, ss)) = star {
            pi = sp + 1;
            si = ss + 1;
            star = Some((sp, ss + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == b'*')
}

fn header_value(i: &Injection, secret: &str) -> Result<String, String> {
    if secret.chars().any(|c| c.is_control()) {
        return Err(format!("the secret {} has a control character", i.secret));
    }
    Ok(match i.kind {
        Kind::Bearer => format!("Bearer {secret}"),
        Kind::Basic => format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{}:{secret}", i.username))),
        Kind::Raw => secret.to_string(),
    })
}

/// Turns a profile's variables into the `Task`'s: `${injection:NAME}` becomes the injection's
/// placeholder (and switches it on), `${secret:NAME}` the secret's value. Returns the injections
/// the proxy must apply. `secret` looks a secret up.
pub fn build(list: &[Injection], vars: &[EnvVar], secret: &dyn Fn(&str) -> Option<String>) -> Result<(Vec<TaskEnv>, Vec<Active>), String> {
    let used: std::cell::RefCell<Vec<&Injection>> = Default::default();
    let mut env = vec![];
    for v in vars {
        let value = crate::secrets::resolve(&v.value, "injection", &|n| {
            let i = list.iter().find(|i| i.name == n)?;
            if !used.borrow().iter().any(|u| u.name == i.name) {
                used.borrow_mut().push(i);
            }
            Some(i.placeholder().to_string())
        })
        .and_then(|s| crate::secrets::resolve(&s, "secret", secret))
        .map_err(|e| format!("{}: {e}", v.name))?;
        env.push(TaskEnv { name: v.name.clone(), value });
    }
    let mut active = vec![];
    for i in used.into_inner() {
        let value = secret(&i.secret).ok_or_else(|| format!("the injection {}: the secret {} is not set (Settings > Secrets)", i.name, i.secret))?;
        active.push(Active { name: i.name.clone(), rules: i.rules.clone(), header: i.header.clone(), value: header_value(i, &value)? });
    }
    Ok((env, active))
}

/// [`build`] with the saved injections and the credential store.
pub fn prepare(app: &tauri::AppHandle, vars: &[EnvVar]) -> Result<(Vec<TaskEnv>, Vec<Active>), String> {
    build(&load(&file(app)?), vars, &crate::secrets::secret_value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inj(name: &str) -> Injection {
        Injection {
            name: name.into(),
            rules: vec![Rule { host: "api.github.com".into(), path: String::new(), methods: vec![] }],
            header: "Authorization".into(),
            kind: Kind::Bearer,
            username: String::new(),
            secret: "GH".into(),
            placeholder: String::new(),
        }
    }

    fn var(n: &str, v: &str) -> EnvVar {
        EnvVar { name: n.into(), value: v.into() }
    }

    #[test]
    fn demo_injections_are_valid_and_use_demo_secrets() {
        let list = demo();
        assert_eq!(cleaned(&list).unwrap().len(), 6);
        for i in &list {
            assert!(crate::secrets::DEMO_SECRETS.contains(&i.secret.as_str()), "{}", i.name);
        }
        let names: Vec<_> = list.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"GITHUB_READONLY") && names.contains(&"CONFLUENCE_READONLY"));
    }

    #[test]
    fn globs() {
        assert!(path_matches("", "/anything"));
        assert!(path_matches("*", "/a/b"));
        assert!(path_matches("/repos/*/issues", "/repos/me/x/issues"));
        assert!(path_matches("/repos/a/*", "/repos/a/"));
        assert!(!path_matches("/repos/a/*", "/repos/b/x"));
        assert!(!path_matches("/user", "/user/emails"));
        assert!(path_matches("/*.git/*", "/me/x.git/info/refs"));
        // paths that could mean something else once normalized never match
        for p in ["/repos/a/../b", "/repos/a/%2e%2e/b", "/repos/a%2fb", "/repos/a/./b", "/repos\\a"] {
            assert!(!path_matches("*", p), "{p}");
        }
    }

    #[test]
    fn validates() {
        assert!(inj("A").validate().is_ok());
        let mut i = inj("A");
        i.header = "Host".into();
        assert!(i.validate().is_err());
        i.header = "X-Api-Key".into();
        assert!(i.validate().is_ok());
        i.kind = Kind::Basic;
        assert!(i.validate().is_err()); // no user name
        i.username = "me".into();
        assert!(i.validate().is_ok());
        let mut i = inj("A");
        i.rules[0].host = "1.2.3.4".into();
        assert!(i.validate().is_err());
        let mut i = inj("A");
        i.rules[0].methods = vec!["FETCH".into()];
        assert!(i.validate().is_err());
        let mut i = inj("A");
        i.rules[0].path = "api".into();
        assert!(i.validate().is_err());
        assert!(inj("bad name").validate().is_err());
        assert!(cleaned(&[inj("A"), inj("A")]).is_err());
    }

    #[test]
    fn applies_by_host_method_and_path() {
        let a = Active {
            name: "A".into(),
            rules: vec![Rule { host: "*.example.com".into(), path: "/v1/*".into(), methods: vec!["GET".into()] }],
            header: "Authorization".into(),
            value: "x".into(),
        };
        assert!(a.applies("api.example.com", "GET", "/v1/a"));
        assert!(!a.applies("api.example.com", "POST", "/v1/a"));
        assert!(!a.applies("api.example.com", "GET", "/v2/a"));
        assert!(!a.applies("example.com", "GET", "/v1/a"));
        assert!(a.covers_host("a.example.com") && !a.covers_host("evil.com"));
    }

    #[test]
    fn builds_the_task_env_and_the_active_set() {
        let secrets = |n: &str| (n == "GH").then(|| "s3cret".to_string()).or_else(|| (n == "PLAIN").then(|| "p".to_string()));
        let list = [inj("GITHUB")];
        let (env, active) = build(&list, &[var("GH_TOKEN", "${injection:GITHUB}"), var("X", "${secret:PLAIN}"), var("Y", "1")], &secrets).unwrap();
        assert_eq!(env[0].value, DEFAULT_PLACEHOLDER);
        assert_eq!(env[1].value, "p");
        assert_eq!(env[2].value, "1");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].value, "Bearer s3cret");
        // the same injection twice is one
        let (_, active) = build(&list, &[var("A", "${injection:GITHUB}"), var("B", "${injection:GITHUB}")], &secrets).unwrap();
        assert_eq!(active.len(), 1);
        // none referenced, none active
        assert!(build(&list, &[var("A", "1")], &secrets).unwrap().1.is_empty());
        // errors
        assert!(build(&list, &[var("A", "${injection:NOPE}")], &secrets).err().unwrap().contains("NOPE"));
        assert!(build(&list, &[var("A", "${injection:GITHUB}")], &|_| None).err().unwrap().contains("GH"));
        // basic and raw
        let mut b = inj("B");
        b.kind = Kind::Basic;
        b.username = "me".into();
        let (_, a) = build(&[b], &[var("A", "${injection:B}")], &secrets).unwrap();
        assert_eq!(a[0].value, format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("me:s3cret")));
        let mut r = inj("R");
        r.kind = Kind::Raw;
        r.header = "X-Api-Key".into();
        r.placeholder = "ghp_xxxx".into();
        let (env, a) = build(&[r], &[var("A", "${injection:R}")], &secrets).unwrap();
        assert_eq!((a[0].value.as_str(), env[0].value.as_str()), ("s3cret", "ghp_xxxx"));
    }
}
