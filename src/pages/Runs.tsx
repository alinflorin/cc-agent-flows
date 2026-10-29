import { useEffect, useRef, useState } from "react";
import { PageHeader } from "../components/PageHeader";
import { useTranslation } from "react-i18next";
import { Badge, Body1, Button, Caption1, Field, Select, Tab, TabList, Textarea, makeStyles, tokens } from "@fluentui/react-components";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";
import { call } from "../tauri";
import { useProjectId } from "../project";
import { fmtTime } from "../text";
import type { NetSpeed, NetTotals, Traffic, VmState, VmStats } from "../sessionLog";
import { NetStats, TrafficLines } from "./session/Calls";
import { Meters, VmPill } from "./session/Graph";
import { fmtBytes, fmtTokens } from "./session/types";
import { useStyles as useSessionStyles } from "./session/styles";

interface ProfileInfo {
  id: string;
  name: string;
}

type Msg =
  | { kind: "event"; body: { seq: number; event: Record<string, any> } }
  | { kind: "final"; body: { status: string; result: string; questions: string[]; tokens: number | null } };

interface Line {
  key: string;
  kind: "user" | "text" | "tool" | "final";
  text: string;
  status?: string;
}

const VM_STATES: VmState[] = ["queued", "preparing", "starting", "booting", "running", "off"];

/** What the run's VM and network have done so far, folded from the run's events (the same ones a session's log keeps). */
interface Info {
  vm: VmState;
  stats: VmStats | null;
  net: NetTotals;
  speed: NetSpeed;
  speeds: NetSpeed[];
  netT: number;
  traffic: Traffic[];
  tokens: number | null;
  status: string | null;
  started: number;
}

const fresh = (): Info => ({ vm: "queued", stats: null, net: { up: 0, down: 0, open: 0, connections: 0 }, speed: { up: 0, down: 0 }, speeds: [], netT: Date.now() / 1000, traffic: [], tokens: null, status: null, started: Date.now() / 1000 });

function fold(i: Info, msg: Msg): Info {
  const now = Date.now() / 1000;
  if (msg.kind === "final") return { ...i, vm: "off", speed: { up: 0, down: 0 }, tokens: msg.body.tokens, status: msg.body.status };
  const ev = msg.body.event;
  if (ev.type === "vm" && VM_STATES.includes(ev.state)) return { ...i, vm: ev.state };
  if (ev.type === "vm_stats") return { ...i, stats: { cpu: Number(ev.cpu) || 0, mem: Number(ev.mem_mb) || 0, memLimit: Number(ev.mem_limit_mb) || 0, cpus: Number(ev.cpus) || 1 } };
  if (ev.type === "net") {
    const up = Number(ev.up) || 0;
    const down = Number(ev.down) || 0;
    const secs = Math.max(1, now - i.netT);
    const speed = { up: Math.max(0, (up - i.net.up) / secs), down: Math.max(0, (down - i.net.down) / secs) };
    return { ...i, speed, speeds: [...i.speeds, speed].slice(-40), net: { up, down, open: Number(ev.open) || 0, connections: Number(ev.connections) || 0 }, netT: now };
  }
  if (ev.type === "egress") {
    const text = String(ev.text ?? "").replace(/^egress: /, "");
    const kind = /denied|refused|cannot|failed|budget/.test(text) ? "denied" : / gets /.test(text) ? "injected" : /allowed$/.test(text) ? "allowed" : "other";
    return { ...i, traffic: [...i.traffic, { t: now, kind, text }] };
  }
  return i;
}

