//! The fake command line behind the MCP `execute` tool: `ccaf <resource> <verb> [args] [--flags]`,
//! with a body on stdin for the commands that save structured data. Each command calls the same
//! functions the UI's commands do, so the validation is the same; this file only parses, checks the
//! permissions the user set (Settings > MCP) and prints JSON. Output is compact on purpose (tokens).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Serialize, de::DeserializeOwned};
use tauri::AppHandle;

use super::Config;
use crate::config::scope::Scope;
use crate::config::{injections, integrations, network, profiles, projects, secrets, system_prompt, variables};
use crate::vm::images;
use crate::workflow::engine as eng;
use crate::workflow::{commands as wf, model::Workflow};

pub const HELP: &str = "\
# ccaf commands

Scope: every <scope> below is `--global` or `--project <id>` (exactly one). Bodies marked (stdin) are read from the `stdin` argument.
Deleting needs `--yes`. Output is JSON; errors are plain text.

help
projects list | get <id> | create --name N [--description D] [--prompt P|stdin] | update <id> [--name N] [--description D] [--prompt P|stdin] | delete <id> --yes
prompt get | set (stdin)                                  the global system prompt
profiles list <scope> | available --project <id> | get <id> <scope> | save <scope> [--id ID] (stdin: profile JSON) | delete <id> <scope> --yes
variables list <scope> | set <NAME> <value> <scope> | delete <NAME> <scope>
secrets list <scope> | create <NAME> <scope> | delete <NAME> <scope> --yes     (create makes a placeholder for the user to fill in; values are never read or set by you)
injections get <scope> | upsert <scope> (stdin: object or array) | delete <NAME> <scope> --yes
integrations list <scope> | save <scope> [--previous NAME] (stdin: integration JSON) | delete <NAME> <scope> --yes
images list <scope> | get <id> <scope> | save <scope> [--id ID] (stdin: spec JSON) | build <id> <scope> | delete <id> <scope> --yes
network get <scope> | add <scope> <host>... | remove <scope> <host>...
workflows list --project P | get <id> --project P | standard --project P | save --project P [--id ID] [--name N] [--prompt P] (stdin: workflow JSON) | delete <id> --project P --yes
sessions list --project P | get <id> --project P | log <id> --project P [--last N]
sessions start --project P [--workflow W] [--autonomy ask|never] --task T|stdin | continue <id> --project P | message <id> --project P (text in --text or stdin)
sessions reply <id> --project P [--approve|--reject] (text in --text or stdin) | cancel <id> --project P

Bodies: see read_instructions.";

const OUTPUT_LIMIT: usize = 100_000;

/// Splits a command line like a shell would: whitespace separates, '...' and \"...\" group.
fn tokenize(s: &str) -> Result<Vec<String>, String> {
    let (mut out, mut cur, mut has) = (vec![], String::new(), false);
    let mut quote: Option<char> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some('"') if c == '\\' => match chars.next() {
                Some(n @ ('"' | '\\')) => cur.push(n),
                Some(n) => {
                    cur.push('\\');
                    cur.push(n);
                }
                None => cur.push('\\'),
            },
            Some(_) => cur.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                has = true;
            }
            None if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            None => cur.push(c),
        }
    }
    if quote.is_some() {
        return Err("unclosed quote in the command".into());
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

#[derive(Default)]
struct Args {
    pos: Vec<String>,
    flags: BTreeMap<String, String>,
    bools: BTreeSet<String>,
}

const BOOL_FLAGS: &[&str] = &["global", "yes", "approve", "reject"];

fn parse(tokens: Vec<String>) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = tokens.into_iter();
    while let Some(t) = it.next() {
        let Some(flag) = t.strip_prefix("--").filter(|f| !f.is_empty()) else {
            a.pos.push(t);
            continue;
        };
        if let Some((k, v)) = flag.split_once('=') {
            a.flags.insert(k.to_string(), v.to_string());
        } else if BOOL_FLAGS.contains(&flag) {
            a.bools.insert(flag.to_string());
        } else {
            let v = it.next().ok_or_else(|| format!("--{flag} needs a value"))?;
            a.flags.insert(flag.to_string(), v);
        }
    }
    Ok(a)
}

