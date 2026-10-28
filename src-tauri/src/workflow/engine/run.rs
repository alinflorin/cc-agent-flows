//! The engine proper: `drive` walks the org chart, calling agents through the `Host`.

use super::*;

enum Stop {
    Cancelled,
    Fail(String),
}

type Res<T> = Result<T, Stop>;

struct Ctx<H: Host> {
    host: Arc<H>,
    wf: Workflow,
    /// The goal of the turn being run (the user's request, with the earlier turns for a follow-up).
    task: std::sync::RwLock<String>,
    cancel: watch::Receiver<bool>,
    state: Mutex<State>,
    visits: Mutex<HashMap<String, u32>>,
    /// Holding it is having the user's attention: one question at a time.
    human: tokio::sync::Mutex<mpsc::Receiver<Reply>>,
    vms: Semaphore,
    asked_user: AtomicU32,
}

fn reported(n: &Node, f: &Final) -> Reported {
    let status = match f.status {
        Status::Done => "done",
        Status::NeedsInput => "needs_input",
        Status::Failed => "failed",
    };
    Reported { label: label_of(n), status: status.into(), summary: f.result.clone(), questions: f.questions.clone() }
}

fn failure(n: &Node, why: impl Into<String>) -> Reported {
    Reported { label: label_of(n), status: "failed".into(), summary: why.into(), questions: vec![] }
}

impl<H: Host> Ctx<H> {
    fn cancelled(&self) -> bool {
        *self.cancel.borrow()
    }

    fn save(&self) {
        let (snap, status) = {
            let s = self.state.lock().unwrap();
            (s.clone(), if s.pending.is_some() { "waiting" } else { "running" })
        };
        self.host.checkpoint(&snap, status);
    }

    fn replayed(&self, key: &str) -> bool {
        self.state.lock().unwrap().journal.contains_key(key)
    }

    /// Runs an agent, or takes its result from the journal. The flag says it came from there.
    async fn run(&self, req: AgentReq) -> Res<(Final, bool)> {
        if self.cancelled() {
            return Err(Stop::Cancelled);
        }
        let journaled = self.state.lock().unwrap().journal.get(&req.call).map(|j| j.done.clone());
        if let Some(f) = journaled {
            return Ok((f, true));
        }
        {
            let mut s = self.state.lock().unwrap();
            if s.runs >= MAX_RUNS {
                return Err(Stop::Fail(format!("the session reached its limit of {MAX_RUNS} agent runs")));
            }
            s.runs += 1;
        }
        let key = req.call.clone();
        let permit = self.vms.acquire().await.map_err(|_| Stop::Cancelled)?;
        if self.cancelled() {
            return Err(Stop::Cancelled);
        }
        self.host.log("run_start", json!({"call": req.call, "node": req.node, "depth": req.depth, "profile": req.profile}));
        let (node, call_key) = (req.node.clone(), req.call.clone());
        let f = self.host.run_agent(req).await;
        drop(permit);
        if self.cancelled() {
            return Err(Stop::Cancelled);
        }
        self.host.log(
            "run_end",
            json!({"call": call_key, "node": node, "status": f.status, "tokens": f.tokens, "summary": clip(&f.result, 4000), "questions": f.questions, "assumptions": assumptions_of(f.output.as_ref())}),
        );
        {
            let mut s = self.state.lock().unwrap();
            s.tokens += f.tokens.unwrap_or(0);
            s.seq += 1;
            let seq = s.seq;
            s.journal.insert(key, Journaled { seq, done: f.clone() });
        }
        self.save();
        Ok((f, false))
    }

    /// Puts something in front of the user and waits for the answer. `gate`: an approval, which is
    /// asked whatever the autonomy; otherwise a question, refused (`None`) when the session is on
    /// `never` or too many have been asked. An answer given before (journaled) is reused.
    async fn ask_user(&self, pending: Pending, gate: bool) -> Res<Option<Reply>> {
        if !gate {
            self.asked_user.fetch_add(1, Ordering::SeqCst);
        }
        let known = self.state.lock().unwrap().replies.get(&pending.id).cloned();
        if let Some(r) = known {
            return Ok(Some(r));
        }
        if !gate && (self.host.autonomy() == "never" || self.asked_user.load(Ordering::SeqCst) > MAX_USER_QUESTIONS) {
            return Ok(None);
        }
        let mut rx = self.human.lock().await;
        let id = pending.id.clone();
        self.host.log("wait", json!({"id": id, "node": pending.node, "label": pending.label, "kind": pending.kind, "prompt": pending.prompt, "questions": pending.questions, "human": pending.human}));
        self.state.lock().unwrap().pending = Some(pending);
        self.save();
        let mut cancel = self.cancel.clone();
        let reply = loop {
            let r = tokio::select! {
                r = rx.recv() => r,
                _ = cancel.wait_for(|v| *v) => None,
            };
            match r {
                None => return Err(Stop::Cancelled),
                Some(r) if gate && r.approved.is_none() => continue, // an approval needs yes or no
                Some(r) => break r,
            }
        };
        while rx.try_recv().is_ok() {} // a second click must not answer the next question
        self.host.log("reply", json!({"id": id, "approved": reply.approved, "text": reply.text}));
        {
            let mut s = self.state.lock().unwrap();
            s.pending = None;
            s.replies.insert(id, reply.clone());
        }
        self.save();
        Ok(Some(reply))
    }
}

