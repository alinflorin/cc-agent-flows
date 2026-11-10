//! Agent profiles: how an agent behaves and which image it runs on. A profile has a name, a
//! system prompt, environment variables and the id of an image (see `images.rs`; `base` is the
//! built-in one). The user creates them in the UI; each is `<id>.json` in the app data dir. The
//! app ships none.
//!
//! Everything in a profile travels in the `Task` when a run starts, nothing is baked into an
//! image (variables baked into an image are set on the image itself).
//!
//! Profiles exist in the global settings (`<app data>/profiles`) and in each project, and a
//! profile may **extend** another (`Profile::extends`): a project's profile can extend a global one
//! or another of the project, a global one only a global one. [`resolve`] turns a profile and its
//! ancestors into the one a run uses. A project's profile wins over a global one with the same id.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::scope::Scope;

/// Variables the app or agentd own; a profile may not set them.
const RESERVED: &[&str] = &["CLAUDE_CODE_OAUTH_TOKEN", "PATH", "HOME", "USER", "LOGNAME", "SHELL", "USE_BUILTIN_RIPGREP"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvVar {
    pub name: String,
    #[serde(default)]
    pub value: String,
}

/// How an MCP server is reached.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpKind {
    /// A program started in the VM, talking over stdin/stdout.
    #[default]
    Stdio,
    /// Streamable HTTP.
    Http,
    /// Server-sent events (the older remote transport).
    Sse,
}

/// An MCP server the agent's `claude` gets (`--mcp-config`). Values may hold `${var:NAME}`,
/// `${secret:NAME}` and `${injection:NAME}` like environment variables do.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServer {
    /// The key in `mcpServers`; tools show up as `mcp__<name>__...`.
    pub name: String,
    #[serde(default)]
    pub kind: McpKind,
    /// `stdio`: the program (it must exist in the image) and its arguments.
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// `stdio`: environment variables of the program.
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// `http` and `sse`: the endpoint. Its host is allowed for the agent automatically.
    #[serde(default)]
    pub url: String,
    /// `http` and `sse`: request headers.
    #[serde(default)]
    pub headers: Vec<EnvVar>,
}

const MAX_MCP_SERVERS: usize = 20;

impl McpServer {
    fn validate(&self) -> Result<(), String> {
        let n = &self.name;
        if n.is_empty() || n.len() > 64 || !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Err(format!("{n:?} is not a valid MCP server name (letters, digits, - and _)"));
        }
        if self.args.len() > 100 || self.env.len() > 100 || self.headers.len() > 100 {
            return Err(format!("the MCP server {n} has too many arguments, variables or headers"));
        }
        let texts = self.args.iter().chain([&self.command, &self.url]).chain(self.env.iter().chain(&self.headers).flat_map(|v| [&v.name, &v.value]));
        if texts.clone().any(|t| t.contains('\0') || t.len() > 16 * 1024) {
            return Err(format!("the MCP server {n} has a value that is too long or has a NUL"));
        }
        match self.kind {
            McpKind::Stdio => {
                if self.command.trim().is_empty() {
                    return Err(format!("the MCP server {n} needs a command"));
                }
                validate_env(self.env.iter().map(|v| (v.name.as_str(), Some(v.value.as_str()))))?;
            }
            McpKind::Http | McpKind::Sse => {
                // a `${var:...}` is only known at run time, when the URL is checked again
                if !self.url.contains("${") && mcp_url_host(&self.url).is_none() {
                    return Err(format!("the MCP server {n} needs an https:// (or http://) URL with a host name"));
                }
                let mut seen = std::collections::HashSet::new();
                for h in &self.headers {
                    let ok = !h.name.is_empty() && h.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                    if !ok {
                        return Err(format!("{:?} is not a valid header name", h.name));
                    }
                    if !seen.insert(h.name.to_ascii_lowercase()) {
                        return Err(format!("the header {} is set twice", h.name));
                    }
                }
            }
        }
        Ok(())
    }
}

/// The host name of an MCP endpoint (`http(s)://host[:port]/...`); `None` for anything else or an
/// IP address (the egress proxy only allows names).
pub fn mcp_url_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.contains('@') {
        return None;
    }
    let host = authority.rsplit_once(':').map_or(authority, |(h, port)| if port.chars().all(|c| c.is_ascii_digit()) { h } else { authority });
    clean_host(host).ok()
}

/// The values of a profile's MCP servers as synthetic variables, so they go through the same
/// expansion as the environment (`injections::prepare`); [`mcp_config`] puts them back.
pub fn mcp_vars(servers: &[McpServer]) -> Vec<EnvVar> {
    let v = |name: String, value: &str| EnvVar { name, value: value.to_string() };
    let mut out = vec![];
    for (i, s) in servers.iter().enumerate() {
        out.push(v(format!("M{i}_COMMAND"), &s.command));
        out.push(v(format!("M{i}_URL"), &s.url));
        out.extend(s.args.iter().enumerate().map(|(j, a)| v(format!("M{i}_ARG{j}"), a)));
        out.extend(s.env.iter().enumerate().map(|(j, e)| v(format!("M{i}_ENV{j}"), &e.value)));
        out.extend(s.headers.iter().enumerate().map(|(j, h)| v(format!("M{i}_HDR{j}"), &h.value)));
    }
    out
}

