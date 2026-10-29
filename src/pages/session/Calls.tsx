import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Badge, Caption1, Spinner, mergeClasses } from "@fluentui/react-components";
import { ArrowRight16Regular, CheckmarkCircle16Filled, DismissCircle16Filled, ErrorCircle16Filled, Person16Regular, QuestionCircle16Filled } from "@fluentui/react-icons";
import { capitalize, fmtTime } from "../../text";
import { clip, type CallInfo, type Digest, type NetSpeed, type Run, type Traffic } from "../../sessionLog";
import { useStyles } from "./styles";
import { fmtBytes, fmtRate, fmtTokens } from "./types";
import { Meters, Spark, VmPill } from "./Graph";

export function Clamp({ text, className }: { text: string; className?: string }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const [open, setOpen] = useState(false);
  const long = text.length > 180 || text.split("\n").length > 3;
  return (
    <div className={className}>
      <div className={mergeClasses(styles.clamp, open && styles.open)}>{text}</div>
      {long && (
        <button type="button" className={styles.link} onClick={(e) => { e.stopPropagation(); setOpen(!open); }}>
          {open ? t("session.less") : t("session.more")}
        </button>
      )}
    </div>
  );
}

/** Two lines over the last reports: bytes per second received (brand) and sent (muted), on a scale of their own. */
function SpeedSpark({ samples }: { samples: NetSpeed[] }) {
  const styles = useStyles();
  if (samples.length < 2) return null;
  const max = Math.max(1, ...samples.flatMap((s) => [s.up, s.down]));
  const line = (pick: (s: NetSpeed) => number) => samples.map((s, i) => `${(i / (samples.length - 1)) * 100},${34 - (pick(s) / max) * 32}`).join(" ");
  return (
    <svg className={styles.spark} viewBox="0 0 100 36" preserveAspectRatio="none" aria-hidden>
      <polyline points={line((s) => s.up)} fill="none" stroke="currentColor" strokeOpacity="0.45" strokeWidth="1.5" vectorEffect="non-scaling-stroke" />
      <polyline points={line((s) => s.down)} fill="none" stroke="currentColor" strokeWidth="1.5" vectorEffect="non-scaling-stroke" />
    </svg>
  );
}

/** What the picked agent's network has carried: bytes in and out over its runs, how fast now, and the connections open. */
export function NetSummary({ calls, d }: { calls: CallInfo[]; d: Digest }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const runs = calls.flatMap((c) => c.runs).map((k) => d.runs.get(k)).filter((r): r is Run => !!r);
  const up = runs.reduce((a, r) => a + r.net.up, 0);
  const down = runs.reduce((a, r) => a + r.net.down, 0);
  const alive = runs.filter((r) => r.vm !== "off");
  const speed = alive.reduce((a, r) => ({ up: a.up + r.speed.up, down: a.down + r.speed.down }), { up: 0, down: 0 });
  const open = alive.reduce((a, r) => a + r.net.open, 0);
  const latest = [...alive].sort((a, b) => b.t - a.t)[0];
  return (
    <div className={styles.netGrid}>
      <div>
        <Caption1 className={styles.muted}>{t("session.net.received")}</Caption1>
        <div className={styles.netValue}>{fmtBytes(down)}</div>
        <Caption1 className={styles.muted}>{alive.length ? fmtRate(speed.down) : "–"}</Caption1>
      </div>
      <div>
        <Caption1 className={styles.muted}>{t("session.net.sent")}</Caption1>
        <div className={styles.netValue}>{fmtBytes(up)}</div>
        <Caption1 className={styles.muted}>{alive.length ? fmtRate(speed.up) : "–"}</Caption1>
      </div>
      <div>
        <Caption1 className={styles.muted}>{t("session.net.connections")}</Caption1>
        <div className={styles.netValue}>{open}</div>
        <Caption1 className={styles.muted}>{t("session.net.total", { count: runs.reduce((a, r) => a + r.net.connections, 0) })}</Caption1>
      </div>
      {latest && <div className={styles.netSpark}><SpeedSpark samples={latest.speeds} /></div>}
    </div>
  );
}