impl Args {
    /// Rejects flags the command does not know (a typo must not be silently ignored).
    fn only(&self, allowed: &[&str]) -> Result<(), String> {
        for f in self.flags.keys().map(String::as_str).chain(self.bools.iter().map(String::as_str)) {
            if !allowed.contains(&f) {
                return Err(format!("unknown option --{f} (allowed here: {})", allowed.iter().map(|a| format!("--{a}")).collect::<Vec<_>>().join(", ")));
            }
        }
        Ok(())
    }

    fn flag(&self, name: &str) -> Option<&str> {
        self.flags.get(name).map(String::as_str)
    }

    /// The positional at `i`, or an error naming what is missing.
    fn arg(&self, i: usize, what: &str) -> Result<&str, String> {
        self.pos.get(i).map(String::as_str).ok_or_else(|| format!("missing <{what}>"))
    }

    fn yes(&self, what: &str) -> Result<(), String> {
        if self.bools.contains("yes") { Ok(()) } else { Err(format!("this deletes {what}: repeat the command with --yes if the user asked for it")) }
    }

    /// `--global` or `--project <id>`, one of them; a project must exist.
    fn scope(&self, app: &AppHandle) -> Result<Scope, String> {
        let s = match (self.flag("project"), self.bools.contains("global")) {
            (Some(_), true) => return Err("use either --global or --project, not both".into()),
            (Some(p), false) => Scope::of(Some(p.to_string()))?,
            (None, true) => Scope::global(),
            (None, false) => return Err("say where: --global or --project <id>".into()),
        };
        s.dir(app)?;
        Ok(s)
    }

    /// `--project <id>` for the things that only exist in a project.
    fn project(&self, app: &AppHandle) -> Result<String, String> {
        if self.bools.contains("global") {
            return Err("this exists only in a project: use --project <id>".into());
        }
        let s = self.scope(app)?;
        Ok(s.id().unwrap_or_default().to_string())
    }
}

fn json<T: Serialize + ?Sized>(v: &T) -> Result<String, String> {
    serde_json::to_string(v).map_err(|e| e.to_string())
}

fn body<T: DeserializeOwned>(stdin: Option<&str>, what: &str) -> Result<T, String> {
    let text = stdin.filter(|s| !s.trim().is_empty()).ok_or_else(|| format!("this command needs the {what} as JSON in `stdin`"))?;
    serde_json::from_str(text).map_err(|e| format!("the {what} in stdin is not valid: {e}"))
}

/// Writes need `write`; the global settings also need `write_global`.
fn can_write(cfg: &Config, scope: Option<&Scope>) -> Result<(), String> {
    if !cfg.write {
        return Err("the user switched writing off (Settings > MCP): this server is read only".into());
    }
    if scope.is_some_and(Scope::is_global) && !cfg.write_global {
        return Err("the user switched off changes to the global settings (Settings > MCP); use a project".into());
    }
    Ok(())
}

fn proj(scope: &Scope) -> Option<String> {
    scope.id().map(str::to_string)
}

fn cut(mut s: String) -> String {
    if s.len() > OUTPUT_LIMIT {
        let mut end = OUTPUT_LIMIT;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push_str("\n… output cut");
    }
    s
}

