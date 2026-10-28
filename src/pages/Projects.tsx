import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";
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
  DialogTitle,
  Field,
  Input,
  Subtitle2,
  Textarea,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { Add20Regular, Delete20Regular } from "@fluentui/react-icons";
import { PageHeader } from "../components/PageHeader";
import { FormDialogSurface } from "../components/FormDialogSurface";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";

interface Project {
  name: string;
  description: string;
  system_prompt: string;
}

interface Entry extends Project {
  id: string;
  sessions: number;
  workflows: number;
}

const EMPTY: Project = { name: "", description: "", system_prompt: "" };

const useStyles = makeStyles({
  list: { display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))", gap: tokens.spacingHorizontalL },
  card: { cursor: "pointer", transitionProperty: "box-shadow", transitionDuration: "120ms", ":hover": { boxShadow: tokens.shadow8 } },
  desc: { color: tokens.colorNeutralForeground3 },
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM },
  footer: { display: "flex", justifyContent: "space-between", alignItems: "center" },
});

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export function Projects() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const navigate = useNavigate();
  const desktop = inTauri();
  const [entries, setEntries] = useState<Entry[]>([]);
  const [editing, setEditing] = useState<{ id: string | null; project: Project } | null>(null);
  // closing only hides the dialog, so the title cannot flip while it fades out
  const [open, setOpen] = useState(false);
  const edit = (v: { id: string | null; project: Project }) => {
    setEditing(v);
    setOpen(true);
  };

  const load = useCallback(async () => {
    try {
      setEntries(await call<Entry[]>("projects_list"));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify]);

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  const save = async () => {
    if (!editing) return;
    try {
      await call("projects_save", { id: editing.id, project: editing.project });
      setOpen(false);
      await load();
      window.dispatchEvent(new Event("projects-changed"));
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const remove = async (id: string) => {
    try {
      await call("projects_delete", { id });
      await load();
      window.dispatchEvent(new Event("projects-changed"));
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const set = (patch: Partial<Project>) => editing && setEditing({ ...editing, project: { ...editing.project, ...patch } });

  return (
    <>
      <PageHeader
        title={t("projects.title")}
        actions={
          <Button appearance="primary" icon={<Add20Regular />} disabled={!desktop} onClick={() => edit({ id: null, project: EMPTY })}>
            {t("projects.new")}
          </Button>
        }
      />
      {!desktop && <Body1 block>{t("app.desktopOnly")}</Body1>}
      {desktop && entries.length === 0 && <Body1 block style={{ color: tokens.colorNeutralForeground3 }}>{t("projects.empty")}</Body1>}
      <div className={styles.list}>
        {entries.map((e) => (
          <Card key={e.id} className={styles.card} onClick={() => navigate(`/projects/${e.id}/sessions`)}>
            <CardHeader
              header={<Subtitle2>{e.name}</Subtitle2>}
              description={<Caption1 className={styles.desc}>{e.description || t("projects.noDescription")}</Caption1>}
              action={
                <Button
                  appearance="subtle"
                  icon={<Delete20Regular />}
                  aria-label={t("projects.delete")}
                  onClick={(ev) => {
                    ev.stopPropagation();
                    void remove(e.id);
                  }}
                />
              }
            />
            <div className={styles.footer}>
              <Caption1>{t("projects.sessions", { count: e.sessions })} · {t("projects.workflows", { count: e.workflows })}</Caption1>
              <Button
                size="small"
                onClick={(ev) => {
                  ev.stopPropagation();
                  edit({ id: e.id, project: e });
                }}
              >
                {t("projects.edit")}
              </Button>
            </div>
          </Card>
        ))}
      </div>

      <Dialog surfaceMotion={null} open={open} onOpenChange={(_, d) => setOpen(d.open)}>
        <FormDialogSurface>
          <DialogBody>
            <DialogTitle>{editing?.id ? t("projects.edit") : t("projects.new")}</DialogTitle>
            <DialogContent className={styles.form}>
              {editing && (
                <>
                  <Field label={t("projects.name")} required>
                    <Input value={editing.project.name} maxLength={80} onChange={(_, d) => set({ name: d.value })} />
                  </Field>
                  <Field label={t("projects.description")}>
                    <Input value={editing.project.description} maxLength={500} onChange={(_, d) => set({ description: d.value })} />
                  </Field>
                  <Field label={t("projects.systemPrompt")} hint={t("projects.systemPromptHint")}>
                    <Textarea rows={8} value={editing.project.system_prompt} onChange={(_, d) => set({ system_prompt: d.value })} />
                  </Field>
                </>
              )}
            </DialogContent>
            <DialogActions>
              <Button onClick={() => setOpen(false)}>{t("projects.cancel")}</Button>
              <Button appearance="primary" disabled={!editing?.project.name.trim()} onClick={save}>
                {t("projects.save")}
              </Button>
            </DialogActions>
          </DialogBody>
        </FormDialogSurface>
      </Dialog>
    </>
  );
}