/** The picked agent's network traffic, live: what its VM asked the egress proxy for. */
export function TrafficView({ calls, d }: { calls: CallInfo[]; d: Digest }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const runs = calls
    .flatMap((c) => c.runs)
    .map((k) => d.runs.get(k))
    .filter((r): r is Run => !!r)
    .sort((a, b) => a.t - b.t);
  const all = runs.flatMap((r) => r.traffic);
  if (runs.length === 0) return <Caption1 className={styles.muted}>{t("session.noCalls")}</Caption1>;
  const count = (k: Traffic["kind"]) => all.filter((x) => x.kind === k).length;
  return (
    <>
      <NetSummary calls={calls} d={d} />
      <div className={styles.cardHead}>
        <Badge appearance="tint" color="success">{t("session.traffic.allowed", { count: count("allowed") })}</Badge>
        <Badge appearance="tint" color="danger">{t("session.traffic.denied", { count: count("denied") })}</Badge>
        <Badge appearance="tint" color="brand">{t("session.traffic.injected", { count: count("injected") })}</Badge>
      </div>
      {all.length === 0 && <Caption1 className={styles.muted}>{t("session.traffic.empty")}</Caption1>}
      {runs.map((r, n) =>
        r.traffic.length === 0 ? null : (
          <div key={r.key} className={styles.steps}>
            <Caption1 className={styles.muted}>
              {t("session.run")} {n + 1} · {fmtTime(r.t)}
            </Caption1>
            {r.traffic.map((x, i) => (
              <div key={i} className={mergeClasses(styles.step, styles.mono)}>
                <Caption1 className={styles.muted}>{fmtTime(x.t)} </Caption1>
                <Badge size="extra-small" appearance="filled" color={x.kind === "denied" ? "danger" : x.kind === "injected" ? "brand" : x.kind === "allowed" ? "success" : "informative"}>
                  {t(`session.traffic.kind.${x.kind}`)}
                </Badge>{" "}
                {clip(x.text, 300)}
              </div>
            ))}
          </div>
        ),
      )}
    </>
  );
}

const STATUS_STYLE: Record<string, "st_done" | "st_failed" | "st_wait" | "st_brand" | "st_muted"> = { done: "st_done", failed: "st_failed", needs_input: "st_wait", running: "st_brand", open: "st_brand", unmet: "st_muted" };

/** A call's or run's outcome as an icon and a coloured word, no box. */
function StatusText({ status, label }: { status: string; label: string }) {
  const styles = useStyles();
  const Icon = status === "done" ? CheckmarkCircle16Filled : status === "failed" ? DismissCircle16Filled : status === "needs_input" ? QuestionCircle16Filled : null;
  return (
    <span className={mergeClasses(styles.status, styles[STATUS_STYLE[status] ?? "st_muted"])}>
      {Icon ? <Icon /> : status === "running" || status === "open" ? <Spinner size="extra-tiny" /> : null}
      {label}
    </span>
  );
}

/** "A → B": who handed work to whom. */
function Route({ from, to }: { from: string; to: string }) {
  const styles = useStyles();
  return (
    <span className={styles.route}>
      {from}
      <ArrowRight16Regular className={styles.arrow} />
      {to}
    </span>
  );
}