/// The `--mcp-config` JSON for `servers`, from their expanded values (what [`mcp_vars`] gave,
/// after expansion), and the hosts of the remote ones. `None` when there are no servers.
pub fn mcp_config(servers: &[McpServer], expanded: &[crate::vm::protocol::TaskEnv]) -> Result<Option<(String, Vec<String>)>, String> {
    if servers.is_empty() {
        return Ok(None);
    }
    let get = |name: String| expanded.iter().find(|e| e.name == name).map(|e| e.value.clone()).unwrap_or_default();
    let mut map = serde_json::Map::new();
    let mut hosts = vec![];
    for (i, s) in servers.iter().enumerate() {
        let entry = match s.kind {
            McpKind::Stdio => {
                let args: Vec<String> = (0..s.args.len()).map(|j| get(format!("M{i}_ARG{j}"))).collect();
                let env: serde_json::Map<String, serde_json::Value> = s.env.iter().enumerate().map(|(j, e)| (e.name.clone(), get(format!("M{i}_ENV{j}")).into())).collect();
                serde_json::json!({"type": "stdio", "command": get(format!("M{i}_COMMAND")), "args": args, "env": env})
            }
            McpKind::Http | McpKind::Sse => {
                let url = get(format!("M{i}_URL"));
                let host = mcp_url_host(&url).ok_or_else(|| format!("the MCP server {}: {url:?} is not an http(s) URL with a host name", s.name))?;
                if !hosts.contains(&host) {
                    hosts.push(host);
                }
                let headers: serde_json::Map<String, serde_json::Value> = s.headers.iter().enumerate().map(|(j, h)| (h.name.clone(), get(format!("M{i}_HDR{j}")).into())).collect();
                let kind = if s.kind == McpKind::Http { "http" } else { "sse" };
                serde_json::json!({"type": kind, "url": url, "headers": headers})
            }
        };
        map.insert(s.name.clone(), entry);
    }
    Ok(Some((serde_json::json!({"mcpServers": map}).to_string(), hosts)))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// Claude model (an alias like `sonnet` or a full id); `None` is Claude Code's default.
    #[serde(default)]
    pub model: Option<String>,
    /// Thinking level, `low | medium | high | xhigh | max`; `None` is Claude Code's default.
    #[serde(default)]
    pub effort: Option<String>,
    /// Virtual CPUs of the VM; `None` gives it all the host's cores (QEMU limits the number of
    /// vCPUs, not their share of time).
    #[serde(default)]
    pub cpus: Option<u32>,
    /// RAM of the VM in MB; `None`: the parent's, else [`DEFAULT_MEMORY_MB`].
    #[serde(default)]
    pub memory_mb: Option<u32>,
    /// Size in MB of the disk at `/home/agent/work` that an agent of a session keeps across its
    /// runs; `None`: the parent's, else Settings > General > Virtual machines. Used when the
    /// session first makes the disk.
    #[serde(default)]
    pub work_disk_mb: Option<u32>,
    /// The image the agent runs on; `None`: the parent's, else `base`.
    #[serde(default)]
    pub image: Option<String>,
    /// Hosts the agent may reach over HTTPS (port 443), besides the Claude API which is always
    /// allowed: an exact name (`github.com`) or all the subdomains of one (`*.github.com`).
    /// Lowercase, no scheme, port or path. Empty means only the Claude API.
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    /// Names of integrations (Integrations page of the project or the global settings) whose
    /// environment variables and allowed hosts this agent gets, on top of its own.
    #[serde(default)]
    pub integrations: Vec<String>,
    /// Tunnels the agent may have open at once; `None`: the global setting (Settings > Network).
    #[serde(default)]
    pub max_connections: Option<u32>,
    /// Data budgets in MB over a whole run, `None` = no budget: bytes the agent may send out ...
    #[serde(default)]
    pub max_upload_mb: Option<u32>,
    /// ... and bytes it may take in. The connections close when one is used up.
    #[serde(default)]
    pub max_download_mb: Option<u32>,
    /// MCP servers the agent's `claude` can use. Remote ones need their host reachable (it is
    /// allowed automatically); a `stdio` command must exist in the image.
    #[serde(default)]
    pub mcp_servers: Vec<McpServer>,
    /// The id of the profile this one builds on (see [`resolve`]); `None` for a plain profile.
    #[serde(default)]
    pub extends: Option<String>,
}

