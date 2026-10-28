import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router";
import { useTranslation } from "react-i18next";
import { Body1, Button, Caption1, Card, Select, Subtitle2, Table, TableBody, TableCell, TableCellLayout, TableHeader, TableHeaderCell, TableRow, Title2, makeStyles, tokens } from "@fluentui/react-components";
import { ArrowClockwise20Regular } from "@fluentui/react-icons";
import { PageHeader } from "../components/PageHeader";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";

interface SessionStat {
  id: string;
  name: string;
  task: string;
  status: string;
  created: number;
  duration_secs: number;
  cost_usd: number;
  runs: number;
  failed_runs: number;
  turns: number;
}
interface AgentStat {
  profile: string;
  runs: number;
  failed: number;
  cost_usd: number;
  total_secs: number;
  avg_secs: number;
  longest_secs: number;
}
interface DayStat {
  day: string;
  sessions: number;
  runs: number;
  cost_usd: number;
}
interface Data {
  sessions: SessionStat[];
  agents: AgentStat[];
  days: DayStat[];
  total_cost_usd: number;
  total_runs: number;
  failed_runs: number;
}

const useStyles = makeStyles({
  tiles: { display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(180px, 1fr))", gap: tokens.spacingHorizontalM, marginBottom: tokens.spacingVerticalXL },
  tile: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalXXS },
  section: { marginBottom: tokens.spacingVerticalXXL, display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM },
  bars: { display: "flex", alignItems: "flex-end", gap: "3px", height: "120px", padding: `${tokens.spacingVerticalS} 0`, borderBottom: `1px solid ${tokens.colorNeutralStroke2}` },
  bar: { flex: 1, minWidth: "4px", backgroundColor: tokens.colorBrandBackground, borderRadius: `${tokens.borderRadiusSmall} ${tokens.borderRadiusSmall} 0 0` },
  muted: { color: tokens.colorNeutralForeground3 },
  row: { cursor: "pointer" },
  bar2: { height: "6px", borderRadius: tokens.borderRadiusCircular, backgroundColor: tokens.colorBrandBackground, minWidth: "2px" },
});

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

const usd = (n: number) => `$${n.toFixed(2)}`;
const dur = (s: number) => (s >= 3600 ? `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m` : s >= 60 ? `${Math.floor(s / 60)}m ${s % 60}s` : `${s}s`);

