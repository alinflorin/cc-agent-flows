import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useParams } from "react-router";
import { useTranslation } from "react-i18next";
import { ReactFlowProvider } from "@xyflow/react";
import { Badge, Body1, Button, Caption1, Field, Input, MessageBar, MessageBarBody, Select, Subtitle2, Tab, TabList, Textarea, Title2 } from "@fluentui/react-components";
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
  const [message, setMessage] = useState("");
  const [title, setTitle] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab_>("chat");
  const [picked, setPicked] = useState<string | null>(null);
  const end = useRef<HTMLDivElement>(null);

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

  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [log.length, tab]);

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
  const sendMessage = () => {
    const text = message.trim();
    if (text) void act("session_message", { text });
  };
  const pickedCalls = [...d.calls.values()].filter((c) => c.node === picked).sort((a, b) => b.t - a.t);

  const reply = (approved: boolean | null) => act("session_reply", { reply: { approved, text: answer } });

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

          <div className={styles.scroll}>
            {tab === "chat" && (
              <>
                {session.task ? <div className={styles.user}>{session.task}</div> : <Body1 className={styles.muted}>{t("session.firstMessage")}</Body1>}
                {[session.task, ...view.messages].map((m, i) => (
                  <Fragment key={i}>
                    {i > 0 && <div className={styles.user}>{m}</div>}
                    {turnResult(i) && !(live && i === view.messages.length) && (
                      <div className={turnResult(i)?.status === "done" ? styles.outDone : styles.outFail}>{turnResult(i)?.text}</div>
                    )}
                  </Fragment>
                ))}
                {d.errors.map((e, i) => <MessageBar key={i} intent="error"><MessageBarBody>{e}</MessageBarBody></MessageBar>)}
                {pending && live && (
                  <div className={styles.ask}>
                    <Body1><b>{t(`session.ask.${pending.kind}`, { label: pending.label })}</b></Body1>
                    <Body1 className={styles.box}>{pending.kind === "questions" ? [pending.prompt, ...pending.questions.map((q) => `• ${q}`)].filter(Boolean).join("\n") : pending.prompt}</Body1>
                    <Field label={pending.kind === "approve" ? t("session.comment") : t("session.answer")}>
                      <Textarea rows={3} value={answer} onChange={(_, x) => setAnswer(x.value)} />
                    </Field>
                    <div className={styles.actions}>
                      {pending.kind === "approve" ? (
                        <>
                          <Button appearance="primary" onClick={() => reply(true)}>{t("session.approve")}</Button>
                          <Button onClick={() => reply(false)}>{t("session.reject")}</Button>
                        </>
                      ) : (
                        <Button appearance="primary" disabled={!answer.trim()} onClick={() => reply(null)}>{t("session.send")}</Button>
                      )}
                    </div>
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
                  <section className={styles.section}>
                    <Subtitle2>{t("session.logs")}</Subtitle2>
                    {pickedCalls.map((c) => <CallCard key={c.path} c={c} d={d} names={names} />)}
                  </section>
                  <section className={styles.section}>
                    <Subtitle2>{t("session.tabs.traffic")}</Subtitle2>
                    <TrafficView calls={pickedCalls} d={d} />
                  </section>
                </>
              ))}
            <div ref={end} />
          </div>

          {!canStart && (
            <div className={styles.composer}>
              <Textarea
                className={styles.grow}
                rows={2}
                placeholder={live ? t("session.followUpQueued") : t("session.followUp")}
                value={message}
                onChange={(_, x) => setMessage(x.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey && message.trim()) {
                    e.preventDefault();
                    sendMessage();
                  }
                }}
              />
              <Button appearance="primary" disabled={!message.trim()} onClick={sendMessage}>
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
