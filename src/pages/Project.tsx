import { useCallback, useEffect, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { useTranslation } from "react-i18next";
import {
  Body1,
  Button,
  Card,
  CardHeader,
  Caption1,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Field,
  Input,
  Tab,
  TabList,
  Textarea,
  Title2,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { Add16Regular, Delete16Regular, Edit16Regular } from "@fluentui/react-icons";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";
import { WorkflowEditor } from "./WorkflowEditor";

interface ProjectInfo {
  id: string;
  name: string;
  description: string;
  system_prompt: string;
}

interface Session {
  id: string;
  name: string;
  task: string;
  created: number;
  status: string;
}

const useStyles = makeStyles({
  head: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalXS, marginBottom: tokens.spacingVerticalM },
  tabs: { marginBottom: tokens.spacingVerticalL },
  bar: { display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: tokens.spacingVerticalM },
  list: { display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))", gap: tokens.spacingHorizontalM },
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM },
});

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

function Sessions({ project }: { project: string }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const navigate = useNavigate();
  const [sessions, setSessions] = useState<Session[]>([]);
  const [draft, setDraft] = useState<{ name: string; task: string } | null>(null);

  const load = useCallback(async () => {
    try {
      setSessions(await call<Session[]>("sessions_list", { project }));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [project, notify]);

  useEffect(() => {
    void load();
  }, [load]);

  const create = async () => {
    if (!draft) return;
    try {
      await call("sessions_create", { project, ...draft });
      setDraft(null);
      await load();
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
      <div className={styles.bar}>
        <Caption1>{t("sessions.hint")}</Caption1>
        <Button appearance="primary" icon={<Add16Regular />} onClick={() => setDraft({ name: "", task: "" })}>
          {t("sessions.new")}
        </Button>
      </div>
      {sessions.length === 0 && <Body1 block>{t("sessions.empty")}</Body1>}
      <div className={styles.list}>
        {sessions.map((s) => (
          <Card key={s.id} onClick={() => navigate(`/projects/${project}/sessions/${s.id}`)}>
            <CardHeader
              header={<Body1><b>{s.name}</b></Body1>}
              description={<Caption1>{new Date(s.created * 1000).toLocaleString()} · {t(`sessions.status.${s.status}`, s.status)}</Caption1>}
              action={
                <Button appearance="subtle" icon={<Delete16Regular />} aria-label={t("sessions.delete")}
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

      <Dialog open={draft !== null} onOpenChange={(_, d) => !d.open && setDraft(null)}>
        <DialogSurface>
          <DialogBody>
            <DialogTitle>{t("sessions.new")}</DialogTitle>
            <DialogContent className={styles.form}>
              {draft && (
                <>
                  <Field label={t("sessions.name")} required>
                    <Input value={draft.name} maxLength={120} onChange={(_, d) => setDraft({ ...draft, name: d.value })} />
                  </Field>
                  <Field label={t("sessions.task")} hint={t("sessions.taskHint")}>
                    <Textarea rows={6} value={draft.task} onChange={(_, d) => setDraft({ ...draft, task: d.value })} />
                  </Field>
                </>
              )}
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={() => setDraft(null)}>
                {t("common.cancel")}
              </Button>
              <Button appearance="primary" disabled={!draft?.name.trim()} onClick={create}>
                {t("common.save")}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </>
  );
}

export function Project() {
  const { id = "" } = useParams();
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const [info, setInfo] = useState<ProjectInfo | null>(null);
  const [tab, setTab] = useState<"sessions" | "workflow">("sessions");
  const [editing, setEditing] = useState<ProjectInfo | null>(null);

  const load = useCallback(async () => {
    try {
      const all = await call<ProjectInfo[]>("projects_list");
      setInfo(all.find((p) => p.id === id) ?? null);
    } catch (e) {
      notify(String(e), "error");
    }
  }, [id, notify]);

  useEffect(() => {
    if (inTauri()) void load();
  }, [load]);

  const saveInfo = async () => {
    if (!editing) return;
    try {
      const { name, description, system_prompt } = editing;
      await call("projects_save", { id, project: { name, description, system_prompt } });
      setEditing(null);
      await load();
    } catch (e) {
      notify(String(e), "error");
    }
  };

  if (!inTauri()) return <Body1 block>{t("projects.desktopOnly")}</Body1>;
  if (!info) return <Body1 block>{t("projects.missing")} <Link to="/projects">{t("projects.title")}</Link></Body1>;

  return (
    <>
      <div className={styles.head}>
        <Caption1><Link to="/projects">{t("projects.title")}</Link></Caption1>
        <div className={styles.bar}>
          <Title2 as="h1">{info.name}</Title2>
          <Button icon={<Edit16Regular />} onClick={() => setEditing(info)}>
            {t("projects.edit")}
          </Button>
        </div>
        {info.description && <Body1>{info.description}</Body1>}
      </div>
      <TabList className={styles.tabs} selectedValue={tab} onTabSelect={(_, d) => setTab(d.value as "sessions" | "workflow")}>
        <Tab value="sessions">{t("projects.tabs.sessions")}</Tab>
        <Tab value="workflow">{t("projects.tabs.workflow")}</Tab>
      </TabList>
      {tab === "sessions" ? <Sessions project={id} /> : <WorkflowEditor project={id} />}

      <Dialog open={editing !== null} onOpenChange={(_, d) => !d.open && setEditing(null)}>
        <DialogSurface>
          <DialogBody>
            <DialogTitle>{t("projects.edit")}</DialogTitle>
            <DialogContent className={styles.form}>
              {editing && (
                <>
                  <Field label={t("projects.name")} required>
                    <Input value={editing.name} maxLength={80} onChange={(_, d) => setEditing({ ...editing, name: d.value })} />
                  </Field>
                  <Field label={t("projects.description")}>
                    <Textarea rows={3} value={editing.description} onChange={(_, d) => setEditing({ ...editing, description: d.value })} />
                  </Field>
                  <Field label={t("projects.systemPrompt")} hint={t("projects.systemPromptHint")}>
                    <Textarea rows={8} value={editing.system_prompt} onChange={(_, d) => setEditing({ ...editing, system_prompt: d.value })} />
                  </Field>
                </>
              )}
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={() => setEditing(null)}>
                {t("common.cancel")}
              </Button>
              <Button appearance="primary" disabled={!editing?.name.trim()} onClick={saveInfo}>
                {t("common.save")}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </>
  );
}