/** Cost, time and failures over all the sessions, from their logs. */
export function Analytics() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const navigate = useNavigate();
  const desktop = inTauri();
  const [data, setData] = useState<Data | null>(null);
  const [range, setRange] = useState("30");

  const load = useCallback(async () => {
    try {
      setData(await call<Data>("analytics_get"));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify]);
  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  // the numbers of the chosen period (days back from now; 0 = all time)
  const view = useMemo(() => {
    if (!data) return null;
    const since = range === "0" ? 0 : Date.now() / 1000 - Number(range) * 86400;
    const sessions = data.sessions.filter((s) => s.created >= since);
    const days = data.days.filter((d) => range === "0" || new Date(d.day).getTime() / 1000 >= since - 86400);
    return {
      sessions,
      days,
      cost: sessions.reduce((a, s) => a + s.cost_usd, 0),
      runs: sessions.reduce((a, s) => a + s.runs, 0),
      failed: sessions.reduce((a, s) => a + s.failed_runs, 0),
      done: sessions.filter((s) => s.status === "done").length,
    };
  }, [data, range]);

  if (!desktop) return (<><PageHeader title={t("nav.analytics")} /><Body1 block>{t("app.desktopOnly")}</Body1></>);
  const maxDay = Math.max(0.0001, ...(view?.days.map((d) => d.cost_usd) ?? [0]));
  const maxAgent = Math.max(0.0001, ...(data?.agents.map((a) => a.cost_usd) ?? [0]));
  const slowest = [...(view?.sessions ?? [])].sort((a, b) => b.cost_usd - a.cost_usd).slice(0, 10);

  return (
    <>
      <PageHeader
        title={t("nav.analytics")}
        description={t("analytics.intro")}
        actions={
          <>
            <Select aria-label={t("analytics.range")} value={range} onChange={(_, d) => setRange(d.value)}>
              <option value="7">{t("analytics.last", { days: 7 })}</option>
              <option value="30">{t("analytics.last", { days: 30 })}</option>
              <option value="90">{t("analytics.last", { days: 90 })}</option>
              <option value="0">{t("analytics.all")}</option>
            </Select>
            <Button icon={<ArrowClockwise20Regular />} aria-label={t("analytics.refresh")} onClick={() => void load()} />
          </>
        }
      />
      {view && (
        <>
          <div className={styles.tiles}>
            {[
              [t("analytics.sessions"), String(view.sessions.length)],
              [t("analytics.finished"), String(view.done)],
              [t("analytics.cost"), usd(view.cost)],
              [t("analytics.avgCost"), view.sessions.length ? usd(view.cost / view.sessions.length) : "–"],
              [t("analytics.runs"), String(view.runs)],
              [t("analytics.failRate"), view.runs ? `${Math.round((view.failed / view.runs) * 100)}%` : "–"],
            ].map(([label, value]) => (
              <Card key={label} className={styles.tile}>
                <Caption1 className={styles.muted}>{label}</Caption1>
                <Title2>{value}</Title2>
              </Card>
            ))}
          </div>

          <section className={styles.section}>
            <Subtitle2>{t("analytics.perDay")}</Subtitle2>
            {view.days.length === 0 ? (
              <Caption1 className={styles.muted}>{t("analytics.none")}</Caption1>
            ) : (
              <div className={styles.bars}>
                {view.days.map((d) => (
                  <div key={d.day} className={styles.bar} style={{ height: `${Math.max(2, (d.cost_usd / maxDay) * 100)}%` }} title={`${d.day}: ${usd(d.cost_usd)}, ${t("analytics.runsN", { count: d.runs })}, ${t("analytics.sessionsN", { count: d.sessions })}`} />
                ))}
              </div>
            )}
          </section>

          <section className={styles.section}>
            <Subtitle2>{t("analytics.perAgent")}</Subtitle2>
            <Table size="small" aria-label={t("analytics.perAgent")}>
              <TableHeader>
                <TableRow>
                  {["agent", "runs", "failed", "avg", "longest", "cost"].map((c) => <TableHeaderCell key={c}>{t(`analytics.col.${c}`)}</TableHeaderCell>)}
                </TableRow>
              </TableHeader>
              <TableBody>
                {(data?.agents ?? []).map((a) => (
                  <TableRow key={a.profile}>
                    <TableCell><TableCellLayout>{a.profile || "–"}</TableCellLayout></TableCell>
                    <TableCell>{a.runs}</TableCell>
                    <TableCell>{a.failed}{a.runs ? ` (${Math.round((a.failed / a.runs) * 100)}%)` : ""}</TableCell>
                    <TableCell>{dur(a.avg_secs)}</TableCell>
                    <TableCell>{dur(a.longest_secs)}</TableCell>
                    <TableCell>
                      {usd(a.cost_usd)}
                      <div className={styles.bar2} style={{ width: `${(a.cost_usd / maxAgent) * 100}%` }} />
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
            <Caption1 className={styles.muted}>{t("analytics.allTime")}</Caption1>
          </section>

          <section className={styles.section}>
            <Subtitle2>{t("analytics.costliest")}</Subtitle2>
            <Table size="small" aria-label={t("analytics.costliest")}>
              <TableHeader>
                <TableRow>
                  {["session", "status", "turns", "runs", "duration", "cost"].map((c) => <TableHeaderCell key={c}>{t(`analytics.col.${c}`)}</TableHeaderCell>)}
                </TableRow>
              </TableHeader>
              <TableBody>
                {slowest.map((s) => (
                  <TableRow key={s.id} className={styles.row} onClick={() => navigate(`/sessions/${s.id}`)}>
                    <TableCell><TableCellLayout>{s.name || s.task.slice(0, 60) || t("sessions.untitled")}</TableCellLayout></TableCell>
                    <TableCell>{t(`sessions.status.${s.status}`, s.status)}</TableCell>
                    <TableCell>{s.turns}</TableCell>
                    <TableCell>{s.runs}{s.failed_runs ? ` (${s.failed_runs} ✗)` : ""}</TableCell>
                    <TableCell>{dur(s.duration_secs)}</TableCell>
                    <TableCell>{usd(s.cost_usd)}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </section>
        </>
      )}
    </>
  );
}
