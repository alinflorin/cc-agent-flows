//! The real host: boots the VMs, keeps the session files, and the commands of the session page.

use super::*;

use crate::config::scope::Scope;

/// What a running session is, for the commands.
struct Live {
    cancel: watch::Sender<bool>,
    reply: mpsc::Sender<Reply>,
}

#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, Live>>);

/// VMs running in the whole app: sessions share them, a run waits for a free one.
pub struct VmSlots(Semaphore);

impl Default for VmSlots {
    fn default() -> Self {
        VmSlots(Semaphore::new(MAX_TOTAL_VMS))
    }
}

/// Sessions of different projects are told apart by project.
fn key(project: &str, sid: &str) -> String {
    format!("{project}/{sid}")
}

/// Whether a session of the project is running or waiting for the user.
pub fn project_busy(app: &AppHandle, project: &str) -> bool {
    let prefix = format!("{project}/");
    app.state::<Sessions>().0.lock().unwrap().keys().any(|k| k.starts_with(&prefix))
}

struct RealHost {
    app: AppHandle,
    project: String,
    root: PathBuf,
    sid: String,
    instructions: String,
    profiles: HashMap<String, Profile>,
    cancel: watch::Receiver<bool>,
    log_file: Mutex<Option<std::fs::File>>,
    entry: Option<String>,
}

impl RealHost {
    fn on_msg(&self, req: (&str, &str, usize), m: &Outbound) {
        let (node, call, depth) = req;
        // The partial-message stream is far too chatty to keep; whole messages are enough.
        if let Outbound::Event(e) = m
            && e.event.get("type").and_then(Value::as_str) != Some("stream_event")
        {
            self.log("agent", json!({"node": node, "call": call, "depth": depth, "seq": e.seq, "event": e.event}));
        }
    }
}

impl Host for RealHost {
    async fn run_agent(&self, req: AgentReq) -> Final {
        let Some(profile) = self.profiles.get(&req.profile) else {
            return failed(format!("the profile {} does not exist", req.profile));
        };
        let run_id = crate::vm::runs::new_run_id();
        let identity = crate::vm::runs::identity(&[("project", &self.project), ("session", &self.sid), ("step", &req.node)], &run_id);
        let (profile_env, profile_hosts) = match crate::config::integrations::expand(&self.app, &Scope::project(&self.project), profile) {
            Ok(v) => v,
            Err(e) => return failed(format!("the profile {}: {e}", req.profile)),
        };
        let (env, injections) = match crate::config::injections::prepare(&self.app, &Scope::project(&self.project), &profile_env) {
            Ok(v) => v,
            Err(e) => return failed(format!("the profile {}: {e}", req.profile)),
        };
        let task = Task {
            prompt: req.prompt,
            model: profile.model.clone(),
            effort: profile.effort.clone(),
            session_id: crate::vm::runs::uuid_v4(),
            resume: false,
            schema: req.schema,
            system_prompt: crate::workflow::combine_system_prompts(&self.instructions, &profile.system_prompt),
            env,
            timeout_secs: u64::from(req.timeout_mins) * 60,
        };
        let spec = Spec { run_id, project: Scope::project(&self.project), image: profile.image.clone(), identity, cpus: profile.cpus, memory_mb: profile.memory_mb, allow_hosts: profile_hosts, injections, task };
        // Cancelling the session cancels this VM.
        let (tx, rx) = oneshot::channel();
        let mut c = self.cancel.clone();
        let forward = tokio::spawn(async move {
            if c.wait_for(|v| *v).await.is_ok() {
                let _ = tx.send(());
            }
        });
        let slots = self.app.state::<VmSlots>();
        let mut gone = self.cancel.clone();
        let permit = tokio::select! {
            p = slots.0.acquire() => p.ok(),
            _ = gone.wait_for(|v| *v) => None,
        };
        let Some(_permit) = permit else {
            forward.abort();
            return failed("cancelled while waiting for a free VM");
        };
        let res = crate::vm::runs::run_vm(&self.app, spec, rx, |m| self.on_msg((&req.node, &req.call, req.depth), m)).await;
        forward.abort();
        res.unwrap_or_else(failed)
    }

    fn log(&self, kind: &str, body: Value) {
        let t = crate::util::unix_secs();
        let entry = json!({"t": t, "kind": kind, "body": body});
        if let Some(f) = self.log_file.lock().unwrap().as_mut() {
            let _ = writeln!(f, "{entry}");
        }
        let _ = self.app.emit("session-log", json!({"project": self.project, "session": self.sid, "entry": entry}));
    }