pub fn run(app: &AppHandle, cfg: &Config, command: &str, stdin: Option<&str>) -> Result<String, String> {
    let mut tokens = tokenize(command)?;
    if tokens.first().is_some_and(|t| t == "ccaf") {
        tokens.remove(0);
    }
    let a = parse(tokens)?;
    let res = a.pos.first().map(String::as_str).unwrap_or("help");
    let verb = a.pos.get(1).map(String::as_str).unwrap_or("");
    let out = match res {
        "help" => Ok(HELP.to_string()),
        "projects" => projects_cmd(app, cfg, verb, &a, stdin),
        "prompt" => prompt_cmd(app, cfg, verb, &a, stdin),
        "profiles" => profiles_cmd(app, cfg, verb, &a, stdin),
        "variables" => variables_cmd(app, cfg, verb, &a),
        "secrets" => secrets_cmd(app, cfg, verb, &a),
        "injections" => injections_cmd(app, cfg, verb, &a, stdin),
        "integrations" => integrations_cmd(app, cfg, verb, &a, stdin),
        "images" => images_cmd(app, cfg, verb, &a, stdin),
        "network" => network_cmd(app, cfg, verb, &a),
        "workflows" => workflows_cmd(app, cfg, verb, &a, stdin),
        "sessions" => sessions_cmd(app, cfg, verb, &a, stdin),
        other => Err(format!("unknown resource {other:?}; run `ccaf help`")),
    };
    out.map(cut)
}

fn unknown<T>(res: &str, verb: &str) -> Result<T, String> {
    Err(format!("unknown command `{res} {verb}`; run `ccaf help`"))
}

fn projects_cmd(app: &AppHandle, cfg: &Config, verb: &str, a: &Args, stdin: Option<&str>) -> Result<String, String> {
    let prompt = |a: &Args| a.flag("prompt").map(str::to_string).or_else(|| stdin.filter(|s| !s.is_empty()).map(str::to_string));
    match verb {
        "list" => {
            a.only(&[])?;
            json(&projects::list(app))
        }
        "get" => {
            a.only(&[])?;
            json(&projects::load(app, a.arg(2, "id")?).ok_or("no such project")?)
        }
        "create" => {
            a.only(&["name", "description", "prompt"])?;
            can_write(cfg, None)?;
            let name = a.flag("name").ok_or("--name is required")?;
            let id = projects::projects_save(app.clone(), None, name.into(), a.flag("description").unwrap_or("").into(), prompt(a).unwrap_or_default())?;
            json(&serde_json::json!({ "id": id }))
        }
        "update" => {
            a.only(&["name", "description", "prompt"])?;
            can_write(cfg, None)?;
            let id = a.arg(2, "id")?;
            let p = projects::load(app, id).ok_or("no such project")?;
            projects::projects_save(
                app.clone(),
                Some(id.into()),
                a.flag("name").map(str::to_string).unwrap_or(p.name),
                a.flag("description").map(str::to_string).unwrap_or(p.description),
                prompt(a).unwrap_or(p.system_prompt),
            )?;
            Ok("ok".into())
        }
        "delete" => {
            a.only(&["yes"])?;
            can_write(cfg, None)?;
            a.yes("the project with everything in it")?;
            projects::projects_delete(app.clone(), a.arg(2, "id")?.into())?;
            Ok("ok".into())
        }
        _ => unknown("projects", verb),
    }
}

fn prompt_cmd(app: &AppHandle, cfg: &Config, verb: &str, a: &Args, stdin: Option<&str>) -> Result<String, String> {
    a.only(&[])?;
    match verb {
        "get" => json(&system_prompt::current(app)),
        "set" => {
            can_write(cfg, Some(&Scope::global()))?;
            system_prompt::system_prompt_set(app.clone(), stdin.ok_or("the new prompt goes in `stdin`")?.to_string())?;
            Ok("ok".into())
        }
        _ => unknown("prompt", verb),
    }
}