export function CallCard({ c, d, names }: { c: CallInfo; d: Digest; names: Record<string, string> }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const open = c.status === "open";
  return (
    <div className={styles.card}>
      <div className={styles.cardHead}>
        <Route from={c.parent ? (names[c.parent] ?? c.parent) : t("session.ev.you")} to={names[c.node] ?? c.node} />
        <span className={styles.grow} />
        <StatusText status={open ? "running" : c.status} label={capitalize(open ? t("session.node.busy") : t(`runs.status.${c.status}`, c.status))} />
        <Caption1 className={styles.muted}>{fmtTime(c.t)}</Caption1>
      </div>
      {c.asked && <Clamp className={styles.quote} text={c.asked} />}

      {c.runs.map((k, n) => {
        const r = d.runs.get(k);
        if (!r) return null;
        return (
          <div key={k}>
            <div className={styles.cardHead}>
              <Caption1 className={styles.muted}>{t("session.run")} {n + 1}</Caption1>
              <StatusText status={r.status} label={capitalize(t(`runs.status.${r.status}`, r.status))} />
              <VmPill state={r.vm} />
              {r.tokens > 0 && <Caption1 className={styles.muted}>{t("session.tokens", { n: fmtTokens(r.tokens) })}</Caption1>}
            </div>
            {r.vm !== "off" && r.stats.length > 0 && (
              <>
                <Meters use={r.stats[r.stats.length - 1]} />
                <Spark samples={r.stats} />
              </>
            )}
            {r.vmLog.length > 1 && (
              <Caption1 block className={styles.muted}>
                {r.vmLog.map((x, i) => `${t(`session.vm.${x.state}`)}${i > 0 ? ` +${Math.max(0, x.t - r.vmLog[i - 1].t)}s` : ""}`).join(" → ")}
              </Caption1>
            )}
            {r.pieces.length > 0 && (
              <details open={r.status === "running" || undefined}>
                <summary className={styles.summaryRow}>{t("session.activity", { count: r.pieces.length })}</summary>
                <div className={styles.steps}>
                  {r.pieces.slice(-30).map((p, i) => (
                    <div key={i} className={mergeClasses(styles.step, p.kind === "tool" && styles.mono)}>{clip(p.text, 600)}</div>
                  ))}
                </div>
              </details>
            )}
            {r.assumptions.length > 0 && (
              <Clamp className={styles.muted} text={`${t("session.assumed")}: ${r.assumptions.join("; ")}`} />
            )}
          </div>
        );
      })}

      {c.decisions.map((x, i) => (
        <div key={i} className={styles.steps}>
          {x.action === "delegate" &&
            (x.detail.calls ?? []).map((k: any, j: number) => (
              <div key={j} className={styles.handoff}>
                <span className={styles.route}>
                  {names[c.node] ?? c.node}
                  <ArrowRight16Regular className={styles.arrow} />
                  {k.label}
                </span>
                <Clamp text={k.instructions} />
              </div>
            ))}
          {x.action === "finish" && (
            <div className={x.detail.success === false ? styles.outFail : styles.outDone}>
              <span className={styles.outLabel}>{t(x.detail.success === false ? "session.out.failed" : "session.out.done")}</span>
              <Clamp text={x.detail.result ?? ""} />
            </div>
          )}
          {x.action === "ask" && (
            <div className={styles.outAsk}>
              <span className={styles.outLabel}>{t("session.out.ask")}</span>
              <Clamp text={(x.detail.questions ?? []).map((q: string) => `? ${q}`).join("\n")} />
            </div>
          )}
          {x.notes && <Clamp className={styles.muted} text={`${t("session.notes")}: ${x.notes}`} />}
        </div>
      ))}
      {c.refused.map((r, i) => <Clamp key={i} className={styles.muted} text={t("session.refused", { reason: r })} />)}
      {!open && c.decisions.length === 0 && c.summary && (
        <div className={c.status === "failed" ? styles.outFail : styles.outDone}>
          <span className={styles.outLabel}>{t(c.status === "failed" ? "session.out.failed" : "session.out.done")}</span>
          <Clamp text={c.summary} />
        </div>
      )}
    </div>
  );
}

export function Timeline({ items, onPick }: { items: Digest["timeline"]; onPick: (node: string) => void }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const you = t("session.ev.you");
  const tone = (x: Digest["timeline"][number]) =>
    x.status === "done" ? "done" : x.status === "failed" || x.kind === "error" ? "failed" : x.status === "needs_input" || x.kind === "wait" ? "wait" : "brand";
  const icon = (x: Digest["timeline"][number]) =>
    x.kind === "start" ? <ArrowRight16Regular /> : x.kind === "reply" ? <Person16Regular /> : x.kind === "error" ? <ErrorCircle16Filled /> : x.kind === "wait" ? <QuestionCircle16Filled /> : x.status === "done" ? <CheckmarkCircle16Filled /> : x.status === "failed" ? <DismissCircle16Filled /> : <QuestionCircle16Filled />;
  const label = (x: Digest["timeline"][number]) => t(`session.ev.${x.kind}${x.kind === "end" ? `_${x.status}` : ""}`, "");
  // who sent it and who gets it: a handover goes from the caller to the agent, a result or a question goes the other way
  const route = (x: Digest["timeline"][number]) =>
    x.kind === "start" ? { from: x.from || you, to: x.who } : x.kind === "end" ? { from: x.who, to: x.to || you } : x.kind === "wait" ? { from: x.who, to: you } : null;
  return (
    <div className={styles.tl}>
      {items.map((x, i) => {
        const r = route(x);
        return (
          <div key={i} className={styles.tlItem} onClick={() => x.node && onPick(x.node)}>
            <div className={styles.rail}>
              <span className={mergeClasses(styles.dot, styles[`dot_${tone(x)}` as "dot_done"])}>{icon(x)}</span>
              {i < items.length - 1 && <span className={styles.line} />}
            </div>
            <div>
              <div className={styles.tlHead}>
                {r ? <Route from={r.from} to={r.to} /> : <span className={styles.tlWho}>{x.who || you}</span>}
                <Caption1 className={styles.tlTime}>{fmtTime(x.t)}</Caption1>
              </div>
              <Caption1 block className={mergeClasses(styles.muted)}>{label(x)}</Caption1>
              {x.text && <Clamp text={x.text} />}
            </div>
          </div>
        );
      })}
    </div>
  );
}