/// The id Claude Code reports as this machine's (`userID` in `~/.claude.json`) for runs of one
/// profile: 64 hex characters from a secret seed kept in the app data dir (made on first use), the
/// project and the profile id. It stays the same across runs and image rebuilds, so the runs show
/// up as one device in the account's session list; each profile is its own device.
pub fn device_id(app: &tauri::AppHandle, project: Option<&str>, profile: &str) -> String {
    use sha2::{Digest, Sha256};
    static SEED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let seed = SEED.get_or_init(|| {
        let path = crate::util::data_dir(app).map(|d| d.join("device_seed.json"));
        if let Ok(path) = &path
            && let Some(s) = crate::util::read_json::<String>(path).filter(|s| s.len() >= 32)
        {
            return s;
        }
        let bytes: [u8; 32] = rand::random();
        let s: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        if let Ok(path) = &path {
            let _ = crate::util::write_json(path, &s);
        }
        s
    });
    let mut h = Sha256::new();
    h.update(seed.as_bytes());
    h.update(project.unwrap_or("").as_bytes());
    h.update([0]);
    h.update(profile.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Most entries in a profile's host allowlist.
const MAX_ALLOW_HOSTS: usize = 100;

/// Normalizes one allowlist entry (trim, lowercase, no trailing dot) and checks it is a host name
/// or a `*.` wildcard over one. IP addresses and single-label names are refused.
pub fn clean_host(entry: &str) -> Result<String, String> {
    let host = entry.trim().trim_end_matches('.').to_ascii_lowercase();
    // `http://` marks an entry that also allows plain HTTP (port 80), see `net::egress::Rules`
    let (scheme, host) = match host.strip_prefix(HTTP_PREFIX) {
        Some(rest) => (HTTP_PREFIX, rest.to_string()),
        None => ("", host),
    };
    let bad = || format!("{entry:?} is not a host name (use example.com or *.example.com)");
    let name = host.strip_prefix("*.").unwrap_or(&host);
    let labels: Vec<&str> = name.split('.').collect();
    let label_ok = |l: &&str| {
        !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    // The last label must not be all digits, so `1.2.3.4` is not taken for a name.
    let tld_ok = labels.last().is_some_and(|l| l.chars().any(|c| c.is_ascii_alphabetic()));
    if host.len() > 253 || labels.len() < 2 || !labels.iter().all(label_ok) || !tld_ok {
        return Err(bad());
    }
    Ok(format!("{scheme}{host}"))
}

/// Prefix of an allowed-hosts entry that also allows plain HTTP to the host.
pub const HTTP_PREFIX: &str = "http://";

pub const DEFAULT_MEMORY_MB: u32 = 2048;
/// The thinking effort of a profile that sets none: agents mostly act, and thinking costs seconds.
pub const DEFAULT_EFFORT: &str = "low";

/// How many profiles a chain of `extends` may have.
const MAX_DEPTH: usize = 8;

/// A profile as listed to the UI.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub id: String,
    #[serde(flatten)]
    pub profile: Profile,
    /// It is one of the global profiles (not the project's own).
    pub global: bool,
}

/// Checks environment variable names (and values, where given) for a profile or an image.
pub fn validate_env<'a>(vars: impl Iterator<Item = (&'a str, Option<&'a str>)>) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for (name, value) in vars {
        let ok = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ok {
            return Err(format!("{name:?} is not a valid variable name"));
        }
        if RESERVED.contains(&name) || name.starts_with("LD_") {
            return Err(format!("{name} is reserved"));
        }
        if !seen.insert(name) {
            return Err(format!("{name} is set twice"));
        }
        if value.is_some_and(|s| s.contains('\0')) {
            return Err(format!("{name} has a NUL in its value"));
        }
    }
    Ok(())
}

impl Profile {
    /// The image to boot (of a resolved profile; an unresolved one may still inherit it).
    pub fn image_id(&self) -> &str {
        self.image.as_deref().unwrap_or(crate::vm::images::BASE)
    }

    /// The VM's memory in MB (of a resolved profile).
    /// The effort a run uses: the profile's, else `DEFAULT_EFFORT`.
    pub fn effort_or_default(&self) -> String {
        self.effort.clone().unwrap_or_else(|| DEFAULT_EFFORT.into())
    }