    fn checkpoint(&self, state: &State, status: &str) {
        if let Ok(path) = crate::workflow::session_file(&self.root, &self.sid, "state.json")
            && let Ok(text) = serde_json::to_string(state)
        {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
        let _ = crate::workflow::set_session_status(&self.root, &self.sid, status);
        self.log("status", json!({"status": status, "tokens": state.tokens}));
    }

    fn autonomy(&self) -> String {
        crate::workflow::read_session(&self.root, &self.sid).map_or_else(|_| "ask".into(), |s| s.autonomy)
    }

    fn follow_up(&self, i: usize) -> Option<String> {
        crate::workflow::read_session(&self.root, &self.sid).ok()?.messages.get(i).cloned()
    }

    fn entry(&self) -> Option<String> {
        self.entry.clone()
    }
}

pub fn read_state(root: &std::path::Path, sid: &str) -> Option<State> {
    serde_json::from_str(&std::fs::read_to_string(crate::workflow::session_file(root, sid, "state.json").ok()?).ok()?).ok()
}

/// Everything a run needs, checked before anything starts.
fn prepare(app: &AppHandle, project: &str, workflow: &str) -> Result<(Workflow, String, HashMap<String, Profile>), String> {
    let root = crate::workflow::root(app, project)?;
    let wf = crate::workflow::workflow(&root, workflow);
    wf.validate()?;
    let profiles_dir = crate::config::profiles::dir(app, project)?;
    let profiles: HashMap<String, Profile> = crate::config::profiles::list(&profiles_dir).into_iter().map(|e| (e.id, e.profile)).collect();
    let ids: Vec<String> = profiles.keys().cloned().collect();
    let problems = wf.problems(&ids);
    if !problems.is_empty() {
        let codes: Vec<String> = problems.iter().map(|p| format!("{} ({})", p.code, p.node.as_deref().unwrap_or("-"))).collect();
        return Err(format!("the workflow has problems: {}", codes.join(", ")));
    }
    for n in wf.nodes.iter().filter(|n| n.kind == crate::workflow::NodeKind::Agent) {
        let image = &profiles.get(&n.profile).ok_or_else(|| format!("the profile {} does not exist", n.profile))?.image;
        if crate::vm::images::path(app, &Scope::project(project), image).is_none() {
            return Err(format!("the image {image} (profile {}) is not built yet", n.profile));
        }
    }
    if !crate::config::accounts::usable(app) {
        return Err("no Claude account is ready: add a setup token in Settings > LLM".into());
    }
    // the project's system prompt comes before the workflow's
    let project_prompt = crate::config::projects::load(app, project).map(|p| p.system_prompt).unwrap_or_default();
    let instructions = crate::workflow::combine_system_prompts(&project_prompt, &crate::workflow::instructions(&root, workflow).system_prompt).unwrap_or_default();
    Ok((wf, instructions, profiles))
}

fn launch(app: AppHandle, project: String, sid: String, fresh: bool) -> Result<(), String> {
    let root = crate::workflow::root(&app, &project)?;
    let session = crate::workflow::read_session(&root, &sid)?;
    let k = key(&project, &sid);
    {
        let live = app.state::<Sessions>();
        let live = live.0.lock().unwrap();
        if live.contains_key(&k) {
            return Err("the session is already running".into());
        }
        if live.len() >= MAX_SESSIONS {
            return Err(format!("{MAX_SESSIONS} sessions are running already, wait for one to end"));
        }
    }
    // A finished session is continued by a message from the user (a new turn).
    match (session.status.as_str(), fresh) {
        ("new", true) | ("interrupted" | "failed" | "cancelled", false) => {}
        ("done", false) if session.messages.len() + 1 > read_state(&root, &sid).map_or(0, |s| s.turns as usize) => {}
        _ => return Err(format!("a session that is {} cannot be {}", session.status, if fresh { "started" } else { "continued" })),
    }
    let (wf, instructions, profiles) = prepare(&app, &project, &session.workflow)?;
    let mut state = if fresh { State::default() } else { read_state(&root, &sid).unwrap_or_default() };
    // A finished session replays all its turns as they were: only an unfinished one retries what failed.
    if session.status != "done" {
        state.rewind_failures();
    }

    let (cancel_tx, cancel_rx) = watch::channel(false);
    let (reply_tx, reply_rx) = mpsc::channel(4);
    app.state::<Sessions>().0.lock().unwrap().insert(k.clone(), Live { cancel: cancel_tx, reply: reply_tx });

    let log_file = crate::workflow::session_file(&root, &sid, "log.jsonl")
        .ok()
        .and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
    let host = Arc::new(RealHost {
        app: app.clone(),
        project: project.clone(),
        root: root.clone(),
        sid: sid.clone(),
        instructions,
        profiles,
        cancel: cancel_rx.clone(),
        log_file: Mutex::new(log_file),
        entry: session.entry.clone(),
    });
    let task = session.task;
    tauri::async_runtime::spawn(async move {
        host.log("started", json!({"continued": !fresh}));
        host.checkpoint(&state, "running");
        let (ended, state) = drive(host.clone(), wf, task, state, cancel_rx, reply_rx).await;
        let status = match &ended {
            Ended::Done => "done",
            Ended::Failed(reason) => {
                host.log("error", json!({"reason": reason}));
                "failed"
            }
            Ended::Cancelled => "cancelled",
        };
        // `checkpoint` writes the state and status; the final status is logged by it too
        host.checkpoint(&state, status);
        // A message that arrived as the session ended starts the next turn. Checked while holding
        // the lock `session_message` takes, so a message is never left unanswered.
        let more = {
            let live = host.app.state::<Sessions>();
            let mut live = live.0.lock().unwrap();
            let more = status == "done" && crate::workflow::read_session(&root, &sid).is_ok_and(|s| s.messages.len() + 1 > state.turns as usize);
            live.remove(&k);
            more
        };
        if more && let Err(e) = launch(host.app.clone(), project.clone(), sid.clone(), false) && !e.contains("already running") {
            host.log("error", json!({"reason": format!("could not answer the last message: {e}")}));
            let _ = crate::workflow::set_session_status(&root, &sid, "failed");
        }
    });
    Ok(())
}

/// Marks sessions that were running or waiting when the app closed as interrupted (run at startup).
pub fn recover(app: &AppHandle) {
    for project in crate::config::projects::ids(app) {
        let Ok(root) = crate::workflow::root(app, &project) else { continue };
        for s in crate::workflow::sessions(&root) {
            if matches!(s.session.status.as_str(), "running" | "waiting") {
                let _ = crate::workflow::set_session_status(&root, &s.id, "interrupted");
            }
        }
    }
}

/// Starts a new session; `task` (the user's first message) is saved first.
#[tauri::command]
pub fn session_start(app: AppHandle, project: String, id: String, task: Option<String>) -> Result<(), String> {
    if let Some(task) = task {
        crate::workflow::edit_session(&crate::workflow::root(&app, &project)?, &id, None, Some(&task))?;
    }
    launch(app, project, id, true)
}

/// Continues an interrupted, failed or cancelled session: what finished is replayed from the
/// journal, the rest runs again.
#[tauri::command]
pub fn session_continue(app: AppHandle, project: String, id: String) -> Result<(), String> {
    launch(app, project, id, false)
}

#[tauri::command]
pub fn session_cancel(app: AppHandle, project: String, id: String) -> bool {
    app.state::<Sessions>().0.lock().unwrap().get(&key(&project, &id)).is_some_and(|l| l.cancel.send(true).is_ok())
}

#[tauri::command]
pub fn session_reply(app: AppHandle, project: String, id: String, reply: Reply) -> Result<(), String> {
    if reply.text.len() > 64 * 1024 {
        return Err("the answer is too large".into());
    }
    let sessions = app.state::<Sessions>();
    let live = sessions.0.lock().unwrap();
    let l = live.get(&key(&project, &id)).ok_or("the session is not running")?;
    l.reply.try_send(reply).map_err(|_| "the session is not waiting for an answer".to_string())
}

/// A message from the user after the first one: starts a new turn of a finished session, or is
/// queued behind the work of a running one (the root answers it when the turn before it ends).
#[tauri::command]
pub fn session_message(app: AppHandle, project: String, id: String, text: String) -> Result<(), String> {
    let root = crate::workflow::root(&app, &project)?;
    let live = app.state::<Sessions>();
    {
        let live = live.0.lock().unwrap();
        crate::workflow::add_session_message(&root, &id, &text)?;
        if live.contains_key(&key(&project, &id)) {
            return Ok(());
        }
    }
    let n = crate::workflow::read_session(&root, &id)?.messages.len();
    launch(app, project, id.clone(), false).inspect_err(|_| {
        // not started (limits, a missing token...): the message is not kept
        let _ = crate::workflow::drop_session_message(&root, &id, n);
    })
}

#[derive(Serialize)]
pub struct SessionView {
    pub session: crate::workflow::Session,
    pub pending: Option<Pending>,
    pub live: bool,
    pub messages: Vec<String>,
    pub tokens: u64,
    pub result: Option<String>,
}

#[tauri::command]
pub fn session_get(app: AppHandle, project: String, id: String) -> Result<SessionView, String> {
    let root = crate::workflow::root(&app, &project)?;
    let session = crate::workflow::read_session(&root, &id)?;
    let state = read_state(&root, &id).unwrap_or_default();
    let live = app.state::<Sessions>().0.lock().unwrap().contains_key(&key(&project, &id));
    Ok(SessionView { messages: session.messages.clone(), session, pending: state.pending.filter(|_| live), live, tokens: state.tokens, result: state.result })
}

/// The session's log (the last entries when it is long).
#[tauri::command]
pub fn session_log(app: AppHandle, project: String, id: String) -> Result<Vec<Value>, String> {
    let root = crate::workflow::root(&app, &project)?;
    let text = std::fs::read_to_string(crate::workflow::session_file(&root, &id, "log.jsonl")?).unwrap_or_default();
    let entries: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let skip = entries.len().saturating_sub(5000);
    Ok(entries.into_iter().skip(skip).collect())
}