fn profiles_cmd(app: &AppHandle, cfg: &Config, verb: &str, a: &Args, stdin: Option<&str>) -> Result<String, String> {
    match verb {
        "list" => {
            a.only(&["project", "global"])?;
            json(&profiles::profiles_list(app.clone(), proj(&a.scope(app)?))?)
        }
        "available" => {
            a.only(&["project", "global"])?;
            json(&profiles::profiles_available(app.clone(), proj(&a.scope(app)?))?)
        }
        "get" => {
            a.only(&["project", "global"])?;
            let id = a.arg(2, "id")?;
            let list = profiles::profiles_list(app.clone(), proj(&a.scope(app)?))?;
            json(list.iter().find(|e| e.id == id).ok_or("no such profile in this scope")?)
        }
        "save" => {
            a.only(&["project", "global", "id"])?;
            let scope = a.scope(app)?;
            can_write(cfg, Some(&scope))?;
            let profile: profiles::Profile = body(stdin, "profile")?;
            let id = profiles::profiles_save(app.clone(), proj(&scope), a.flag("id").map(str::to_string), profile)?;
            json(&serde_json::json!({ "id": id }))
        }
        "delete" => {
            a.only(&["project", "global", "yes"])?;
            let scope = a.scope(app)?;
            can_write(cfg, Some(&scope))?;
            a.yes("the profile")?;
            profiles::profiles_delete(app.clone(), proj(&scope), a.arg(2, "id")?.into())?;
            Ok("ok".into())
        }
        _ => unknown("profiles", verb),
    }
}

fn variables_cmd(app: &AppHandle, cfg: &Config, verb: &str, a: &Args) -> Result<String, String> {
    a.only(&["project", "global"])?;
    let scope = a.scope(app)?;
    match verb {
        "list" => json(&variables::variables_list(app.clone(), proj(&scope))?),
        "set" => {
            can_write(cfg, Some(&scope))?;
            variables::set(app, &scope, a.arg(2, "NAME")?, a.arg(3, "value")?)?;
            Ok("ok".into())
        }
        "delete" => {
            can_write(cfg, Some(&scope))?;
            variables::variables_delete(app.clone(), proj(&scope), a.arg(2, "NAME")?.into())?;
            Ok("ok".into())
        }
        _ => unknown("variables", verb),
    }
}

/// What `secrets create` stores: the user replaces it in the app. Agents never set real values.
const PLACEHOLDER: &str = "ccaf-placeholder-set-me";

/// Creates the secrets of `names` that do not exist yet, with the placeholder value; returns those it made.
fn placeholders(app: &AppHandle, scope: &Scope, names: &[String]) -> Result<Vec<String>, String> {
    let have = secrets::names(app, scope);
    let mut made: Vec<String> = vec![];
    for n in names.iter().filter(|n| !have.contains(n)) {
        if let Err(e) = secrets::set(app, scope, n, PLACEHOLDER) {
            for m in &made {
                let _ = secrets::secrets_delete(app.clone(), proj(scope), m.clone());
            }
            return Err(e);
        }
        made.push(n.clone());
    }
    Ok(made)
}

fn secrets_cmd(app: &AppHandle, cfg: &Config, verb: &str, a: &Args) -> Result<String, String> {
    a.only(&["project", "global", "yes"])?;
    let scope = a.scope(app)?;
    match verb {
        "list" => {
            let list: Vec<_> = secrets::names(app, &scope).into_iter().map(|n| serde_json::json!({ "name": n, "placeholder": secrets::raw_value(&scope, &n).as_deref() == Some(PLACEHOLDER) })).collect();
            json(&list)
        }
        "create" => {
            can_write(cfg, Some(&scope))?;
            let name = a.arg(2, "NAME")?;
            if secrets::names(app, &scope).iter().any(|n| n == name) {
                return Err("this secret exists already; its value is the user's, change it in the app".into());
            }
            placeholders(app, &scope, &[name.to_string()])?;
            Ok("created with a placeholder value: tell the user to set the real value in the app (the secrets page)".into())
        }
        "set" => Err("agents do not set secret values: use `secrets create <NAME>`, the user fills in the value in the app".into()),
        "delete" => {
            can_write(cfg, Some(&scope))?;
            a.yes("the secret")?;
            secrets::secrets_delete(app.clone(), proj(&scope), a.arg(2, "NAME")?.into())?;
            Ok("ok".into())
        }
        _ => unknown("secrets", verb),
    }
}

