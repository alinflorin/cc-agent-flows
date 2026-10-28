import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { PageHeader } from "../components/PageHeader";
import { useTranslation } from "react-i18next";
import { Body1, Button, Card, CardHeader, Caption1, makeStyles, tokens } from "@fluentui/react-components";
import { Add20Regular, Delete20Regular } from "@fluentui/react-icons";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";

interface Session {
  id: string;
  name: string;
  task: string;
  created: number;
  status: string;
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

  const load = useCallback(async () => {
    try {
      setSessions(await call<Session[]>("sessions_list"));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify]);

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  const create = async () => {
    try {
      navigate(`/sessions/${await call<string>("sessions_create")}`);
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
        actions={
          <Button appearance="primary" icon={<Add20Regular />} disabled={!desktop} onClick={create}>
            {t("sessions.new")}
          </Button>
        }
      />
      {!desktop && <Body1 block>{t("app.desktopOnly")}</Body1>}
      {desktop && sessions.length === 0 && <Body1 block>{t("sessions.empty")}</Body1>}
      <div className={styles.list}>
        {sessions.map((s) => (
          <Card key={s.id} onClick={() => navigate(`/sessions/${s.id}`)}>
            <CardHeader
              header={<Body1><b>{s.name || t("sessions.untitled")}</b></Body1>}
              description={<Caption1>{new Date(s.created * 1000).toLocaleString()} · {t(`sessions.status.${s.status}`, s.status)}</Caption1>}
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
            {s.task && <Caption1>{s.task.length > 160 ? `${s.task.slice(0, 160)}…` : s.task}</Caption1>}
          </Card>
        ))}
      </div>
    </>
  );
}