    pub fn memory(&self) -> u32 {
        self.memory_mb.unwrap_or(DEFAULT_MEMORY_MB)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.name.len() > 80 {
            return Err("the name must be 1 to 80 characters".into());
        }
        validate_env(self.env.iter().map(|v| (v.name.as_str(), Some(v.value.as_str()))))?;
        if self.system_prompt.len() > 64 * 1024 {
            return Err("the system prompt is too large".into());
        }
        if let Some(m) = &self.model
            && (m.is_empty() || m.len() > 100 || !m.chars().all(|c| c.is_ascii_alphanumeric() || "-._:[]".contains(c)))
        {
            return Err("the model is not a valid name".into());
        }
        if let Some(e) = &self.effort
            && !["low", "medium", "high", "xhigh", "max"].contains(&e.as_str())
        {
            return Err("the effort must be low, medium, high, xhigh or max".into());
        }
        if self.cpus.is_some_and(|c| !(1..=256).contains(&c)) {
            return Err("CPUs must be between 1 and 256".into());
        }
        if self.memory_mb.is_some_and(|m| !(1024..=262_144).contains(&m)) {
            return Err("memory must be between 1024 and 262144 MB".into());
        }
        {
            use crate::config::vm_settings::{MAX_WORK_DISK_MB, MIN_WORK_DISK_MB};
            if self.work_disk_mb.is_some_and(|m| !(MIN_WORK_DISK_MB..=MAX_WORK_DISK_MB).contains(&m)) {
                return Err(format!("the work disk must be between {MIN_WORK_DISK_MB} and {MAX_WORK_DISK_MB} MB"));
            }
        }
        if self.image.as_deref().is_some_and(str::is_empty) {
            return Err("pick an image".into());
        }
        if self.extends.as_deref().is_some_and(|e| user_path(Path::new(""), e).is_err()) {
            return Err("the parent profile is not a valid id".into());
        }
        if self.max_connections.is_some_and(|c| !(1..=1000).contains(&c)) {
            return Err("connections must be between 1 and 1000".into());
        }
        if [self.max_upload_mb, self.max_download_mb].iter().flatten().any(|m| !(1..=1_048_576).contains(m)) {
            return Err("a data budget must be between 1 MB and 1048576 MB".into());
        }
        if self.allow_hosts.len() > MAX_ALLOW_HOSTS {
            return Err(format!("at most {MAX_ALLOW_HOSTS} allowed hosts"));
        }
        if self.integrations.len() > 50 || self.integrations.iter().any(|n| n.is_empty() || n.len() > 80) {
            return Err("at most 50 integrations".into());
        }
        if self.mcp_servers.len() > MAX_MCP_SERVERS {
            return Err(format!("at most {MAX_MCP_SERVERS} MCP servers"));
        }
        let mut names = std::collections::HashSet::new();
        for m in &self.mcp_servers {
            m.validate()?;
            if !names.insert(&m.name) {
                return Err(format!("the MCP server {} is listed twice", m.name));
            }
        }
        let mut seen = std::collections::HashSet::new();
        for h in &self.allow_hosts {
            if !h.contains("${var:") && clean_host(h)? != *h {
                return Err(format!("{h:?} is not in normal form"));
            }
            if !seen.insert(h) {
                return Err(format!("{h} is listed twice"));
            }
        }
        Ok(())
    }
}

/// Turns a name into a file-safe id.
pub fn slug(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

fn read_dir(dir: &Path) -> Vec<Entry> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut out = vec![];
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "json")
            && let (Some(id), Ok(text)) = (p.file_stem().and_then(|s| s.to_str()), std::fs::read_to_string(&p))
            && let Ok(profile) = serde_json::from_str::<Profile>(&text)
            && profile.validate().is_ok()
        {
            out.push(Entry { id: id.to_string(), profile, global: false });
        }
    }
    out
}

/// All profiles, sorted by name.
pub fn list(user: &Path) -> Vec<Entry> {
    let mut v = read_dir(user);
    v.sort_by_key(|x| x.profile.name.to_lowercase());
    v
}

fn user_path(user: &Path, id: &str) -> Result<PathBuf, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err("bad profile id".into());
    }
    Ok(user.join(format!("{id}.json")))
}

#[cfg(test)]
/// Saves a profile. `id` is `None` for a new one (derived from the name). Returns the id.
pub fn save(user: &Path, id: Option<&str>, profile: &Profile) -> Result<String, String> {
    save_avoiding(user, id, profile, &[])
}

/// [`save`], where a new id also steers clear of `avoid` (the ids of the global profiles, when a
/// project's profile is saved).
fn save_avoiding(user: &Path, id: Option<&str>, profile: &Profile, avoid: &[String]) -> Result<String, String> {
    // Normalize the allowlist (case, blanks, duplicates) before checking it.
    let mut profile = profile.clone();
    let mut hosts: Vec<String> = vec![];
    for h in profile.allow_hosts.iter().filter(|h| !h.trim().is_empty()) {
        // a `${var:NAME}` is only known at run time, when the entry is checked again
        let h = if h.contains("${var:") { h.trim().to_string() } else { clean_host(h)? };
        if !hosts.contains(&h) {
            hosts.push(h);
        }
    }
    profile.allow_hosts = hosts;
    let profile = &profile;
    profile.validate()?;
    let id = match id {
        Some(id) => id.to_string(),
        None => {
            let base = slug(&profile.name);
            if base.is_empty() {
                return Err("the name needs letters or digits".into());
            }
            let mut id = base.clone();
            let mut n = 2;
            while user.join(format!("{id}.json")).exists() || avoid.contains(&id) {
                id = format!("{base}-{n}");
                n += 1;
            }
            id
        }
    };
    let path = user_path(user, &id)?;
    crate::util::write_json(&path, profile)?;
    Ok(id)
}

pub fn delete(user: &Path, id: &str) -> Result<(), String> {
    std::fs::remove_file(user_path(user, id)?).map_err(|e| e.to_string())
}

/// Where a project's profiles are.
pub fn dir(app: &tauri::AppHandle, project: &str) -> Result<PathBuf, String> {
    dir_of(app, &Scope::project(project))
}