fn injections_cmd(app: &AppHandle, cfg: &Config, verb: &str, a: &Args, stdin: Option<&str>) -> Result<String, String> {
    a.only(&["project", "global", "yes"])?;
    let scope = a.scope(app)?;
    match verb {
        "get" => json(&injections::in_scope(app, &scope)),
        "upsert" => {
            can_write(cfg, Some(&scope))?;
            let v: serde_json::Value = body(stdin, "injection")?;
            let list: Vec<injections::Injection> = if v.is_array() { serde_json::from_value(v) } else { serde_json::from_value(v).map(|i| vec![i]) }.map_err(|e| format!("the injection in stdin is not valid: {e}"))?;
            injections::upsert(app, &scope, &list)?;
            Ok("ok".into())
        }
        "delete" => {
            can_write(cfg, Some(&scope))?;
            a.yes("the injection")?;
            let name = a.arg(2, "NAME")?;
            let mut list = injections::in_scope(app, &scope);
            if !list.iter().any(|i| i.name == name) {
                return Err("no such injection in this scope".into());
            }
            list.retain(|i| i.name != name);
            injections::injections_save(app.clone(), proj(&scope), list)?;
            Ok("ok".into())
        }
        _ => unknown("injections", verb),
    }
}

fn integrations_cmd(app: &AppHandle, cfg: &Config, verb: &str, a: &Args, stdin: Option<&str>) -> Result<String, String> {
    a.only(&["project", "global", "previous", "yes"])?;
    let scope = a.scope(app)?;
    match verb {
        "list" => json(&integrations::integrations_list(app.clone(), proj(&scope))?),
        "save" => {
            can_write(cfg, Some(&scope))?;
            let input: integrations::Input = body(stdin, "integration")?;
            if input.secrets.iter().any(|s| s.value.is_some()) {
                return Err("agents do not set secret values: send every secret with `\"value\": null`; new ones get a placeholder the user fills in the app".into());
            }
            let names: Vec<String> = input.secrets.iter().map(|s| s.name.clone()).collect();
            let made = placeholders(app, &scope, &names)?;
            if let Err(e) = integrations::integrations_save(app.clone(), proj(&scope), a.flag("previous").map(str::to_string), input) {
                for m in made {
                    let _ = secrets::secrets_delete(app.clone(), proj(&scope), m);
                }
                return Err(e);
            }
            Ok(if made.is_empty() { "ok".into() } else { format!("ok; placeholder secrets for the user to fill in: {}", made.join(", ")) })
        }
        "delete" => {
            can_write(cfg, Some(&scope))?;
            a.yes("the integration")?;
            integrations::integrations_delete(app.clone(), proj(&scope), a.arg(2, "NAME")?.into())?;
            Ok("ok".into())
        }
        _ => unknown("integrations", verb),
    }
}

fn images_cmd(app: &AppHandle, cfg: &Config, verb: &str, a: &Args, stdin: Option<&str>) -> Result<String, String> {
    a.only(&["project", "global", "id", "yes"])?;
    let scope = a.scope(app)?;
    match verb {
        "list" => json(&images::images_list(app.clone(), proj(&scope))?),
        "get" => {
            let id = a.arg(2, "id")?;
            let spec = images::read_specs(app, &scope).into_iter().find(|(i, _)| i == id).map(|(_, s)| s);
            json(&spec.ok_or("no such custom image in this scope (`base` is built in)")?)
        }
        "save" => {
            can_write(cfg, Some(&scope))?;
            let spec: images::Spec = body(stdin, "image spec")?;
            let id = images::images_save(app.clone(), proj(&scope), a.flag("id").map(str::to_string), spec)?;
            json(&serde_json::json!({ "id": id, "note": "not built yet: run `images build`" }))
        }
        "build" => {
            can_write(cfg, Some(&scope))?;
            images::images_build(app.clone(), proj(&scope), a.arg(2, "id")?.into())?;
            Ok("build started; check `images list` for its state".into())
        }
        "delete" => {
            can_write(cfg, Some(&scope))?;
            a.yes("the image")?;
            images::images_delete(app.clone(), proj(&scope), a.arg(2, "id")?.into())?;
            Ok("ok".into())
        }
        _ => unknown("images", verb),
    }
}

