import { useCallback, useEffect, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { PageHeader } from "../components/PageHeader";
import { useTranslation } from "react-i18next";
import { Body1, Button, Card, CardHeader, Caption1, Select, makeStyles, tokens } from "@fluentui/react-components";
import { Add20Regular, Delete20Regular } from "@fluentui/react-icons";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";

interface Session {
  id: string;
  name: string;
  task: string;
  created: number;
  status: string;
  workflow: string;
  project?: string | null;
}

const useStyles = makeStyles({
  list: { display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))", gap: tokens.spacingHorizontalM },
});

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export function Sessions() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const navigate = useNavigate();
  const desktop = inTauri();
  const [sessions, setSessions] = useState<Session[]>([]);
  const [workflows, setWorkflows] = useState<Record<string, string>>({});
  const [filter, setFilter] = useState("");
  const [projects, setProjects] = useState<Record<string, string>>({});
  // the project filter lives in the URL (`#/sessions?project=<id>`), which is where the Projects page links to
  const [params, setParams] = useSearchParams();
  const project = params.get("project") ?? "";
  const shown = sessions.filter((s) => (!filter || s.workflow === filter) && (!project || (project === "none" ? !s.project : s.project === project)));

  const load = useCallback(async () => {
    try {
      setSessions(await call<Session[]>("sessions_list"));
      setProjects(Object.fromEntries((await call<{ id: string; name: string }[]>("projects_list")).map((p) => [p.id, p.name])));
      setWorkflows(Object.fromEntries((await call<{ id: string; name: string }[]>("workflows_list")).map((w) => [w.id, w.name])));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify]);

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  // sessions started by a cron or webhook node, and status changes, show up without a reload
  useEffect(() => {
    if (!desktop) return;
    let gone = false;
    const offs: (() => void)[] = [];
    void (async () => {
      const { listen } = await import("@tauri-apps/api/event");
      const subs = [
        await listen("sessions-changed", () => void load()),
        await listen<{ entry: { kind: string } }>("session-log", ({ payload }) => {
          if (payload.entry.kind === "status") void load();
        }),
      ];
      if (gone) subs.forEach((u) => u());
      else offs.push(...subs);
    })();
    return () => {
      gone = true;
      offs.forEach((u) => u());
    };
  }, [desktop, load]);

  const create = async () => {
    try {
      navigate(`/sessions/${await call<string>("sessions_create", { project: project && project !== "none" ? project : null })}`);
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const remove = async (id: string) => {
    try {
      await call("sessions_delete", { id });
      await load();
    } catch (e) {
      notify(String(e), "error");
    }
  };

  return (
    <>
      <PageHeader
        title={t("nav.sessions")}
        description={desktop ? t("sessions.intro") : undefined}
        actions={
          <Button appearance="primary" icon={<Add20Regular />} disabled={!desktop} onClick={create}>
            {t("sessions.new")}
          </Button>
        }
      />
      {!desktop && <Body1 block>{t("app.desktopOnly")}</Body1>}
      {desktop && sessions.length > 0 && (
        <div style={{ display: "flex", flexWrap: "wrap", gap: tokens.spacingHorizontalM, marginBottom: tokens.spacingVerticalM }}>
        <Select
          aria-label={t("sessions.filterProject")}
          value={project}
          onChange={(_, d) => setParams(d.value ? { project: d.value } : {})}
          style={{ minWidth: 200 }}
        >
          <option value="">{t("sessions.allProjects")}</option>
          <option value="none">{t("sessions.noProject")}</option>
          {Object.entries(projects).map(([id, name]) => (
            <option key={id} value={id}>{name}</option>
          ))}
        </Select>
        <Select
          aria-label={t("sessions.filterWorkflow")}
          value={filter}
          onChange={(_, d) => setFilter(d.value)}
          style={{ minWidth: 200 }}
        >
          <option value="">{t("sessions.allWorkflows")}</option>
          {Object.entries(workflows).map(([id, name]) => (
            <option key={id} value={id}>{name}</option>
          ))}
        </Select>
        </div>
      )}
      {desktop && sessions.length === 0 && <Body1 block style={{ color: tokens.colorNeutralForeground3 }}>{t("sessions.empty")}</Body1>}
      <div className={styles.list}>
        {shown.map((s) => (
          <Card key={s.id} onClick={() => navigate(`/sessions/${s.id}`)}>
            <CardHeader
              header={<Body1><b>{s.name || (s.task ? (s.task.length > 60 ? `${s.task.slice(0, 60)}…` : s.task) : t("sessions.untitled"))}</b></Body1>}
              description={<Caption1>{new Date(s.created * 1000).toLocaleString()} · {t(`sessions.status.${s.status}`, s.status)} · {workflows[s.workflow] ?? s.workflow}{s.project && projects[s.project] ? ` · ${projects[s.project]}` : ""}</Caption1>}
              action={
                <Button
                  appearance="subtle"
                  icon={<Delete20Regular />}
                  aria-label={t("sessions.delete")}
                  onClick={(ev) => {
                    ev.stopPropagation();
                    void remove(s.id);
                  }}
                />
              }
            />
            
          </Card>
        ))}
      </div>
    </>
  );
}