fn find<'a>(wf: &'a Workflow, id: &str) -> Res<&'a Node> {
    wf.nodes.iter().find(|n| n.id == id).ok_or_else(|| Stop::Fail(format!("the agent {id} does not exist (the workflow changed?)")))
}

type Call = Pin<Box<dyn Future<Output = Res<Reported>> + Send>>;

/// One call of an agent, to its end. `path` identifies the call in the journal; `parent` is the
/// agent that made it.
fn call<H: Host>(ctx: Arc<Ctx<H>>, node_id: String, path: String, asked: String, depth: usize, parent: Option<String>) -> Call {
    Box::pin(async move {
        let n = find(&ctx.wf, &node_id)?.clone();
        if depth > MAX_DEPTH {
            return Ok(failure(&n, format!("calls are nested more than {MAX_DEPTH} deep")));
        }
        {
            let mut v = ctx.visits.lock().unwrap();
            let c = v.entry(n.id.clone()).or_insert(0);
            if *c >= n.max_calls {
                return Ok(failure(&n, format!("{} was already called {c} times, its limit", label_of(&n))));
            }
            *c += 1;
        }
        let opts = options(&ctx.wf, &n.id);
        // A call that was already replayed from the journal says nothing again.
        let first_run = if opts.is_empty() { path.clone() } else { format!("{path}:1:1") };
        let started = !ctx.replayed(&first_run);
        let seq = ctx.state.lock().unwrap().seq;
        if started {
            ctx.host.log("call_start", json!({"call": path, "node": n.id, "label": label_of(&n), "depth": depth, "parent": parent, "asked": clip(&asked, 2000)}));
        }
        let res = if opts.is_empty() { worker(ctx.clone(), n.clone(), path.clone(), asked, depth).await } else { coordinator(ctx.clone(), n.clone(), opts, path.clone(), asked, depth).await };
        if let Ok(r) = &res
            && (started || ctx.state.lock().unwrap().seq != seq)
        {
            ctx.host.log("call_end", json!({"call": path, "node": n.id, "status": r.status, "summary": clip(&r.summary, 4000), "questions": r.questions}));
        }
        res
    })
}

fn request(n: &Node, call: &str, depth: usize, prompt: String, schema: &str) -> AgentReq {
    AgentReq { node: n.id.clone(), call: call.into(), depth, profile: n.profile.clone(), prompt, schema: schema.into(), timeout_mins: n.timeout_mins }
}

async fn worker<H: Host>(ctx: Arc<Ctx<H>>, n: Node, path: String, asked: String, depth: usize) -> Res<Reported> {
    let task = ctx.task.read().unwrap().clone();
    let mut asked = if depth == 0 { task.clone() } else { asked };
    let mut round = 0;
    loop {
        let key = if round == 0 { path.clone() } else { format!("{path}~{round}") };
        let (f, _) = ctx.run(request(&n, &key, depth, worker_prompt(&task, &n, &asked), crate::vm::protocol::RESULT_SCHEMA)).await?;
        let r = reported(&n, &f);
        // The root, or an agent linked to the user's node, puts its blocking questions to the user.
        if (depth == 0 || ctx.wf.ask_node(&n.id).is_some()) && r.status == "needs_input" && round < MAX_ROUNDS {
            let human = ctx.wf.ask_node(&n.id).map(|h| h.id.clone());
            let pending = Pending { id: format!("{key}:ask"), node: n.id.clone(), label: label_of(&n), kind: "questions".into(), prompt: clip(&r.summary, 2000), questions: r.questions.clone(), human };
            asked = match ctx.ask_user(pending, false).await? {
                Some(reply) => format!("{asked}\n\nYou stopped to ask:\n{}\n\nThe user answered:\n{}\n\nContinue with this answer.", r.questions.join("\n"), clip(reply.text.trim(), 20_000)),
                None => format!("{asked}\n\nThe user cannot be asked in this session: decide yourself and list your assumptions."),
            };
            round += 1;
            continue;
        }
        return Ok(r);
    }
}