/// Where the profiles of a scope are (the global ones in the app data dir).
pub fn dir_of(app: &tauri::AppHandle, scope: &Scope) -> Result<PathBuf, String> {
    Ok(scope.dir(app)?.join("profiles"))
}

/// The global profiles.
fn global(app: &tauri::AppHandle) -> Vec<Entry> {
    dir_of(app, &Scope::global())
        .map(|d| list(&d).into_iter().map(|e| Entry { global: true, ..e }).collect())
        .unwrap_or_default()
}

/// What a scope's profiles can pick from: the project's own, then the global ones (those with an
/// id the project has itself are hidden); only the global ones for the global scope.
pub fn available(app: &tauri::AppHandle, project: Option<&str>) -> Vec<Entry> {
    let mut all = match project {
        Some(p) => dir(app, p).map(|d| list(&d)).unwrap_or_default(),
        None => vec![],
    };
    for g in global(app) {
        if !all.iter().any(|e| e.id == g.id) {
            all.push(g);
        }
    }
    all
}

/// Joins two system prompts, the parent's first.
fn join_prompts(parent: &str, child: &str) -> String {
    crate::workflow::combine_system_prompts(parent, child).unwrap_or_default()
}

/// `child` on top of `parent`: the child's own settings win, the lists (hosts, integrations) are
/// added together, the environment is merged by name, the system prompts are joined (parent first).
fn inherit(parent: Profile, child: Profile) -> Profile {
    let mut env = parent.env;
    for v in child.env {
        match env.iter_mut().find(|e| e.name == v.name) {
            Some(e) => e.value = v.value,
            None => env.push(v),
        }
    }
    let union = |mut a: Vec<String>, b: Vec<String>| {
        for x in b {
            if !a.contains(&x) {
                a.push(x);
            }
        }
        a
    };
    let mut mcp_servers = parent.mcp_servers;
    for m in child.mcp_servers {
        match mcp_servers.iter_mut().find(|e| e.name == m.name) {
            Some(e) => *e = m,
            None => mcp_servers.push(m),
        }
    }
    Profile {
        name: child.name,
        description: child.description,
        system_prompt: join_prompts(&parent.system_prompt, &child.system_prompt),
        env,
        model: child.model.or(parent.model),
        effort: child.effort.or(parent.effort),
        cpus: child.cpus.or(parent.cpus),
        memory_mb: child.memory_mb.or(parent.memory_mb),
        work_disk_mb: child.work_disk_mb.or(parent.work_disk_mb),
        image: child.image.or(parent.image),
        allow_hosts: union(parent.allow_hosts, child.allow_hosts),
        integrations: union(parent.integrations, child.integrations),
        max_connections: child.max_connections.or(parent.max_connections),
        max_upload_mb: child.max_upload_mb.or(parent.max_upload_mb),
        max_download_mb: child.max_download_mb.or(parent.max_download_mb),
        mcp_servers,
        extends: None,
    }
}

/// The ancestors of a profile, nearest first, found among `all`. Fails on a missing parent, a
/// cycle or a chain over [`MAX_DEPTH`].
fn ancestors(profile: &Profile, all: &[Entry]) -> Result<Vec<Profile>, String> {
    let mut chain: Vec<Profile> = vec![];
    let mut seen: Vec<&str> = vec![];
    let mut next = profile.extends.as_deref();
    while let Some(id) = next {
        if seen.contains(&id) {
            return Err(format!("the profile {id} extends itself through its parents"));
        }
        if seen.len() >= MAX_DEPTH {
            return Err(format!("profiles extend each other more than {MAX_DEPTH} deep"));
        }
        seen.push(id);
        let parent = all.iter().find(|e| e.id == id).ok_or_else(|| format!("the parent profile {id} does not exist"))?;
        chain.push(parent.profile.clone());
        next = parent.profile.extends.as_deref();
    }
    Ok(chain)
}

/// The profile a run uses: `profile` on top of its ancestors found among `all`. The result has no
/// `extends`.
pub fn resolve(profile: &Profile, all: &[Entry]) -> Result<Profile, String> {
    let mut chain = ancestors(profile, all)?;
    let mut out = match chain.pop() {
        Some(root) => root,
        None => return Ok(Profile { extends: None, ..profile.clone() }),
    };
    while let Some(p) = chain.pop() {
        out = inherit(out, p);
    }
    Ok(inherit(out, profile.clone()))
}

/// Every profile of `all` resolved, by id; a profile that cannot be resolved has its reason.
pub fn resolve_all(all: &[Entry]) -> std::collections::HashMap<String, Result<Profile, String>> {
    all.iter().map(|e| (e.id.clone(), resolve(&e.profile, all))).collect()
}