const useStyles = makeStyles({
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM, maxWidth: "720px", marginBottom: tokens.spacingVerticalL },
  actions: { display: "flex", gap: tokens.spacingHorizontalS },
  feed: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalS, maxWidth: "900px" },
  line: { whiteSpace: "pre-wrap", wordBreak: "break-word", padding: tokens.spacingVerticalS, borderRadius: tokens.borderRadiusMedium, backgroundColor: tokens.colorNeutralBackground2 },
  bar: { display: "flex", alignItems: "center", flexWrap: "wrap", gap: tokens.spacingHorizontalM, marginBottom: tokens.spacingVerticalM },
  panel: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM, maxWidth: "720px", marginTop: tokens.spacingVerticalM },
  user: { backgroundColor: tokens.colorBrandBackground2 },
  tool: { fontFamily: tokens.fontFamilyMonospace, color: tokens.colorNeutralForeground3 },
});

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
  const ss = useSessionStyles();
  const notify = useNotify();
  const desktop = inTauri();
  const project = useProjectId();
  const [profiles, setProfiles] = useState<ProfileInfo[]>([]);
  const [profile, setProfile] = useState("");
  const [prompt, setPrompt] = useState("");
  const [run, setRun] = useState<string | null>(null);
  const [lines, setLines] = useState<Line[]>([]);
  const [finished, setFinished] = useState(true);
  const [info, setInfo] = useState<Info>(fresh);
  const [tab, setTab] = useState<"output" | "network">("output");
  const current = useRef<string | null>(null);

  useEffect(() => {
    if (!desktop) return;
    void call<ProfileInfo[]>("profiles_list", { project }).then((p) => {
      setProfiles(p);
      setProfile((cur) => cur || p[0]?.id || "");
    });
    let off: (() => void) | undefined;
    void import("@tauri-apps/api/event").then(({ listen }) =>
      listen<{ run: string; msg: Msg }>("agent-run", (e) => {
        if (e.payload.run !== current.current) return;
        setInfo((i) => fold(i, e.payload.msg));
        setLines((l) => [...l, ...linesOf(e.payload.run, e.payload.msg)]);
        if (e.payload.msg.kind === "final") {
          setFinished(true);
          // the agent answered: the box is ready for the next message (a failed run keeps the text so it can be sent again)
          if (e.payload.msg.body.status !== "failed") setPrompt("");
        }
      }).then((u) => (off = u)),
    );
    return () => off?.();
  }, [desktop, project]);

  const start = async () => {
    try {
      current.current = null;
      setLines([]);
      setInfo(fresh());
      setTab("output");
      const id = await call<string>("agent_run", { project, profile, prompt });
      setLines([{ key: `${id}-user`, kind: "user", text: prompt }]);
      current.current = id;
      setRun(id);
      setFinished(false);
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const cancel = () => run && void call("agent_run_cancel", { run });

  if (!desktop) return (<><PageHeader title={t("runs.title")} /><Body1 block>{t("app.desktopOnly")}</Body1></>);
  return (
    <>
      <PageHeader title={t("runs.title")} description={t("runs.hint")} />
      <div className={styles.form}>
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
          <Textarea
            rows={5}
            value={prompt}
            placeholder={t("runs.placeholder")}
            onChange={(_, d) => setPrompt(d.value)}
            onKeyDown={(e) => {
              // Enter sends, Shift+Enter adds a line (not while an input method is composing)
              if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault();
                if (profile && prompt.trim() && finished) void start();
              }
            }}
          />
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
      {run && (
        <>
          <div className={styles.bar}>
            <VmPill state={info.vm} count={info.vm === "off" ? 0 : 1} />
            {info.status && (
              <Badge appearance="filled" color={info.status === "done" ? "success" : info.status === "failed" ? "danger" : "warning"}>
                {t(`runs.status.${info.status}`)}
              </Badge>
            )}
            {info.tokens ? <Caption1>{t("session.tokens", { n: fmtTokens(info.tokens) })}</Caption1> : null}
            {info.net.up + info.net.down > 0 && <Caption1 title={t("session.net.title")}>↓ {fmtBytes(info.net.down)} · ↑ {fmtBytes(info.net.up)}</Caption1>}
            <Caption1>{fmtTime(info.started)}</Caption1>
          </div>
          <TabList selectedValue={tab} onTabSelect={(_, d) => setTab(d.value as "output" | "network")}>
            <Tab value="output">{t("runs.tabs.output")}</Tab>
            <Tab value="network">{t("runs.tabs.network")}</Tab>
          </TabList>
          {tab === "network" ? (
            <div className={styles.panel}>
              {info.stats && info.vm !== "off" && <Meters use={info.stats} />}
              <NetStats up={info.net.up} down={info.net.down} speed={info.speed} speeds={info.speeds} open={info.net.open} connections={info.net.connections} alive={info.vm !== "off"} />
              {info.traffic.length === 0 ? <Caption1 className={ss.muted}>{t("session.traffic.empty")}</Caption1> : <div className={ss.steps}><TrafficLines traffic={info.traffic} /></div>}
            </div>
          ) : (
            <div className={styles.feed}>
              {!finished && !lines.some((l) => l.kind !== "user") && <Caption1>{t("runs.booting")}</Caption1>}
              {lines.map((l) => (
                <div key={l.key} className={`${styles.line} ${l.kind === "tool" ? styles.tool : l.kind === "user" ? styles.user : ""}`}>
                  {l.kind === "final" && (
                    <Badge appearance="filled" color={l.status === "done" ? "success" : l.status === "failed" ? "danger" : "warning"}>
                      {t(`runs.status.${l.status}`)}
                    </Badge>
                  )}{" "}
                  {l.text}
                </div>
              ))}
            </div>
          )}
        </>
      )}
    </>
  );
}
