import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Badge, Caption1, Spinner, mergeClasses } from "@fluentui/react-components";
import { clip, type CallInfo, type Digest, type Run, type Traffic } from "../../sessionLog";
import { useStyles } from "./styles";
import { fmtTokens, statusColor } from "./types";
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
              {t("session.run")} {n + 1} · {new Date(r.t * 1000).toLocaleTimeString()}
            </Caption1>
            {r.traffic.map((x, i) => (
              <div key={i} className={mergeClasses(styles.step, styles.mono)}>
                <Caption1 className={styles.muted}>{new Date(x.t * 1000).toLocaleTimeString()} </Caption1>
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

export function CallCard({ c, d, names }: { c: CallInfo; d: Digest; names: Record<string, string> }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const open = c.status === "open";
  return (
    <div className={styles.card}>
      <div className={styles.cardHead}>
        <Badge appearance="filled" color={statusColor(open ? "running" : c.status)}>{open ? t("session.node.busy") : t(`runs.status.${c.status}`, c.status)}</Badge>
        <Caption1 className={styles.muted}>{c.parent ? t("session.askedBy", { name: names[c.parent] ?? c.parent }) : t("session.thisIsTheRequest")}</Caption1>
        <Caption1 className={mergeClasses(styles.muted, styles.grow)} style={{ textAlign: "right" }}>{new Date(c.t * 1000).toLocaleTimeString()}</Caption1>
      </div>
      {c.asked && <Clamp className={styles.quote} text={c.asked} />}

      {c.runs.map((k, n) => {
        const r = d.runs.get(k);
        if (!r) return null;
        return (
          <div key={k}>
            <div className={styles.cardHead}>
              {r.status === "running" ? <Spinner size="extra-tiny" /> : <Badge size="small" appearance="tint" color={statusColor(r.status)}>{t(`runs.status.${r.status}`, r.status)}</Badge>}
              <Caption1>{t("session.run")} {n + 1}</Caption1>
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
                <span className={styles.chip} title={k.label}>→ {k.label}</span>
                <Clamp text={k.instructions} />
              </div>
            ))}
          {x.action === "finish" && (
            <div className={x.detail.success === false ? styles.outFail : styles.outDone}>
              <Clamp text={x.detail.result ?? ""} />
            </div>
          )}
          {x.action === "ask" && (
            <div className={styles.outAsk}>
              <Clamp text={(x.detail.questions ?? []).map((q: string) => `? ${q}`).join("\n")} />
            </div>
          )}
          {x.notes && <Clamp className={styles.muted} text={`${t("session.notes")}: ${x.notes}`} />}
        </div>
      ))}
      {c.refused.map((r, i) => <Clamp key={i} className={styles.muted} text={t("session.refused", { reason: r })} />)}
      {!open && c.decisions.length === 0 && c.summary && (
        <div className={c.status === "failed" ? styles.outFail : styles.outDone}><Clamp text={c.summary} /></div>
      )}
    </div>
  );
}

export function Timeline({ items, onPick }: { items: Digest["timeline"]; onPick: (node: string) => void }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const dotOf = (x: Digest["timeline"][number]) =>
    x.status === "done" ? styles.dot_done : x.status === "failed" || x.kind === "error" ? styles.dot_failed : x.status === "needs_input" || x.kind === "wait" ? styles.dot_wait : styles.dot_brand;
  const verb = (x: Digest["timeline"][number]) =>
    x.kind === "start" ? t("session.ev.start", { from: x.from || t("session.ev.you") }) : t(`session.ev.${x.kind}${x.kind === "end" ? `_${x.status}` : ""}`, "");
  return (
    <div className={styles.tl}>
      {items.map((x, i) => (
        <div key={i} className={styles.tlItem} onClick={() => x.node && onPick(x.node)}>
          <div className={styles.rail}>
            <span className={mergeClasses(styles.dot, dotOf(x))} />
            {i < items.length - 1 && <span className={styles.line} />}
          </div>
          <div>
            <div className={styles.tlHead}>
              {x.who && <span className={styles.tlWho}>{x.who}</span>}
              <Caption1 className={styles.muted}>{verb(x)}</Caption1>
              <Caption1 className={styles.tlTime}>{new Date(x.t * 1000).toLocaleTimeString()}</Caption1>
            </div>
            {x.text && <Clamp text={x.text} />}
          </div>
        </div>
      ))}
    </div>
  );
}