async fn coordinator<H: Host>(ctx: Arc<Ctx<H>>, n: Node, opts: Vec<Opt>, path: String, asked: String, depth: usize) -> Res<Reported> {
    let task = ctx.task.read().unwrap().clone();
    let is_root = depth == 0;
    let may_ask = is_root || ctx.wf.ask_node(&n.id).is_some();
    let mut finished: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut notes = String::new();
    let mut log: Vec<String> = vec![];
    let mut inbox: Vec<Reported> = vec![];
    let mut feedback: Option<String> = None;
    let mut round = 0;
    loop {
        round += 1;
        if round > MAX_ROUNDS {
            return Ok(failure(&n, format!("{} took more than {MAX_ROUNDS} turns", label_of(&n))));
        }
        let can_ask_user = may_ask && ctx.host.autonomy() != "never";
        let mut attempt = 0;
        let (decision, replayed) = loop {
            attempt += 1;
            let key = format!("{path}:{round}:{attempt}");
            let prompt = coordinator_prompt(
                &n,
                &TurnContext { task: &task, asked: (!is_root).then_some(asked.as_str()), notes: &notes, log: &log, inbox: &inbox, options: &opts, is_root, may_ask, can_ask_user, feedback: feedback.as_deref() },
            );
            let (f, replayed) = ctx.run(request(&n, &key, depth, prompt, COORDINATOR_SCHEMA)).await?;
            if f.status == Status::Failed {
                return Ok(reported(&n, &f));
            }
            match parse_decision(f.output.as_ref(), &opts, &finished) {
                Ok(d) => break (d, replayed),
                Err(e) if attempt < DECISION_ATTEMPTS => {
                    if !replayed {
                        ctx.host.log("decision_rejected", json!({"node": n.id, "label": label_of(&n), "call": key, "reason": e}));
                    }
                    feedback = Some(e);
                }
                Err(e) => return Ok(failure(&n, format!("{} gave no valid decision: {e}", label_of(&n)))),
            }
        };
        feedback = None;
        if !decision.notes.trim().is_empty() {
            notes = decision.notes.clone();
        }
        if !replayed {
            let (action, detail) = match &decision.action {
                Action::Delegate(calls) => ("delegate", json!({"calls": calls.iter().map(|(o, i)| json!({"to": o.id, "label": o.label, "instructions": i})).collect::<Vec<_>>()})),
                Action::Finish { result, success } => ("finish", json!({"result": result, "success": success})),
                Action::Ask(q) => ("ask", json!({"questions": q})),
            };
            ctx.host.log("decision", json!({"call": path, "node": n.id, "label": label_of(&n), "depth": depth, "round": round, "action": action, "detail": detail, "notes": notes}));
        }

        match decision.action {
            Action::Finish { result, success } => {
                return Ok(Reported { label: label_of(&n), status: if success { "done" } else { "unmet" }.into(), summary: result, questions: vec![] });
            }
            Action::Ask(questions) => {
                if !may_ask {
                    return Ok(Reported { label: label_of(&n), status: "needs_input".into(), summary: format!("{} cannot go on without answers.", label_of(&n)), questions });
                }
                let pending = Pending { id: format!("{path}:{round}:ask"), node: n.id.clone(), label: label_of(&n), kind: "questions".into(), prompt: String::new(), questions: questions.clone(), human: ctx.wf.ask_node(&n.id).map(|h| h.id.clone()) };
                match ctx.ask_user(pending, false).await? {
                    Some(reply) => {
                        inbox = vec![Reported {
                                            label: "The user".into(),
                            status: "user".into(),
                            summary: format!("Your questions:\n{}\n\nThe user answered:\n{}", questions.join("\n"), clip(reply.text.trim(), 20_000)),
                            questions: vec![],
                        }];
                    }
                    None => feedback = Some("The user cannot be asked (this session does not allow it, or too many questions were asked). Decide yourself, list what you assumed in `assumptions`, and go on.".into()),
                }
            }
            Action::Delegate(calls) => {
                let mut set = tokio::task::JoinSet::new();
                for (i, (opt, instructions)) in calls.iter().cloned().enumerate() {
                    let ctx = ctx.clone();
                    let child_path = format!("{path}/{round}.{i}");
                    let from = n.clone();
                    set.spawn(async move {
                        if opt.gate.is_some() {
                            let pending = Pending {
                                id: format!("{child_path}:gate"),
                                node: opt.id.clone(),
                                label: opt.label.clone(),
                                kind: "approve".into(),
                                prompt: format!("{} wants to hand work to {}:\n\n{}", label_of(&from), opt.label, clip(&instructions, 3000)),
                                questions: vec![],
                                human: opt.gate.clone(),
                            };
                            let approved = ctx.ask_user(pending, true).await?.is_some_and(|r| r.approved == Some(true));
                            if !approved {
                                let summary = "The user did not approve this handover. Do not do it; choose another way or finish.".to_string();
                                return Ok((i, Reported { label: opt.label, status: "rejected".into(), summary, questions: vec![] }));
                            }
                        }
                        let r = call(ctx, opt.id.clone(), child_path, instructions, depth + 1, Some(from.id.clone())).await?;
                        Ok::<_, Stop>((i, r))
                    });
                }
                let mut results: Vec<Option<Reported>> = (0..calls.len()).map(|_| None).collect();
                while let Some(joined) = set.join_next().await {
                    match joined {
                        Ok(Ok((i, r))) => results[i] = Some(r),
                        Ok(Err(stop)) => return Err(stop),
                        Err(e) => return Err(Stop::Fail(format!("an agent task crashed: {e}"))),
                    }
                }
                inbox = Vec::new();
                for ((opt, instructions), r) in calls.iter().zip(results) {
                    let r = r.unwrap_or_else(|| failure(&n, "no result"));
                    if r.status == "done" {
                        finished.insert(opt.id.clone());
                    }
                    log.push(format!("- round {round}: {} ← {} → {}: {}", opt.label, clip(instructions, 160), r.status, clip(&r.summary, 200)));
                    inbox.push(r);
                }
            }
        }
    }
}

