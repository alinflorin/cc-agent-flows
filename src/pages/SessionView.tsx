import { useCallback, useEffect, useRef, useState } from "react";
import { Link, useParams } from "react-router";
import { useTranslation } from "react-i18next";
import { Badge, Body1, Button, Caption1, Field, Input, MessageBar, MessageBarBody, Textarea, Title2, makeStyles, tokens } from "@fluentui/react-components";
import { Edit16Regular } from "@fluentui/react-icons";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";

interface Pending {
  node: string;
  kind: "approve" | "input" | "questions";
  prompt: string;
  questions: string[];
}

interface View {
  session: { name: string; task: string; status: string };
  pending: Pending | null;
  live: boolean;
  cost_usd: number;
}

interface Entry {
  t: number;
  kind: string;
  body: Record<string, any>;
}

const useStyles = makeStyles({
  head: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalXS, marginBottom: tokens.spacingVerticalM },
  bar: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "center", flexWrap: "wrap", marginBottom: tokens.spacingVerticalM },
  feed: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalS, maxWidth: "900px" },
  box: { whiteSpace: "pre-wrap", wordBreak: "break-word", padding: tokens.spacingVerticalS, borderRadius: tokens.borderRadiusMedium, backgroundColor: tokens.colorNeutralBackground2 },
  step: { fontWeight: tokens.fontWeightSemibold, marginTop: tokens.spacingVerticalM },
  tool: { fontFamily: tokens.fontFamilyMonospace, color: tokens.colorNeutralForeground3 },
  muted: { color: tokens.colorNeutralForeground3 },
  ask: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalS, maxWidth: "720px", padding: tokens.spacingHorizontalM, border: `1px solid ${tokens.colorBrandStroke1}`, borderRadius: tokens.borderRadiusMedium, marginBottom: tokens.spacingVerticalL },
  title: { display: "flex", alignItems: "center", gap: tokens.spacingHorizontalS },
  user: { whiteSpace: "pre-wrap", wordBreak: "break-word", padding: tokens.spacingVerticalS, borderRadius: tokens.borderRadiusMedium, backgroundColor: tokens.colorBrandBackground2, alignSelf: "flex-end", maxWidth: "80%" },
  composer: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "flex-end", maxWidth: "900px", marginTop: tokens.spacingVerticalL },
  grow: { flexGrow: 1 },
  actions: { display: "flex", gap: tokens.spacingHorizontalS },
});

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

const statusColor = (s: string) =>
  s === "done" ? "success" : s === "failed" ? "danger" : s === "running" ? "brand" : s === "waiting" ? "warning" : "informative";