/// Checks the parent of a profile about to be saved: it exists among `all` (what the scope can
/// see), is not the profile itself and does not lead back to it.
fn check_extends(id: Option<&str>, profile: &Profile, all: &[Entry]) -> Result<(), String> {
    let Some(parent) = profile.extends.as_deref() else { return Ok(()) };
    if id == Some(parent) {
        return Err("a profile cannot extend itself".into());
    }
    if !all.iter().any(|e| e.id == parent) {
        return Err(format!("the parent profile {parent} does not exist"));
    }
    // missing parents, loops among the ancestors and chains that are too deep
    ancestors(profile, all)?;
    // the ancestors are known by content, not by id, so walk the ids to see whether it comes back
    let mut next = Some(parent);
    while let Some(n) = next {
        if Some(n) == id {
            return Err("the parent extends this profile: that would be a loop".into());
        }
        next = all.iter().find(|e| e.id == n).and_then(|e| e.profile.extends.as_deref());
    }
    Ok(())
}

/// Ids of the images that the profiles of a project (`None`: the global ones) use, after
/// inheritance.
pub fn used_images(app: &tauri::AppHandle, project: Option<&str>) -> Vec<String> {
    let all = available(app, project);
    let own: Vec<&Entry> = all.iter().filter(|e| e.global == project.is_none()).collect();
    own.into_iter()
        .map(|e| resolve(&e.profile, &all).map_or_else(|_| e.profile.image_id().to_string(), |p| p.image_id().to_string()))
        .collect()
}

/// The profiles of the project (`project` null: the global ones), not the inherited global ones.
#[tauri::command]
pub fn profiles_list(app: tauri::AppHandle, project: Option<String>) -> Result<Vec<Entry>, String> {
    let scope = Scope::of(project)?;
    let global = scope.is_global();
    Ok(list(&dir_of(&app, &scope)?).into_iter().map(|e| Entry { global, ..e }).collect())
}

/// The profiles a project can use: its own and the global ones (null project: the global ones).
#[tauri::command]
pub fn profiles_available(app: tauri::AppHandle, project: Option<String>) -> Result<Vec<Entry>, String> {
    let scope = Scope::of(project)?;
    scope.dir(&app)?;
    Ok(available(&app, scope.id()))
}

/// Creates (`id` null) or updates a profile; returns its id.
#[tauri::command]
pub fn profiles_save(app: tauri::AppHandle, project: Option<String>, id: Option<String>, profile: Profile) -> Result<String, String> {
    let scope = Scope::of(project)?;
    let dir = dir_of(&app, &scope)?;
    let all = available(&app, scope.id());
    check_extends(id.as_deref(), &profile, &all)?;
    // a project's profile never takes the id of a global one
    let taken: Vec<String> = if scope.is_global() { vec![] } else { global(&app).into_iter().map(|e| e.id).collect() };
    if id.as_deref().is_some_and(|i| taken.iter().any(|t| t == i) && !user_path(&dir, i).is_ok_and(|p| p.exists())) {
        return Err("a global profile has this id".into());
    }
    save_avoiding(&dir, id.as_deref(), &profile, &taken)
}

/// Deletes a profile; refused while another profile extends it (for a global one, in any project).
#[tauri::command]
pub fn profiles_delete(app: tauri::AppHandle, project: Option<String>, id: String) -> Result<(), String> {
    let scope = Scope::of(project)?;
    let dir = dir_of(&app, &scope)?;
    let mut scopes: Vec<Scope> = vec![scope.clone()];
    if scope.is_global() {
        scopes.extend(crate::config::projects::ids(&app).iter().map(|p| Scope::project(p)));
    }
    for s in scopes {
        let in_scope = dir_of(&app, &s)?;
        // a project's profile with the same id hides the global one, so it does not count
        if !s.is_global() && scope.is_global() && user_path(&in_scope, &id).is_ok_and(|p| p.exists()) {
            continue;
        }
        if let Some(e) = list(&in_scope).into_iter().find(|e| e.profile.extends.as_deref() == Some(id.as_str())) {
            return Err(format!("the agent profile {} extends it", e.profile.name));
        }
    }
    delete(&dir, &id)
}

#[cfg(test)]
mod tests {
    #[test]
    fn mcp_servers_become_a_config_and_hosts() {
        use crate::vm::protocol::TaskEnv;
        let pair = |n: &str, v: &str| EnvVar { name: n.into(), value: v.into() };
        let servers = vec![
            McpServer { name: "docs".into(), kind: McpKind::Http, url: "https://mcp.example.com:8443/mcp".into(), headers: vec![pair("Authorization", "Bearer x")], ..Default::default() },
            McpServer { name: "old".into(), kind: McpKind::Sse, url: "http://sse.example.org/sse".into(), ..Default::default() },
            McpServer { name: "local".into(), command: "npx".into(), args: vec!["-y".into(), "pkg".into()], env: vec![pair("KEY", "v")], ..Default::default() },
        ];
        for s in &servers {
            s.validate().unwrap();
        }
        let expanded: Vec<TaskEnv> = mcp_vars(&servers).into_iter().map(|v| TaskEnv { name: v.name, value: v.value }).collect();
        let (json, hosts) = mcp_config(&servers, &expanded).unwrap().unwrap();
        assert_eq!(hosts, vec!["mcp.example.com", "sse.example.org"]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["mcpServers"]["docs"]["type"], "http");
        assert_eq!(v["mcpServers"]["docs"]["headers"]["Authorization"], "Bearer x");
        assert_eq!(v["mcpServers"]["old"]["type"], "sse");
        assert_eq!(v["mcpServers"]["local"]["args"], serde_json::json!(["-y", "pkg"]));
        assert_eq!(v["mcpServers"]["local"]["env"]["KEY"], "v");
        assert!(mcp_config(&[], &[]).unwrap().is_none());
    }

