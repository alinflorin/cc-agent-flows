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
  DialogSurface,
  DialogTitle,
  Field,
  Input,
  Textarea,
  Title2,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { Add16Regular, Delete16Regular } from "@fluentui/react-icons";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";

interface Project {
  id: string;
  name: string;
  description: string;
}

const useStyles = makeStyles({
  head: { display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: tokens.spacingVerticalL },
  list: { display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))", gap: tokens.spacingHorizontalM },
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM },
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

  const [projects, setProjects] = useState<Project[]>([]);
  const [draft, setDraft] = useState<{ name: string; description: string; system_prompt: string } | null>(null);
  const [removing, setRemoving] = useState<Project | null>(null);

  const load = useCallback(async () => {
    try {
      setProjects(await call<Project[]>("projects_list"));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify]);

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  const create = async () => {
    if (!draft) return;
    try {
      const id = await call<string>("projects_save", { id: null, project: draft });
      setDraft(null);
      navigate(`/projects/${id}`);
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const remove = async () => {
    if (!removing) return;
    try {
      await call("projects_delete", { id: removing.id });
      setRemoving(null);
      await load();
    } catch (e) {
      notify(String(e), "error");
    }
  };

  return (
    <>
      <div className={styles.head}>
        <Title2 as="h1">{t("projects.title")}</Title2>
        <Button appearance="primary" icon={<Add16Regular />} disabled={!desktop} onClick={() => setDraft({ name: "", description: "", system_prompt: "" })}>
          {t("projects.new")}
        </Button>
      </div>
      {!desktop && <Body1 block>{t("projects.desktopOnly")}</Body1>}
      {desktop && projects.length === 0 && <Body1 block>{t("projects.empty")}</Body1>}
      <div className={styles.list}>
        {projects.map((p) => (
          <Card key={p.id} onClick={() => navigate(`/projects/${p.id}`)}>
            <CardHeader
              header={<Body1><b>{p.name}</b></Body1>}
              description={<Caption1>{p.description || t("projects.noDescription")}</Caption1>}
              action={
                <Button
                  appearance="subtle"
                  icon={<Delete16Regular />}
                  aria-label={t("projects.delete")}
                  onClick={(ev) => {
                    ev.stopPropagation();
                    setRemoving(p);
                  }}
                />
              }
            />
          </Card>
        ))}
      </div>

      <Dialog open={draft !== null} onOpenChange={(_, d) => !d.open && setDraft(null)}>
        <DialogSurface>
          <DialogBody>
            <DialogTitle>{t("projects.new")}</DialogTitle>
            <DialogContent className={styles.form}>
              {draft && (
                <>
                  <Field label={t("projects.name")} required>
                    <Input value={draft.name} maxLength={80} onChange={(_, d) => setDraft({ ...draft, name: d.value })} />
                  </Field>
                  <Field label={t("projects.description")}>
                    <Textarea rows={3} value={draft.description} onChange={(_, d) => setDraft({ ...draft, description: d.value })} />
                  </Field>
                  <Field label={t("projects.systemPrompt")} hint={t("projects.systemPromptHint")}>
                    <Textarea rows={5} value={draft.system_prompt} onChange={(_, d) => setDraft({ ...draft, system_prompt: d.value })} />
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

      <Dialog open={removing !== null} onOpenChange={(_, d) => !d.open && setRemoving(null)}>
        <DialogSurface>
          <DialogBody>
            <DialogTitle>{t("projects.delete")}</DialogTitle>
            <DialogContent>
              <Body1>{t("projects.deleteConfirm", { name: removing?.name })}</Body1>
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={() => setRemoving(null)}>
                {t("common.cancel")}
              </Button>
              <Button appearance="primary" onClick={remove}>
                {t("projects.delete")}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </>
  );
}