/// Runs the session until it ends. `state` is the journal of an earlier run (empty for a new one).
pub async fn drive<H: Host>(host: Arc<H>, wf: Workflow, task: String, state: State, cancel: watch::Receiver<bool>, replies: mpsc::Receiver<Reply>) -> (Ended, State) {
    let Some(root) = host.entry().filter(|e| wf.nodes.iter().any(|n| n.id == *e)).or_else(|| wf.chat_entry().map(|n| n.id.clone())) else {
        return (Ended::Failed("the workflow has no agent to start with: one that no other agent hands work to, or the one a trigger points to".into()), state);
    };
    let ctx = Arc::new(Ctx {
        host,
        wf,
        task: std::sync::RwLock::new(task.clone()),
        cancel,
        state: Mutex::new(state),
        visits: Mutex::new(HashMap::new()),
        human: tokio::sync::Mutex::new(replies),
        vms: Semaphore::new(MAX_VMS),
        asked_user: AtomicU32::new(0),
    });
    // The first turn is the user's request; each message they send afterwards is a turn of its own,
    // run by the root again with the earlier turns in front of it. Every turn has its own path in
    // the journal, so a resumed session replays them in order.
    let mut done: Vec<(String, String)> = vec![];
    let mut message = task.clone();
    let mut turn = 0usize;
    let ended = loop {
        if turn > 0 {
            *ctx.task.write().unwrap() = follow_up_goal(&task, &done, &message);
            ctx.host.log("turn", json!({"turn": turn, "message": message}));
        }
        ctx.visits.lock().unwrap().clear();
        let path = if turn == 0 { "r".to_string() } else { format!("t{turn}") };
        match call(ctx.clone(), root.clone(), path, String::new(), 0, None).await {
            Ok(r) => {
                {
                    let mut s = ctx.state.lock().unwrap();
                    s.result = Some(r.summary.clone());
                    s.turns = s.turns.max(turn as u32 + 1);
                }
                // `unmet`: the root decided the goal cannot be met. Nothing broke, the turn is over and
                // the user can answer it (a failed run, on the other hand, ends the session as failed).
                if r.status != "done" && r.status != "unmet" {
                    break Ended::Failed(r.summary);
                }
                done.push((message.clone(), r.summary));
            }
            Err(Stop::Cancelled) => break Ended::Cancelled,
            Err(Stop::Fail(e)) => break Ended::Failed(e),
        }
        match ctx.host.follow_up(turn) {
            Some(m) => {
                message = m;
                turn += 1;
            }
            None => break Ended::Done,
        }
    };
    let state = ctx.state.lock().unwrap().clone();
    (ended, state)
}