    #[test]
    fn mcp_servers_are_checked() {
        let bad = |s: McpServer| s.validate().is_err();
        assert!(bad(McpServer { name: "a b".into(), ..Default::default() }));
        assert!(bad(McpServer { name: "x".into(), ..Default::default() }), "stdio needs a command");
        assert!(bad(McpServer { name: "x".into(), kind: McpKind::Http, url: "ftp://a.com".into(), ..Default::default() }));
        assert!(bad(McpServer { name: "x".into(), kind: McpKind::Http, url: "http://127.0.0.1/mcp".into(), ..Default::default() }));
        assert!(!bad(McpServer { name: "x".into(), kind: McpKind::Sse, url: "${var:URL}".into(), ..Default::default() }));
        assert_eq!(mcp_url_host("https://user@a.com/"), None);
    }

    use super::*;

    fn p(name: &str) -> Profile {
        Profile { name: name.into(), description: String::new(), system_prompt: "sp".into(), env: vec![], model: None, effort: None, cpus: None, memory_mb: Some(DEFAULT_MEMORY_MB), work_disk_mb: None, image: Some("base".into()), allow_hosts: vec![], integrations: vec![], max_connections: None, max_upload_mb: None, max_download_mb: None, mcp_servers: vec![], extends: None }
    }

