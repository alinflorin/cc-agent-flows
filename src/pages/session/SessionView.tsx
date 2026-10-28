import { Follow } from "../../follow";
import { Fragment, useCallback, useEffect, useMemo, useState } from "react";
import { Link, useParams } from "react-router";
import { useTranslation } from "react-i18next";
import { ReactFlowProvider } from "@xyflow/react";
import { Badge, Body1, Button, Caption1, Field, Input, MessageBar, MessageBarBody, Select, Subtitle2, Tab, TabList, Textarea, Title2, mergeClasses } from "@fluentui/react-components";
import { Edit20Regular } from "@fluentui/react-icons";
import { inTauri } from "../../prefs";
import { useNotify } from "../../notifications";
import { clip, digest, type Entry } from "../../sessionLog";
import { call } from "../../tauri";
import { projectPath, useProjectId } from "../../project";
import { useStyles } from "./styles";
import { Tab_, View, WEdge, WNode, fmtTokens, statusColor } from "./types";
import { Graph } from "./Graph";
import { CallCard, Timeline, TrafficView } from "./Calls";

export function SessionView() {
  const { sid = "" } = useParams();
  const project = useProjectId();
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const [view, setView] = useState<View | null>(null);
  const [wf, setWf] = useState<{ nodes: WNode[]; edges: WEdge[] } | null>(null);
  const [workflows, setWorkflows] = useState<{ id: string; name: string }[]>([]);
  const [log, setLog] = useState<Entry[]>([]);
  const [answer, setAnswer] = useState("");
  // several questions: the answer drafted for each and the one being answered
  const [drafts, setDrafts] = useState<string[]>([]);
  const [sel, setSel] = useState(0);
  const pendingId = view?.pending?.id;
  useEffect(() => {
    setDrafts([]);
    setSel(0);
  }, [pendingId]);
  const [message, setMessage] = useState("");
  const [title, setTitle] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab_>("chat");
  const [picked, setPicked] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setView(await call<View>("session_get", { project, id: sid }));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [sid, project, notify]);

  useEffect(() => {
    if (!inTauri()) return;
    let off: (() => void) | undefined;
    let gone = false;
    void (async () => {
      try {
        setWorkflows(await call<{ id: string; name: string }[]>("workflows_list", { project }));
      } catch (e) {
        notify(String(e), "error");
      }
      setLog(await call<Entry[]>("session_log", { project, id: sid }));
      await refresh();
      const { listen } = await import("@tauri-apps/api/event");
      const u = await listen<{ project: string; session: string; entry: Entry }>("session-log", (e) => {
        if (e.payload.session !== sid || e.payload.project !== project) return;
        setLog((l) => [...l, e.payload.entry]);
        if (["status", "wait", "reply", "call_end"].includes(e.payload.entry.kind)) void refresh();
      });
      if (gone) u();
      else off = u;
    })();
    return () => {
      gone = true;
      off?.();
    };
  }, [sid, project, refresh, notify]);

  const wfId = view?.session.workflow;
  useEffect(() => {
    if (!inTauri() || !wfId) return;
    void (async () => {
      try {
        setWf((await call<{ workflow: { nodes: WNode[]; edges: WEdge[] } }>("workflow_get", { project, id: wfId })).workflow);
      } catch (e) {
        notify(String(e), "error");
      }
    })();
  }, [wfId, project, notify]);

  const names = useMemo(() => Object.fromEntries((wf?.nodes ?? []).map((n) => [n.id, n.label || n.profile || t("wf.kind.you")])), [wf, t]);
  const d = useMemo(() => digest(log, names), [log, names]);
  const vmAlive = useMemo(() => [...d.runs.values()].filter((r) => r.vm !== "off").length, [d]);

  const act = async (cmd: string, args: Record<string, unknown> = {}) => {
    try {
      await call(cmd, { project, id: sid, ...args });
      setAnswer("");
      setMessage("");
      await refresh();
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const rename = async () => {
    if (title === null) return;
    const name = title;
    setTitle(null);
    await act("sessions_rename", { name });
  };

  const pick = (node: string) => {
    setPicked(node);
    setTab("agent");
  };

  if (!inTauri()) return <Body1 block>{t("app.desktopOnly")}</Body1>;
  if (!view) return null;
  const { session, pending, live } = view;
  const canStart = session.status === "new";
  const canContinue = !live && ["interrupted", "failed", "cancelled"].includes(session.status);
  // what the root answered to the request and to each message after it, in order
  const turnResults = log
    .filter((e) => e.kind === "call_end" && typeof e.body.call === "string" && /^(r|t\d+)$/.test(e.body.call as string))
    .map((e) => ({ turn: e.body.call === "r" ? 0 : Number((e.body.call as string).slice(1)), status: e.body.status as string, text: String(e.body.summary ?? "") }));
  const turnResult = (i: number) => {
    const all = turnResults.filter((r) => r.turn === i);
    return all[all.length - 1];
  };
  // questions and approvals already put to the user, with the answers, by the turn they happened in
  const exchanges = (() => {
    const out: { turn: number; id: string; wait: Record<string, any>; reply?: Record<string, any> }[] = [];
    let turn = 0;
    for (const e of log) {
      if (e.kind === "call_start" && typeof e.body.call === "string" && /^(r|t\d+)$/.test(e.body.call)) turn = e.body.call === "r" ? 0 : Number(e.body.call.slice(1));
      else if (e.kind === "wait") out.push({ turn, id: String(e.body.id), wait: e.body });
      else if (e.kind === "reply") {
        const x = out.find((o) => o.id === String(e.body.id) && !o.reply);
        if (x) x.reply = e.body;
      }
    }
    return out.filter((x) => !(live && pending && x.id === pending.id));
  })();
  // while an agent's questions wait, the box at the bottom answers them; with several, the one picked in the list
  const answering = !!pending && live && pending.kind === "questions";
  const questions = answering ? pending.questions : [];
  const multi = questions.length > 1;
  const composed = multi ? (drafts[sel] ?? "") : answering ? answer : message;
  const anyDraft = multi && drafts.some((x) => x?.trim());
  const setComposed = (v: string) => {
    if (multi) setDrafts((d) => Object.assign([...questions.map((_, i) => d[i] ?? "")], { [sel]: v }));
    else if (answering) setAnswer(v);
    else setMessage(v);
  };
  const sendAll = () => {
    const text = questions.map((q, i) => `${q}\n→ ${drafts[i]?.trim() || "(not answered)"}`).join("\n\n");
    void act("session_reply", { reply: { approved: null, text } });
  };
  // Enter: with several questions it keeps the answer and moves on to the next open one, the last one sends them all
  const send = () => {
    if (!multi) return answering ? void reply(null) : sendMessage();
    const next = questions.findIndex((_, i) => i !== sel && !drafts[i]?.trim());
    if (next >= 0 && composed.trim()) setSel(next);
    else sendAll();
  };
  const sendMessage = () => {
    const text = message.trim();
    if (text) void act("session_message", { text });
  };
  const pickedCalls = [...d.calls.values()].filter((c) => c.node === picked).sort((a, b) => b.t - a.t);
  // the agent tab shows two panes that scroll on their own
  const follows = [d, view];
  const split = tab === "agent" && !!picked && pickedCalls.length > 0;

  const reply = async (approved: boolean | null) => {
    await act("session_reply", { reply: { approved, text: answer } });
    setAnswer("");
  };

  return (
    <>
      <div className={styles.head}>
        <Caption1><Link to={projectPath(project, "/sessions")}>{t("session.back")}</Link></Caption1>
        {title === null ? (
          <div className={styles.title}>
            <Title2 as="h1" className={session.name || session.task ? undefined : styles.muted}>{session.name || (session.task.length > 60 ? `${session.task.slice(0, 60)}…` : session.task) || t("sessions.untitled")}</Title2>
            <Button appearance="subtle" icon={<Edit20Regular />} aria-label={t("session.rename")} onClick={() => setTitle(session.name)} />
          </div>
        ) : (
          <Input
            autoFocus
            maxLength={120}
            placeholder={t("sessions.untitled")}
            value={title}
            onChange={(_, d) => setTitle(d.value)}
            onBlur={rename}
            onKeyDown={(e) => {
              if (e.key === "Enter") void rename();
              if (e.key === "Escape") setTitle(null);
            }}
          />
        )}
      </div>
      <div className={styles.bar}>
        <Badge appearance="filled" color={statusColor(session.status)}>{t(`sessions.status.${session.status}`, session.status)}</Badge>
        {view.tokens > 0 && <Caption1>{t("session.tokens", { n: fmtTokens(view.tokens) })}</Caption1>}
        {vmAlive > 0 && <Caption1>{t("session.vm.alive", { count: vmAlive })}</Caption1>}
        {canContinue && <Button appearance="primary" onClick={() => act("session_continue")}>{t("session.continue")}</Button>}
        {live && <Button onClick={() => act("session_cancel")}>{t("runs.cancel")}</Button>}
        {session.source !== "chat" && <Badge appearance="tint">{t(`session.source.${session.source}`)}</Badge>}
        <span className={styles.grow} />
        <Field className={styles.autonomy} orientation="horizontal" label={t("session.workflow")}>
          <Select
            value={session.workflow}
            disabled={!canStart}
            onChange={async (_, x) => {
              try {
                await call("sessions_workflow", { project, id: sid, workflow: x.value });
                await refresh();
              } catch (e) {
                notify(String(e), "error");
              }
            }}
          >
            {workflows.map((w) => (
              <option key={w.id} value={w.id}>
                {w.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field className={styles.autonomy} orientation="horizontal" label={t("session.autonomy.label")} title={t(`session.autonomy.${session.autonomy}Hint`)}>
          <Select value={session.autonomy} onChange={(_, x) => act("sessions_autonomy", { autonomy: x.value })}>
            <option value="ask">{t("session.autonomy.ask")}</option>
            <option value="never">{t("session.autonomy.never")}</option>
          </Select>
        </Field>
      </div>

      <div className={styles.body}>
        <div className={styles.canvas}>
          {wf && wf.nodes.length > 0 ? (
            <ReactFlowProvider>
              <Graph wf={wf} d={d} pending={live ? pending : null} picked={picked} onPick={pick} />
            </ReactFlowProvider>
          ) : (
            <Body1 block>{t("session.noWorkflow")}</Body1>
          )}
        </div>

        <div className={styles.side}>
          <TabList className={styles.tabs} size="small" selectedValue={tab} onTabSelect={(_, x) => setTab(x.value as Tab_)}>
            <Tab value="chat">{t("session.tabs.chat")}{pending && live ? " ●" : ""}</Tab>
            <Tab value="timeline">{t("session.tabs.timeline")}</Tab>
            <Tab value="agent">{picked ? clip(names[picked] ?? "", 18) : t("session.tabs.agent")}</Tab>
          </TabList>

          <Follow key={tab} deps={follows} className={mergeClasses(styles.scroll, split && styles.split)}>
            {tab === "chat" && (
              <>
                {session.task ? <div className={styles.user}>{session.task}</div> : <Body1 className={styles.muted}>{t("session.firstMessage")}</Body1>}
                {[session.task, ...view.messages].map((m, i) => (
                  <Fragment key={i}>
                    {i > 0 && <div className={styles.user}>{m}</div>}
                    {exchanges.filter((x) => x.turn === i).map((x) => (
                      <Fragment key={x.id}>
                        <Body1 className={styles.box}>
                          <b>{t(`session.ask.${x.wait.kind}`, { label: x.wait.label })}</b>
                          {"\n"}
                          {x.wait.kind === "questions" ? [x.wait.prompt, ...(x.wait.questions ?? []).map((q: string) => `• ${q}`)].filter(Boolean).join("\n") : x.wait.prompt}
                        </Body1>
                        {x.reply && (
                          <div className={styles.user}>
                            {[x.reply.approved === true ? t("session.approve") : x.reply.approved === false ? t("session.reject") : "", x.reply.text].filter(Boolean).join(": ")}
                          </div>
                        )}
                      </Fragment>
                    ))}
                    {turnResult(i) && !(live && i === view.messages.length) && (
                      <div className={turnResult(i)?.status === "done" ? styles.outDone : styles.outFail}>{turnResult(i)?.text}</div>
                    )}
                  </Fragment>
                ))}
                {d.errors.map((e, i) => <MessageBar key={i} intent="error"><MessageBarBody>{e}</MessageBarBody></MessageBar>)}
                {pending && live && (
                  <div className={styles.ask}>
                    <Body1><b>{t(`session.ask.${pending.kind}`, { label: pending.label })}</b></Body1>
                    {pending.kind === "questions" && pending.questions.length > 1 ? (
                      <>
                        {pending.prompt && <Body1 className={styles.box}>{pending.prompt}</Body1>}
                        {pending.questions.map((q, i) => (
                          <button key={i} type="button" className={mergeClasses(styles.question, i === sel && styles.questionSel)} onClick={() => setSel(i)}>
                            <span>{drafts[i]?.trim() ? "✓" : `${i + 1}.`}</span>
                            <span>{q}</span>
                          </button>
                        ))}
                      </>
                    ) : (
                      <Body1 className={styles.box}>{pending.kind === "questions" ? [pending.prompt, ...pending.questions.map((q) => `• ${q}`)].filter(Boolean).join("\n") : pending.prompt}</Body1>
                    )}
                    {pending.kind === "approve" && (
                      <Field label={t("session.comment")}>
                        <Textarea rows={3} value={answer} onChange={(_, x) => setAnswer(x.value)} />
                      </Field>
                    )}
                    {pending.kind === "approve" && (
                      <div className={styles.actions}>
                        <Button appearance="primary" onClick={() => reply(true)}>{t("session.approve")}</Button>
                        <Button onClick={() => reply(false)}>{t("session.reject")}</Button>
                      </div>
                    )}
                  </div>
                )}
                {view.result && !live && turnResults.length === 0 && (
                  <>
                    <Subtitle2>{t("session.result")}</Subtitle2>
                    <div className={styles.box}>{view.result}</div>
                  </>
                )}
              </>
            )}

            {tab === "timeline" &&
              (d.timeline.length === 0 ? <Caption1 className={styles.muted}>{t("session.noEvents")}</Caption1> : <Timeline items={d.timeline} onPick={pick} />)}

            {tab === "agent" &&
              (!picked ? (
                <Caption1 className={styles.muted}>{t("session.pickAgent")}</Caption1>
              ) : pickedCalls.length === 0 ? (
                <Caption1 className={styles.muted}>{t("session.noCalls")}</Caption1>
              ) : (
                <>
                  <section className={styles.pane} style={{ flex: "1 1 0" }}>
                    <Subtitle2>{t("session.logs")}</Subtitle2>
                    <Follow deps={follows} className={styles.paneBody}>{pickedCalls.map((c) => <CallCard key={c.path} c={c} d={d} names={names} />)}</Follow>
                  </section>
                  <section className={styles.pane} style={{ flex: "1 1 0" }}>
                    <Subtitle2>{t("session.tabs.traffic")}</Subtitle2>
                    <Follow deps={follows} className={styles.paneBody}>
                      <TrafficView calls={pickedCalls} d={d} />
                    </Follow>
                  </section>
                </>
              ))}
          </Follow>

          {!canStart && (
            <div className={styles.composer}>
              <Textarea
                className={styles.grow}
                rows={2}
                placeholder={answering ? t("session.answerPlaceholder") : live ? t("session.followUpQueued") : t("session.followUp")}
                value={composed}
                onChange={(_, x) => setComposed(x.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey && composed.trim()) {
                    e.preventDefault();
                    send();
                  }
                }}
              />
              <Button appearance="primary" disabled={multi ? !anyDraft && !composed.trim() : !composed.trim()} onClick={multi ? sendAll : send}>
                {t("session.send")}
              </Button>
            </div>
          )}
          {canStart && (
            <div className={styles.composer}>
              <Textarea
                className={styles.grow}
                rows={3}
                autoFocus
                placeholder={t("session.placeholder")}
                value={message}
                onChange={(_, x) => setMessage(x.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey && message.trim()) {
                    e.preventDefault();
                    void act("session_start", { task: message.trim() });
                  }
                }}
              />
              <Button appearance="primary" disabled={!message.trim()} onClick={() => act("session_start", { task: message.trim() })}>
                {t("session.send")}
              </Button>
            </div>
          )}
        </div>
      </div>
    </>
  );
}