fn network_cmd(app: &AppHandle, cfg: &Config, verb: &str, a: &Args) -> Result<String, String> {
    a.only(&["project", "global"])?;
    let scope = a.scope(app)?;
    let mut net = network::network_get(app.clone(), proj(&scope))?;
    match verb {
        "get" => json(&net),
        "add" | "remove" => {
            can_write(cfg, Some(&scope))?;
            let hosts = &a.pos[2.min(a.pos.len())..];
            if hosts.is_empty() {
                return Err("missing <host>".into());
            }
            for h in hosts {
                if verb == "add" {
                    net.allow_hosts.push(h.clone());
                } else {
                    net.allow_hosts.retain(|x| x != h);
                }
            }
            json(&network::network_set(app.clone(), proj(&scope), net)?)
        }
        _ => unknown("network", verb),
    }
}

fn workflows_cmd(app: &AppHandle, cfg: &Config, verb: &str, a: &Args, stdin: Option<&str>) -> Result<String, String> {
    a.only(&["project", "global", "id", "name", "prompt", "yes"])?;
    let project = a.project(app)?;
    match verb {
        "list" => json(&wf::workflows_list(app.clone(), project)?),
        "get" => json(&wf::workflow_get(app.clone(), project, a.arg(2, "id")?.into())?),
        "standard" => json(&wf::workflow_standard(app.clone(), project)?.workflow),
        "save" => {
            can_write(cfg, None)?;
            let id = a.flag("id").unwrap_or("");
            // updating keeps what the command does not mention
            let old = (!id.is_empty()).then(|| wf::workflow_get(app.clone(), project.clone(), id.into())).transpose()?;
            let name = a.flag("name").map(str::to_string).or_else(|| old.as_ref().map(|o| o.name.clone())).ok_or("--name is required for a new workflow")?;
            let prompt = a.flag("prompt").map(str::to_string).or_else(|| old.as_ref().map(|o| o.system_prompt.clone())).unwrap_or_default();
            let workflow: Workflow = match (stdin.filter(|s| !s.trim().is_empty()), old) {
                (Some(_), _) => body(stdin, "workflow")?,
                (None, Some(o)) => o.workflow,
                (None, None) => return Err("a new workflow needs its JSON in `stdin` (start from `workflows standard`)".into()),
            };
            json(&wf::workflow_save(app.clone(), project, id.into(), name, workflow, prompt)?)
        }
        "delete" => {
            can_write(cfg, None)?;
            a.yes("the workflow")?;
            wf::workflow_delete(app.clone(), project, a.arg(2, "id")?.into())?;
            Ok("ok".into())
        }
        _ => unknown("workflows", verb),
    }
}

/// Starting work spends the user's subscription and boots VMs.
fn can_run(cfg: &Config) -> Result<(), String> {
    if cfg.runs { Ok(()) } else { Err("the user switched off starting sessions (Settings > MCP)".into()) }
}

/// A text given as `--<flag>` or in stdin.
fn text(a: &Args, flag: &str, stdin: Option<&str>) -> Option<String> {
    a.flag(flag).map(str::to_string).or_else(|| stdin.filter(|s| !s.trim().is_empty()).map(str::to_string))
}

