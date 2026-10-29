import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { PageHeader } from "../components/PageHeader";
import { fmtDateTime } from "../text";
import { useTranslation } from "react-i18next";
import { Body1, Button, Card, CardHeader, Caption1, Select, Subtitle2, makeStyles, tokens } from "@fluentui/react-components";
import { Add20Regular, Delete20Regular } from "@fluentui/react-icons";
import { EmptyState, useConfirmDelete } from "../components/Confirm";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";
import { call } from "../tauri";
import { projectPath, useProjectId } from "../project";

interface Session {
  id: string;
  name: string;
  task: string;
  created: number;
  status: string;
  workflow: string;
}

const useStyles = makeStyles({
  list: { display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))", gap: tokens.spacingHorizontalM },
});

export function Sessions() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const navigate = useNavigate();
  const project = useProjectId();
  const desktop = inTauri();
  const confirm = useConfirmDelete();
  const [sessions, setSessions] = useState<Session[]>([]);
  const [workflows, setWorkflows] = useState<Record<string, string>>({});
  const [filter, setFilter] = useState("");
  const shown = filter ? sessions.filter((s) => s.workflow === filter) : sessions;

  const label = (s: Session) => s.name || (s.task ? (s.task.length > 60 ? `${s.task.slice(0, 60)}…` : s.task) : t("sessions.untitled"));

  const load = useCallback(async () => {
    try {
      setSessions(await call<Session[]>("sessions_list", { project }));
      setWorkflows(Object.fromEntries((await call<{ id: string; name: string }[]>("workflows_list", { project })).map((w) => [w.id, w.name])));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify, project]);

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
      navigate(projectPath(project, `/sessions/${await call<string>("sessions_create", { project })}`));
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const remove = async (id: string) => {
    try {
      await call("sessions_delete", { project, id });
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
        <Select
          aria-label={t("sessions.filterWorkflow")}
          value={filter}
          onChange={(_, d) => setFilter(d.value)}
          style={{ marginBottom: tokens.spacingVerticalM, maxWidth: 320 }}
        >
          <option value="">{t("sessions.allWorkflows")}</option>
          {Object.entries(workflows).map(([id, name]) => (
            <option key={id} value={id}>{name}</option>
          ))}
        </Select>
      )}
      {desktop && sessions.length === 0 && <EmptyState>{t("sessions.empty")}</EmptyState>}
      <div className={styles.list}>
        {shown.map((s) => (
          <Card key={s.id} onClick={() => navigate(projectPath(project, `/sessions/${s.id}`))}>
            <CardHeader
              header={<Subtitle2>{label(s)}</Subtitle2>}
              description={<Caption1>{fmtDateTime(s.created)} · {t(`sessions.status.${s.status}`, s.status)} · {workflows[s.workflow] ?? s.workflow}</Caption1>}
              action={
                <Button
                  appearance="subtle"
                  icon={<Delete20Regular />}
                  aria-label={t("sessions.delete")}
                  disabled={s.status === "running" || s.status === "waiting"}
                  onClick={(ev) => {
                    ev.stopPropagation();
                    confirm.ask(label(s), () => remove(s.id));
                  }}
                />
              }
            />
          </Card>
        ))}
      </div>
      {confirm.dialog}
    </>
  );
}