    #[test]
    fn allowlist_is_normalized_and_validated() {
        let dir = tmp("allow");
        let mut x = p("a");
        x.allow_hosts = vec![" GitHub.com. ".into(), "".into(), "*.NPMjs.org".into(), "github.com".into()];
        let id = save(&dir, None, &x).unwrap();
        let saved = list(&dir).into_iter().find(|e| e.id == id).unwrap();
        assert_eq!(saved.profile.allow_hosts, vec!["github.com", "*.npmjs.org"]);
        assert_eq!(clean_host(" HTTP://Example.com. ").unwrap(), "http://example.com");
        assert!(clean_host("http://10.0.0.1").is_err());
        for bad in ["1.2.3.4", "localhost", "https://a.com", "a.com:443", "a.com/x", "*", "a.*.com", "-a.com", "a b.com", "a..com"] {
            x.allow_hosts = vec![bad.into()];
            assert!(save(&dir, None, &x).is_err(), "{bad}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn allowlist_accepts_variable_references_unchecked() {
        let dir = tmp("allowvar");
        let mut x = p("a");
        x.allow_hosts = vec!["${var:ATLASSIAN_HOST}".into(), "github.com".into()];
        let id = save(&dir, None, &x).unwrap();
        let saved = list(&dir).into_iter().find(|e| e.id == id).unwrap();
        assert_eq!(saved.profile.allow_hosts, vec!["${var:ATLASSIAN_HOST}", "github.com"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ccaf-prof-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn entry(id: &str, profile: Profile) -> Entry {
        Entry { id: id.into(), profile, global: false }
    }

    #[test]
    fn a_child_is_built_on_its_parents() {
        let mut base = p("Base");
        base.system_prompt = "base rules".into();
        base.env = vec![EnvVar { name: "A".into(), value: "1".into() }, EnvVar { name: "B".into(), value: "1".into() }];
        base.allow_hosts = vec!["github.com".into()];
        base.integrations = vec!["Jira".into()];
        base.model = Some("opus".into());
        base.cpus = Some(2);
        base.memory_mb = Some(4096);
        base.work_disk_mb = Some(8192);
        base.image = Some("developer".into());
        let mut mid = p("Mid");
        mid.extends = Some("base".into());
        mid.system_prompt = "mid rules".into();
        mid.env = vec![EnvVar { name: "B".into(), value: "2".into() }];
        mid.allow_hosts = vec!["npmjs.org".into(), "github.com".into()];
        mid.memory_mb = None;
        mid.image = None;
        mid.model = None;
        let mut leaf = p("Leaf");
        leaf.extends = Some("mid".into());
        leaf.system_prompt = String::new();
        leaf.env = vec![EnvVar { name: "C".into(), value: "3".into() }];
        leaf.effort = Some("high".into());
        leaf.memory_mb = None;
        leaf.image = None;
        let all = vec![entry("base", base), entry("mid", mid)];
        let r = resolve(&leaf, &all).unwrap();
        assert_eq!(r.work_disk_mb, Some(8192), "the work disk size comes from the parent");
        assert_eq!(r.name, "Leaf");
        assert_eq!(r.system_prompt, "base rules\n\nmid rules");
        let env: Vec<(&str, &str)> = r.env.iter().map(|v| (v.name.as_str(), v.value.as_str())).collect();
        assert_eq!(env, [("A", "1"), ("B", "2"), ("C", "3")]);
        assert_eq!(r.allow_hosts, ["github.com", "npmjs.org"]);
        assert_eq!(r.integrations, ["Jira"]);
        assert_eq!((r.model.as_deref(), r.effort.as_deref(), r.cpus), (Some("opus"), Some("high"), Some(2)));
        assert_eq!((r.memory(), r.image_id()), (4096, "developer"));
        assert!(r.extends.is_none());
        // a plain profile resolves to itself
        assert_eq!(resolve(&p("x"), &all).unwrap().system_prompt, "sp");
    }

    #[test]
    fn a_missing_parent_or_a_loop_fails_to_resolve() {
        let mut a = p("A");
        a.extends = Some("b".into());
        let mut b = p("B");
        b.extends = Some("a".into());
        let all = vec![entry("a", a.clone()), entry("b", b)];
        assert!(resolve(&a, &all).unwrap_err().contains("itself"));
        assert!(resolve(&a, &[]).unwrap_err().contains("does not exist"));
        let resolved = resolve_all(&all);
        assert!(resolved["a"].is_err() && resolved["b"].is_err());
    }

    #[test]
    fn saving_checks_the_parent() {
        let mut a = p("A");
        a.extends = Some("b".into());
        let mut b = p("B");
        b.extends = Some("c".into());
        let c = p("C");
        let all = vec![entry("b", b), entry("c", c)];
        assert!(check_extends(Some("a"), &a, &all).is_ok());
        // a new profile may extend any, none leads back to it
        assert!(check_extends(None, &a, &all).is_ok());
        // c would extend a, which extends b, which extends c
        let mut c2 = p("C");
        c2.extends = Some("a".into());
        let mut all2 = all.clone();
        all2.push(entry("a", a.clone()));
        assert!(check_extends(Some("c"), &c2, &all2).unwrap_err().contains("loop"));
        let mut me = p("Me");
        me.extends = Some("me".into());
        assert!(check_extends(Some("me"), &me, &all2).is_err());
        me.extends = Some("nobody".into());
        assert!(check_extends(None, &me, &all2).unwrap_err().contains("does not exist"));
        me.extends = Some("../x".into());
        assert!(me.validate().is_err());
    }

    #[test]
    fn validates_env() {
        let mut x = p("a");
        x.env = vec![EnvVar { name: "1BAD".into(), value: String::new() }];
        assert!(x.validate().is_err());
        x.env = vec![EnvVar { name: "PATH".into(), value: "/".into() }];
        assert!(x.validate().is_err());
        x.env = vec![EnvVar { name: "A".into(), value: String::new() }, EnvVar { name: "A".into(), value: String::new() }];
        assert!(x.validate().is_err());
        x.env = vec![EnvVar { name: "GH_TOKEN".into(), value: String::new() }, EnvVar { name: "LANG".into(), value: "C".into() }];
        assert!(x.validate().is_ok());
    }

    #[test]
    fn validates_model_and_effort() {
        let mut x = p("a");
        x.model = Some("claude-sonnet-5-5".into());
        x.effort = Some("xhigh".into());
        assert!(x.validate().is_ok());
        x.model = Some("bad model; rm".into());
        assert!(x.validate().is_err());
        x.model = Some(String::new());
        assert!(x.validate().is_err());
        x.model = None;
        x.effort = Some("extreme".into());
        assert!(x.validate().is_err());
        // old files have neither
        let old: Profile = serde_json::from_str(r#"{"name":"o"}"#).unwrap();
        assert!(old.model.is_none() && old.effort.is_none());
        assert_eq!((old.cpus, old.memory(), old.image_id()), (None, 2048, "base"));
        assert!(old.extends.is_none());
    }

    #[test]
    fn validates_resources() {
        let mut x = p("a");
        x.cpus = Some(4);
        x.memory_mb = Some(4096);
        assert!(x.validate().is_ok());
        x.cpus = Some(0);
        assert!(x.validate().is_err());
        x.cpus = None;
        x.memory_mb = Some(256);
        assert!(x.validate().is_err());
        x.memory_mb = None;
        x.work_disk_mb = Some(8192);
        assert!(x.validate().is_ok());
        x.work_disk_mb = Some(10);
        assert!(x.validate().is_err());
    }

    #[test]
    fn profiles_roundtrip() {
        let usr = tmp("usr");
        let id = save(&usr, None, &p("My Agent!")).unwrap();
        assert_eq!(id, "my-agent");
        assert_eq!(save(&usr, None, &p("My Agent")).unwrap(), "my-agent-2");
        assert_eq!(save(&usr, Some("my-agent"), &p("Renamed")).unwrap(), "my-agent");
        assert!(delete(&usr, "../x").is_err());
        let all = list(&usr);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].profile.name, "My Agent");
        delete(&usr, "my-agent").unwrap();
        assert_eq!(list(&usr).len(), 1);
    }

}