/// What the root is told on a follow-up turn: the first request, what came of each turn so far,
/// and the new message.
fn follow_up_goal(task: &str, done: &[(String, String)], message: &str) -> String {
    let mut s = format!("{}\n\nThis session is a conversation and the user has written again. What came of the earlier turns:", clip(task, 4_000));
    for (i, (ask, result)) in done.iter().enumerate() {
        if i == 0 {
            s.push_str(&format!("\n\nResult of the request above: {}", clip(result, 3_000)));
        } else {
            s.push_str(&format!("\n\nFollow-up {i}: {}\nResult: {}", clip(ask, 2_000), clip(result, 3_000)));
        }
    }
    s.push_str(&format!("\n\nThe user now writes (handle this one; build on the earlier work, do not redo it unless asked): {}", clip(message, 4_000)));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::{Edge, EdgeKind, NodeKind};

    /// A host whose agents are a function of the request.
    type Script = Box<dyn FnMut(&AgentReq) -> Final + Send>;

    struct Fake {
        script: Mutex<Script>,
        /// (node, call key, prompt) of every run, in the order they started.
        seen: Mutex<Vec<(String, String, String)>>,
        autonomy: Mutex<String>,
        /// The user's messages after the first.
        messages: Mutex<Vec<String>>,
        /// Keeps the cancel channel open.
        gone: Mutex<Option<watch::Sender<bool>>>,
    }

    impl Fake {
        fn new(f: impl FnMut(&AgentReq) -> Final + Send + 'static) -> Arc<Self> {
            Arc::new(Fake { script: Mutex::new(Box::new(f)), seen: Mutex::new(vec![]), autonomy: Mutex::new("ask".into()), messages: Mutex::new(vec![]), gone: Mutex::new(None) })
        }
        fn nodes(&self) -> Vec<String> {
            self.seen.lock().unwrap().iter().map(|s| s.0.clone()).collect()
        }
        fn prompt(&self, i: usize) -> String {
            self.seen.lock().unwrap()[i].2.clone()
        }
    }

    impl Host for Fake {
        async fn run_agent(&self, req: AgentReq) -> Final {
            self.seen.lock().unwrap().push((req.node.clone(), req.call.clone(), req.prompt.clone()));
            (self.script.lock().unwrap())(&req)
        }
        fn log(&self, _: &str, _: Value) {}
        fn checkpoint(&self, _: &State, _: &str) {}
        fn autonomy(&self) -> String {
            self.autonomy.lock().unwrap().clone()
        }
        fn follow_up(&self, i: usize) -> Option<String> {
            self.messages.lock().unwrap().get(i).cloned()
        }
    }

    fn fin(status: Status, summary: &str, output: Option<Value>) -> Final {
        Final { status, result: summary.into(), questions: vec![], session_id: None, tokens: Some(500), output, exit_code: Some(0) }
    }

    fn done(summary: &str) -> Final {
        fin(Status::Done, summary, None)
    }

    fn delegate(calls: &[(&str, &str)], notes: &str) -> Final {
        let calls: Vec<Value> = calls.iter().map(|(to, i)| json!({"to": to, "instructions": i})).collect();
        fin(Status::Done, "", Some(json!({"action": "delegate", "calls": calls, "notes": notes})))
    }

    fn finish(result: &str) -> Final {
        fin(Status::Done, "", Some(json!({"action": "finish", "result": result, "success": true, "notes": "done"})))
    }

    fn ask(questions: &[&str]) -> Final {
        fin(Status::Done, "", Some(json!({"action": "ask", "questions": questions, "notes": "blocked"})))
    }

    async fn run_with(host: &Arc<Fake>, wf: &Workflow, state: State, replies: Vec<Reply>) -> (Ended, State) {
        let (tx, rx) = mpsc::channel(16);
        for r in replies {
            tx.send(r).await.unwrap();
        }
        let (c, crx) = watch::channel(false);
        *host.gone.lock().unwrap() = Some(c);
        drive(host.clone(), wf.clone(), "build a thing".into(), state, crx, rx).await
    }

    async fn run(host: &Arc<Fake>, wf: &Workflow, replies: Vec<Reply>) -> (Ended, State) {
        run_with(host, wf, State::default(), replies).await
    }

    /// An org chart from `(from, to)` pairs; the nodes are the names in order of appearance.
    fn chart(edges: &[(&str, &str)]) -> Workflow {
        let mut seen = std::collections::HashSet::new();
        let nodes = edges
            .iter()
            .flat_map(|(a, b)| [*a, *b])
            .filter(|i| seen.insert(*i))
            .map(|id| Node { id: id.into(), label: id.into(), x: 0, y: 0, kind: NodeKind::Agent, profile: "dev".into(), prompt: String::new(), max_calls: 20, timeout_mins: 30, trigger: Default::default() })
            .collect();
        Workflow { nodes, edges: edges.iter().map(|(a, b)| Edge { id: format!("{a}-{b}"), from: (*a).into(), to: (*b).into(), kind: EdgeKind::Delegate }).collect() }
    }

    /// `wf` with the user's node `id` of `kind`, wired with delegate edges `from -> id` and `id -> to`.
    fn with_human(mut wf: Workflow, id: &str, kind: NodeKind, from: &str, to: Option<&str>) -> Workflow {
        wf.nodes.push(Node { id: id.into(), label: id.into(), x: 0, y: 0, kind, profile: String::new(), prompt: String::new(), max_calls: 20, timeout_mins: 30, trigger: Default::default() });
        let edge = |a: &str, b: &str| Edge { id: format!("{a}-{b}"), from: a.into(), to: b.into(), kind: EdgeKind::Delegate };
        wf.edges.push(edge(from, id));
        if let Some(to) = to {
            wf.edges.push(edge(id, to));
        }
        wf
    }

    #[tokio::test]
    async fn a_coordinator_loops_over_items_and_the_result_goes_back_up() {
        // orch -> pm -> lead -> {ba, dev}; the pm goes through two tickets one at a time
        let wf = chart(&[("orch", "pm"), ("pm", "lead"), ("lead", "ba"), ("lead", "dev")]);
        let (mut orch, mut pm, mut lead) = (0, 0, 0);
        let host = Fake::new(move |req| match req.node.as_str() {
            "orch" => {
                orch += 1;
                if orch == 1 { delegate(&[("pm", "deliver PROJ-1 and PROJ-2")], "waiting for pm") } else { finish("both delivered") }
            }
            "pm" => {
                pm += 1;
                match pm {
                    1 => delegate(&[("lead", "PROJ-1")], "ticket 1 started"),
                    2 => delegate(&[("lead", "PROJ-2")], "ticket 1 done, ticket 2 started"),
                    _ => finish("PROJ-1 and PROJ-2 done"),
                }
            }
            "lead" => {
                lead += 1;
                match lead % 3 {
                    1 => delegate(&[("ba", "write the requirements")], "ba asked"),
                    2 => delegate(&[("dev", "implement it")], "dev asked"),
                    _ => finish("delivered"),
                }
            }
            other => done(&format!("{other} finished")),
        });
        let (ended, state) = run(&host, &wf, vec![]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(state.result.as_deref(), Some("both delivered"));
        assert_eq!(host.nodes(), ["orch", "pm", "lead", "ba", "lead", "dev", "lead", "pm", "lead", "ba", "lead", "dev", "lead", "pm", "orch"]);
        // a coordinator gets the result of what it called, its own notes and what it handed out
        let pm_second = host.prompt(7);
        assert!(pm_second.contains("delivered") && pm_second.contains("ticket 1 started") && pm_second.contains("PROJ-1"));
        // workers know the goal, their role and only what they were told
        let ba = host.prompt(3);
        assert!(ba.contains("build a thing") && ba.contains("write the requirements") && !ba.contains("PROJ-2"));
        assert_eq!(state.tokens, 7500);
    }

    #[tokio::test]
    async fn a_follow_up_message_is_another_turn_of_the_root() {
        let wf = chart(&[("root", "dev")]);
        let mut n = 0;
        let host = Fake::new(move |_| {
            n += 1;
            finish(&format!("answer {n}"))
        });
        host.messages.lock().unwrap().push("now fix the typo".into());
        let (ended, state) = run(&host, &wf, vec![]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!((state.turns, state.result.as_deref()), (2, Some("answer 2")));
        assert!(host.prompt(0).contains("build a thing") && !host.prompt(0).contains("fix the typo"));
        let second = host.prompt(1);
        assert!(second.contains("build a thing") && second.contains("answer 1") && second.contains("now fix the typo"));
        // resumed with one more message: the earlier turns are replayed, only the new one runs
        host.messages.lock().unwrap().push("and update the docs".into());
        let before = host.seen.lock().unwrap().len();
        let (ended, state) = run_with(&host, &wf, state, vec![]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!((host.seen.lock().unwrap().len() - before, state.turns), (1, 3));
        assert!(host.prompt(before).contains("and update the docs") && host.prompt(before).contains("answer 2"));
    }

    #[tokio::test]
    async fn calls_in_one_decision_run_in_parallel() {
        let wf = chart(&[("root", "dev")]);
        let mut n = 0;
        let host = Fake::new(move |req| match req.node.as_str() {
            "root" => {
                n += 1;
                if n == 1 { delegate(&[("dev", "api"), ("dev", "ui")], "two devs") } else { finish("ok") }
            }
            _ => done("built"),
        });
        let (ended, _) = run(&host, &wf, vec![]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(host.nodes().iter().filter(|n| *n == "dev").count(), 2);
        assert_eq!(host.prompt(3).matches("### dev (done)").count(), 2, "both results come back together");
    }

    #[tokio::test]
    async fn a_decision_outside_the_chart_is_refused_then_fails_the_call() {
        let wf = chart(&[("root", "dev")]);
        let host = Fake::new(|_| delegate(&[("qa", "test")], ""));
        let (ended, _) = run(&host, &wf, vec![]).await;
        assert!(matches!(ended, Ended::Failed(r) if r.contains("no valid decision") && r.contains("qa")));
        assert_eq!(host.nodes().len(), 2, "one retry, then it gives up");
        assert!(host.prompt(1).contains("refused") && host.prompt(1).contains("may not call"));

        // a first bad answer followed by a good one goes through
        let mut n = 0;
        let host = Fake::new(move |req| {
            n += 1;
            match (req.node.as_str(), n) {
                ("root", 1) => delegate(&[("qa", "x")], ""),
                ("root", 2) => delegate(&[("dev", "x")], "n"),
                ("root", _) => finish("ok"),
                _ => done("built"),
            }
        });
        assert_eq!(run(&host, &wf, vec![]).await.0, Ended::Done);
    }

    #[tokio::test]
    async fn a_subordinate_question_goes_to_its_caller_which_may_answer() {
        let wf = chart(&[("root", "lead"), ("lead", "ba")]);
        let (mut lead, mut ba) = (0, 0);
        let host = Fake::new(move |req| match req.node.as_str() {
            "root" => {
                if req.prompt.contains("Results since") { finish("done") } else { delegate(&[("lead", "go")], "") }
            }
            "lead" => {
                lead += 1;
                match lead {
                    1 => delegate(&[("ba", "write requirements")], ""),
                    2 => delegate(&[("ba", "write requirements; the limit is 10 items")], ""),
                    _ => finish("delivered"),
                }
            }
            _ => {
                ba += 1;
                if ba == 1 { Final { questions: vec!["What is the limit?".into()], ..fin(Status::NeedsInput, "blocked", None) } } else { done("requirements written") }
            }
        });
        let (ended, state) = run(&host, &wf, vec![]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(host.nodes(), ["root", "lead", "ba", "lead", "ba", "lead", "root"]);
        assert!(host.prompt(3).contains("What is the limit?") && host.prompt(3).contains("needs_input"));
        assert!(state.pending.is_none() && state.replies.is_empty(), "the user was not bothered");
    }

    #[tokio::test]
    async fn a_question_that_reaches_the_root_goes_to_the_user() {
        let wf = chart(&[("root", "dev")]);
        let mut n = 0;
        let host = Fake::new(move |_| {
            n += 1;
            if n == 1 { ask(&["Which database?"]) } else { finish("used postgres") }
        });
        let (ended, state) = run(&host, &wf, vec![Reply { approved: None, text: "postgres".into() }]).await;
        assert_eq!(ended, Ended::Done);
        assert!(host.prompt(1).contains("Which database?") && host.prompt(1).contains("postgres"));
        assert_eq!(state.replies.len(), 1);

        // never: nobody is asked, the root is told to assume
        let mut n = 0;
        let host = Fake::new(move |_| {
            n += 1;
            if n == 1 { ask(&["Which database?"]) } else { finish("assumed") }
        });
        *host.autonomy.lock().unwrap() = "never".into();
        let (ended, state) = run(&host, &wf, vec![]).await;
        assert_eq!(ended, Ended::Done);
        assert!(host.prompt(0).contains("decide yourself"));
        assert!(host.prompt(1).contains("cannot be asked") && state.replies.is_empty());
    }

    #[tokio::test]
    async fn a_lone_agent_that_needs_input_asks_the_user_and_runs_again() {
        let wf = Workflow { nodes: chart(&[("a", "b")]).nodes.into_iter().filter(|n| n.id == "b").collect(), edges: vec![] };
        let mut n = 0;
        let host = Fake::new(move |_| {
            n += 1;
            if n == 1 { Final { questions: vec!["Name?".into()], ..fin(Status::NeedsInput, "need a name", None) } } else { done("hello Ana") }
        });
        let (ended, state) = run(&host, &wf, vec![Reply { approved: None, text: "Ana".into() }]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(state.result.as_deref(), Some("hello Ana"));
        assert!(host.prompt(1).contains("Name?") && host.prompt(1).contains("Ana"));
    }

    #[tokio::test]
    async fn an_approval_gate_waits_for_the_user() {
        // root -> gate -> dev: the direct edge is replaced by the gate
        let wf = with_human(Workflow { nodes: chart(&[("root", "dev")]).nodes, edges: vec![] }, "gate", NodeKind::Approve, "root", Some("dev"));
        let mut n = 0;
        let host = Fake::new(move |req| match req.node.as_str() {
            "root" => {
                n += 1;
                if n == 1 { delegate(&[("dev", "merge it")], "") } else { finish("stopped") }
            }
            _ => done("merged"),
        });
        *host.autonomy.lock().unwrap() = "never".into(); // the gate asks anyway
        let (ended, _) = run(&host, &wf, vec![Reply { approved: Some(false), text: "not yet".into() }]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(host.nodes(), ["root", "root"], "the dev never ran");
        assert!(host.prompt(0).contains("approves each handover") && host.prompt(1).contains("did not approve"));

        let mut n = 0;
        let host = Fake::new(move |req| match req.node.as_str() {
            "root" => {
                n += 1;
                if n == 1 { delegate(&[("dev", "merge it")], "") } else { finish("merged") }
            }
            _ => done("merged"),
        });
        let (_, state) = run(&host, &wf, vec![Reply { approved: Some(true), text: String::new() }]).await;
        assert_eq!(host.nodes(), ["root", "dev", "root"]);
        assert_eq!(state.replies.len(), 1);
    }

    #[tokio::test]
    async fn only_agents_linked_to_the_user_ask_directly() {
        // lead -> ba -> you: the analyst asks the user itself; the architect's question goes to the lead
        let wf = with_human(chart(&[("lead", "ba"), ("lead", "arch"), ("root", "lead")]), "you", NodeKind::Ask, "ba", None);
        let (mut lead, mut ba, mut arch) = (0, 0, 0);
        let host = Fake::new(move |req| match req.node.as_str() {
            "root" => {
                if req.prompt.contains("Results since") { finish("done") } else { delegate(&[("lead", "go")], "") }
            }
            "lead" => {
                lead += 1;
                match lead {
                    1 => delegate(&[("ba", "requirements"), ("arch", "design")], ""),
                    _ => finish("delivered"),
                }
            }
            "ba" => {
                ba += 1;
                if ba == 1 { Final { questions: vec!["Who signs off?".into()], ..fin(Status::NeedsInput, "blocked", None) } } else { done("written") }
            }
            _ => {
                arch += 1;
                if arch == 1 { Final { questions: vec!["Which cloud?".into()], ..fin(Status::NeedsInput, "blocked", None) } } else { done("designed") }
            }
        });
        let (ended, state) = run(&host, &wf, vec![Reply { approved: None, text: "the CTO".into() }]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(state.replies.len(), 1, "only the analyst's question reached the user");
        let ba_again = host.seen.lock().unwrap().iter().filter(|s| s.0 == "ba").map(|s| s.2.clone()).collect::<Vec<_>>();
        assert!(ba_again[1].contains("Who signs off?") && ba_again[1].contains("the CTO"));
        let lead_second = host.seen.lock().unwrap().iter().filter(|s| s.0 == "lead").nth(1).unwrap().2.clone();
        assert!(lead_second.contains("Which cloud?"), "the architect's question went up to the lead");
        assert!(!lead_second.contains("Who signs off?"));
    }

    #[tokio::test]
    async fn order_arrows_make_a_coordinator_follow_the_order() {
        let mut wf = chart(&[("lead", "arch"), ("lead", "dev")]);
        wf.edges.push(Edge { id: "arch-dev-before".into(), from: "arch".into(), to: "dev".into(), kind: EdgeKind::Before });
        let mut n = 0;
        let host = Fake::new(move |req| match req.node.as_str() {
            "lead" => {
                n += 1;
                match n {
                    1 => delegate(&[("dev", "just build it")], "skipping the design"), // refused
                    2 => delegate(&[("arch", "design"), ("dev", "build")], ""),           // same answer: refused too
                    3 => delegate(&[("arch", "design")], ""),
                    4 => delegate(&[("dev", "build")], ""),
                    _ => finish("shipped"),
                }
            }
            other => done(&format!("{other} done")),
        });
        // the lead is the root: two refusals in a row end the call, so give it a chance on each turn
        let (ended, _) = run(&host, &wf, vec![]).await;
        assert!(matches!(&ended, Ended::Failed(r) if r.contains("no valid decision") && r.contains("arch")), "{ended:?}");
        assert!(host.prompt(1).contains("cannot be called yet") && host.prompt(1).contains("arch"));
        assert!(host.prompt(0).contains("only after arch finished"));
        assert_eq!(host.nodes(), ["lead", "lead"]);

        // following the order works, and a later turn may call dev again (the loop after a review)
        let mut n = 0;
        let host = Fake::new(move |req| match req.node.as_str() {
            "lead" => {
                n += 1;
                match n {
                    1 => delegate(&[("arch", "design")], ""),
                    2 => delegate(&[("dev", "build")], ""),
                    3 => delegate(&[("dev", "fix the review comments")], ""),
                    _ => finish("shipped"),
                }
            }
            other => done(&format!("{other} done")),
        });
        let (ended, _) = run(&host, &wf, vec![]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(host.nodes(), ["lead", "arch", "lead", "dev", "lead", "dev", "lead"]);
    }

    #[tokio::test]
    async fn limits_hold() {
        // an agent may be called only max_calls times; a coordinator that keeps going runs out of turns
        let mut wf = chart(&[("root", "dev")]);
        wf.nodes[1].max_calls = 2;
        let host = Fake::new(|req| if req.node == "root" { delegate(&[("dev", "again")], "") } else { done("x") });
        let (ended, _) = run(&host, &wf, vec![]).await;
        assert!(matches!(&ended, Ended::Failed(r) if r.contains("took more than")), "{ended:?}");
        assert_eq!(host.nodes().iter().filter(|n| *n == "dev").count(), 2);
        assert!(host.prompt(5).contains("its limit"), "the caller is told the limit was hit");

        // a failed worker is reported to its caller, which decides
        let wf = chart(&[("root", "dev")]);
        let mut n = 0;
        let host = Fake::new(move |req| match req.node.as_str() {
            "root" => {
                n += 1;
                if n == 1 { delegate(&[("dev", "x")], "") } else { fin(Status::Done, "", Some(json!({"action": "finish", "result": "gave up", "success": false, "notes": "n"}))) }
            }
            _ => fin(Status::Failed, "the VM died", None),
        });
        let (ended, _) = run(&host, &wf, vec![]).await;
        assert_eq!(ended, Ended::Done, "the root's own decision is a finished turn");
        assert!(host.prompt(2).contains("the VM died"));
    }

    #[tokio::test]
    async fn a_failed_session_continues_from_its_journal() {
        let wf = chart(&[("root", "dev")]);
        // the decisions depend on the round, so a second script makes the same ones
        let script = |dev_fails: bool| {
            Fake::new(move |req| match (req.node.as_str(), req.call.as_str()) {
                ("root", "r:1:1") => delegate(&[("dev", "a")], "first"),
                ("root", "r:2:1") => delegate(&[("dev", "b")], "second"),
                ("root", _) => finish("all done"),
                ("dev", call) if dev_fails && call == "r/2.0" => fin(Status::Failed, "boom", None),
                _ => done("built"),
            })
        };
        let first = script(true);
        let (ended, mut state) = run(&first, &wf, vec![]).await;
        assert_eq!(ended, Ended::Done, "the root finished anyway");
        assert_eq!(first.nodes(), ["root", "dev", "root", "dev", "root"]);

        // continuing forgets the failed run and what was decided after it
        state.rewind_failures();
        assert_eq!(state.journal.len(), 3, "root, dev a, root again");
        let again = script(false);
        let (ended, state) = run_with(&again, &wf, state, vec![]).await;
        assert_eq!(ended, Ended::Done);
        assert_eq!(again.nodes(), ["dev", "root"], "only the failed run and what came after start VMs");
        assert_eq!(state.journal.len(), 5);
    }

    #[tokio::test]
    async fn cancelling_stops_the_session() {
        let wf = chart(&[("root", "dev")]);
        let host = Fake::new(|_| done("x"));
        let (_tx, rx) = mpsc::channel(1);
        let (_c, crx) = watch::channel(true);
        let (ended, _) = drive(host.clone(), wf, "t".into(), State::default(), crx, rx).await;
        assert_eq!(ended, Ended::Cancelled);
        assert!(host.nodes().is_empty());
    }
}