fn sessions_cmd(app: &AppHandle, cfg: &Config, verb: &str, a: &Args, stdin: Option<&str>) -> Result<String, String> {
    a.only(&["project", "global", "workflow", "autonomy", "task", "text", "last", "approve", "reject"])?;
    let project = a.project(app)?;
    let id = || a.arg(2, "id").map(str::to_string);
    match verb {
        "list" => json(&wf::sessions_list(app.clone(), project)?),
        "get" => json(&eng::session_get(app.clone(), project, id()?)?),
        "log" => {
            let last: usize = a.flag("last").map_or(Ok(40), str::parse).map_err(|_| "--last is a number")?;
            // the agents' own messages are most of a log: left out, the rest tells what happened
            let log: Vec<_> = eng::session_log(app.clone(), project, id()?)?.into_iter().filter(|e| e["kind"] != "agent").collect();
            json(&log[log.len().saturating_sub(last.min(200))..])
        }
        "start" => {
            can_run(cfg)?;
            let task = text(a, "task", stdin).ok_or("the task goes in --task or stdin")?;
            let sid = wf::sessions_create(app.clone(), project.clone(), a.flag("workflow").map(str::to_string))?;
            let started = a.flag("autonomy").map_or(Ok(()), |m| wf::sessions_autonomy(app.clone(), project.clone(), sid.clone(), m.into())).and_then(|_| eng::session_start(app.clone(), project.clone(), sid.clone(), Some(task)));
            if let Err(e) = started {
                // a session that never started is not left behind
                let _ = wf::sessions_delete(app.clone(), project, sid);
                return Err(e);
            }
            json(&serde_json::json!({ "session": sid }))
        }
        "continue" => {
            can_run(cfg)?;
            eng::session_continue(app.clone(), project, id()?)?;
            Ok("ok".into())
        }
        "message" => {
            can_run(cfg)?;
            eng::session_message(app.clone(), project, id()?, text(a, "text", stdin).ok_or("the message goes in --text or stdin")?)?;
            Ok("ok".into())
        }
        "reply" => {
            can_run(cfg)?;
            let approved = match (a.bools.contains("approve"), a.bools.contains("reject")) {
                (true, true) => return Err("use either --approve or --reject".into()),
                (true, false) => Some(true),
                (false, true) => Some(false),
                _ => None,
            };
            eng::session_reply(app.clone(), project, id()?, eng::Reply { approved, text: text(a, "text", stdin).unwrap_or_default() })?;
            Ok("ok".into())
        }
        "cancel" => json(&serde_json::json!({ "cancelled": eng::session_cancel(app.clone(), project, id()?) })),
        _ => unknown("sessions", verb),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_like_a_shell() {
        assert_eq!(tokenize("a  b").unwrap(), ["a", "b"]);
        assert_eq!(tokenize(r#"variables set X "a b" --global"#).unwrap(), ["variables", "set", "X", "a b", "--global"]);
        assert_eq!(tokenize("x 'it''s' \"\"").unwrap(), ["x", "its", ""]);
        assert_eq!(tokenize(r#"say "a \"q\" b""#).unwrap(), ["say", r#"a "q" b"#]);
        assert!(tokenize("a 'b").is_err());
    }

    #[test]
    fn parses_flags_and_positionals() {
        let a = parse(tokenize("profiles get r1 --project p --yes --name=x").unwrap()).unwrap();
        assert_eq!(a.pos, ["profiles", "get", "r1"]);
        assert_eq!(a.flag("project"), Some("p"));
        assert_eq!(a.flag("name"), Some("x"));
        assert!(a.bools.contains("yes"));
        assert!(a.only(&["project", "yes", "name"]).is_ok());
        assert!(a.only(&["project"]).is_err());
        assert!(parse(tokenize("x --project").unwrap()).is_err());
    }

    #[test]
    fn writes_follow_the_switches() {
        let mut c = Config::default();
        assert!(can_write(&c, Some(&Scope::global())).is_ok());
        c.write_global = false;
        assert!(can_write(&c, Some(&Scope::global())).is_err());
        assert!(can_write(&c, Some(&Scope::project("p"))).is_ok());
        c.write = false;
        assert!(can_write(&c, None).is_err());
    }

    #[test]
    fn long_output_is_cut_on_a_char_boundary() {
        let s = "é".repeat(OUTPUT_LIMIT);
        assert!(cut(s).ends_with("output cut"));
    }
}
