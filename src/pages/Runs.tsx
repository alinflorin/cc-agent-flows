import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Badge, Body1, Button, Caption1, Field, Select, Textarea, Title2, makeStyles, tokens } from "@fluentui/react-components";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";

interface ProfileInfo {
  id: string;
  name: string;
}

type Msg =
  | { kind: "event"; body: { seq: number; event: Record<string, any> } }
  | { kind: "final"; body: { status: string; result: string; questions: string[]; cost_usd: number | null } };

interface Line {
  key: string;
  kind: "text" | "tool" | "final";
  text: string;
  status?: string;
}

const useStyles = makeStyles({
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM, maxWidth: "720px", marginBottom: tokens.spacingVerticalL },
  actions: { display: "flex", gap: tokens.spacingHorizontalS },
  feed: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalS, maxWidth: "900px" },
  line: { whiteSpace: "pre-wrap", wordBreak: "break-word", padding: tokens.spacingVerticalS, borderRadius: tokens.borderRadiusMedium, backgroundColor: tokens.colorNeutralBackground2 },
  tool: { fontFamily: tokens.fontFamilyMonospace, color: tokens.colorNeutralForeground3 },
});

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

/** What the feed shows of one stream-json event: the assistant's text and the tools it calls. */
function linesOf(run: string, msg: Msg): Line[] {
  if (msg.kind === "final") {
    const f = msg.body;
    const text = [f.result, ...f.questions.map((q) => `? ${q}`)].filter(Boolean).join("\n");
    return [{ key: `${run}-final`, kind: "final", text, status: f.status }];
  }
  const ev = msg.body.event;
  if (ev.type !== "assistant") return [];
  const out: Line[] = [];
  (ev.message?.content ?? []).forEach((c: any, i: number) => {
    const key = `${run}-${msg.body.seq}-${i}`;
    if (c.type === "text" && c.text) out.push({ key, kind: "text", text: c.text });
    if (c.type === "tool_use") out.push({ key, kind: "tool", text: `${c.name} ${JSON.stringify(c.input).slice(0, 200)}` });
  });
  return out;
}

export function Runs() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();
  const [profiles, setProfiles] = useState<ProfileInfo[]>([]);
  const [profile, setProfile] = useState("");
  const [prompt, setPrompt] = useState("");
  const [run, setRun] = useState<string | null>(null);
  const [lines, setLines] = useState<Line[]>([]);
  const [finished, setFinished] = useState(true);
  const current = useRef<string | null>(null);

  useEffect(() => {
    if (!desktop) return;
    void call<ProfileInfo[]>("profiles_list").then((p) => {
      setProfiles(p);
      setProfile((cur) => cur || p[0]?.id || "");
    });
    let off: (() => void) | undefined;
    void import("@tauri-apps/api/event").then(({ listen }) =>
      listen<{ run: string; msg: Msg }>("agent-run", (e) => {
        if (e.payload.run !== current.current) return;
        setLines((l) => [...l, ...linesOf(e.payload.run, e.payload.msg)]);
        if (e.payload.msg.kind === "final") setFinished(true);
      }).then((u) => (off = u)),
    );
    return () => off?.();
  }, [desktop]);

  const start = async () => {
    try {
      current.current = null;
      setLines([]);
      const id = await call<string>("agent_run", { profile, prompt });
      current.current = id;
      setRun(id);
      setFinished(false);
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const cancel = () => run && void call("agent_run_cancel", { run });

  if (!desktop) return <Body1 block>{t("app.desktopOnly")}</Body1>;
  return (
    <>
      <Title2 as="h1" block>
        {t("runs.title")}
      </Title2>
      <div className={styles.form}>
        <Caption1>{t("runs.hint")}</Caption1>
        <Field label={t("wf.profile")}>
          <Select value={profile} onChange={(_, d) => setProfile(d.value)}>
            {profiles.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={t("runs.prompt")}>
          <Textarea rows={5} value={prompt} onChange={(_, d) => setPrompt(d.value)} />
        </Field>
        <div className={styles.actions}>
          <Button appearance="primary" disabled={!profile || !prompt.trim() || !finished} onClick={start}>
            {t("runs.start")}
          </Button>
          <Button disabled={finished} onClick={cancel}>
            {t("runs.cancel")}
          </Button>
        </div>
      </div>
      <div className={styles.feed}>
        {run && !finished && lines.length === 0 && <Caption1>{t("runs.booting")}</Caption1>}
        {lines.map((l) => (
          <div key={l.key} className={`${styles.line} ${l.kind === "tool" ? styles.tool : ""}`}>
            {l.kind === "final" && (
              <Badge appearance="filled" color={l.status === "done" ? "success" : l.status === "failed" ? "danger" : "warning"}>
                {t(`runs.status.${l.status}`)}
              </Badge>
            )}{" "}
            {l.text}
          </div>
        ))}
      </div>
    </>
  );
}