export function SessionView() {
  const { id = "", sid = "" } = useParams();
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const [view, setView] = useState<View | null>(null);
  const [log, setLog] = useState<Entry[]>([]);
  const [answer, setAnswer] = useState("");
  const [message, setMessage] = useState("");
  const [title, setTitle] = useState<string | null>(null);
  const end = useRef<HTMLDivElement>(null);

  const refresh = useCallback(async () => {
    try {
      setView(await call<View>("session_get", { project: id, id: sid }));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [id, sid, notify]);

  useEffect(() => {
    if (!inTauri()) return;
    let off: (() => void) | undefined;
    let gone = false;
    void (async () => {
      setLog(await call<Entry[]>("session_log", { project: id, id: sid }));
      await refresh();
      const { listen } = await import("@tauri-apps/api/event");
      const u = await listen<{ project: string; session: string; entry: Entry }>("session-log", (e) => {
        if (e.payload.project !== id || e.payload.session !== sid) return;
        setLog((l) => [...l, e.payload.entry]);
        if (e.payload.entry.kind === "status" || e.payload.entry.kind === "wait") void refresh();
      });
      if (gone) u();
      else off = u;
    })();
    return () => {
      gone = true;
      off?.();
    };
  }, [id, sid, refresh]);

  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [log.length]);

  const act = async (cmd: string, args: Record<string, unknown> = {}) => {
    try {
      await call(cmd, { project: id, id: sid, ...args });
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

  if (!inTauri()) return <Body1 block>{t("projects.desktopOnly")}</Body1>;
  if (!view) return null;
  const { session, pending, live } = view;
  const canStart = session.status === "new";
  const canContinue = !live && ["interrupted", "failed", "cancelled"].includes(session.status);

  const render = (e: Entry, i: number) => {
    const b = e.body;
    switch (e.kind) {
      case "step":
        return (
          <div key={i} className={styles.step}>
            ▶ {b.label} <Caption1>({b.profile}{b.instances > 1 ? ` ×${b.instances}` : ""})</Caption1>
          </div>
        );
      case "agent": {
        if (b.event?.type !== "assistant") return null;
        return (b.event.message?.content ?? []).map((c: any, j: number) => {
          const who = b.instances > 1 ? `[${b.instance}] ` : "";
          if (c.type === "text" && c.text) return <div key={`${i}-${j}`} className={styles.box}>{who}{c.text}</div>;
          if (c.type === "tool_use")
            return <div key={`${i}-${j}`} className={`${styles.box} ${styles.tool}`}>{who}{c.name} {JSON.stringify(c.input).slice(0, 200)}</div>;
          return null;
        });
      }
      case "step_end":
        return (
          <div key={i} className={styles.box}>
            <Badge appearance="filled" color={statusColor(b.status)}>{t(`runs.status.${b.status}`, b.status)}</Badge> {b.summary}
          </div>
        );
      case "decision":
        return (
          <div key={i} className={styles.box}>
            → <b>{b.label}</b>{b.instructions ? `: ${b.instructions}` : ""}{b.parts?.length ? ` (${b.parts.join(" | ")})` : ""}
          </div>
        );
      case "decision_rejected":
        return <Caption1 key={i} className={styles.muted} block>{t("session.rejected", { reason: b.reason })}</Caption1>;
      case "condition":
        return <Caption1 key={i} className={styles.muted} block>{t("session.condition", { field: b.field, result: String(b.result) })}</Caption1>;
      case "wait":
        return <Caption1 key={i} className={styles.muted} block>{t("session.waiting", { label: b.label })}</Caption1>;
      case "reply":
        return <div key={i} className={styles.box}>{b.approved === null || b.approved === undefined ? "" : b.approved ? `✔ ${t("session.approve")} ` : `✘ ${t("session.reject")} `}{b.text}</div>;
      case "error":
        return <MessageBar key={i} intent="error"><MessageBarBody>{b.reason}</MessageBarBody></MessageBar>;
      case "end":
        return <Caption1 key={i} className={styles.muted} block>{t("session.ended", { outcome: b.outcome })}</Caption1>;
      default:
        return null;
    }
  };

  return (
    <>
      <div className={styles.head}>
        <Caption1><Link to={`/projects/${id}`}>{t("session.back")}</Link></Caption1>
        {title === null ? (
          <div className={styles.title}>
            <Title2 as="h1" className={session.name ? undefined : styles.muted}>{session.name || t("sessions.untitled")}</Title2>
            <Button appearance="subtle" icon={<Edit16Regular />} aria-label={t("session.rename")} onClick={() => setTitle(session.name)} />
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
        {view.cost_usd > 0 && <Caption1>${view.cost_usd.toFixed(2)}</Caption1>}
        {canContinue && <Button appearance="primary" onClick={() => act("session_continue")}>{t("session.continue")}</Button>}
        {live && <Button onClick={() => act("session_cancel")}>{t("runs.cancel")}</Button>}
      </div>

      {pending && live && (
        <div className={styles.ask}>
          <Body1><b>{t(`session.ask.${pending.kind}`)}</b></Body1>
          {pending.kind === "questions" ? (
            <Body1 className={styles.box}>{pending.questions.join("\n") || pending.prompt}</Body1>
          ) : (
            <Body1 className={styles.box}>{pending.prompt}</Body1>
          )}
          <Field label={pending.kind === "approve" ? t("session.comment") : t("session.answer")}>
            <Textarea rows={3} value={answer} onChange={(_, d) => setAnswer(d.value)} />
          </Field>
          <div className={styles.actions}>
            {pending.kind === "approve" ? (
              <>
                <Button appearance="primary" onClick={() => act("session_reply", { reply: { approved: true, text: answer } })}>{t("session.approve")}</Button>
                <Button onClick={() => act("session_reply", { reply: { approved: false, text: answer } })}>{t("session.reject")}</Button>
              </>
            ) : (
              <Button appearance="primary" disabled={!answer.trim()} onClick={() => act("session_reply", { reply: { approved: null, text: answer } })}>{t("session.send")}</Button>
            )}
          </div>
        </div>
      )}

      <div className={styles.feed}>
        {session.task && <div className={styles.user}>{session.task}</div>}
        {canStart && <Body1 className={styles.muted}>{t("session.firstMessage")}</Body1>}
        {log.map(render)}
        <div ref={end} />
      </div>

      {canStart && (
        <div className={styles.composer}>
          <Textarea
            className={styles.grow}
            rows={3}
            autoFocus
            placeholder={t("session.placeholder")}
            value={message}
            onChange={(_, d) => setMessage(d.value)}
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
    </>
  );
}
