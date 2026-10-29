import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { useTranslation } from "react-i18next";
import { Body1, Button, Caption1, Card, CardHeader, Dialog, DialogActions, DialogBody, DialogContent, DialogSurface, DialogTitle, Field, Input, Subtitle2, Textarea, makeStyles, tokens } from "@fluentui/react-components";
import { Add20Regular, Folder24Regular } from "@fluentui/react-icons";
import { PageHeader } from "../components/PageHeader";
import { EmptyState } from "../components/Confirm";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";
import { call } from "../tauri";
import { DEMO_PROJECT, ProjectInfo, projectPath } from "../project";

const useStyles = makeStyles({
  grid: { display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(300px, 1fr))", gap: tokens.spacingHorizontalM },
  icon: { color: tokens.colorBrandForeground1 },
  desc: { color: tokens.colorNeutralForeground3 },
  counts: { display: "flex", gap: tokens.spacingHorizontalL, color: tokens.colorNeutralForeground3, padding: `0 ${tokens.spacingHorizontalM} ${tokens.spacingVerticalM}` },
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM },
  empty: { display: "flex", flexDirection: "column", alignItems: "flex-start", gap: tokens.spacingVerticalM },
});

/** The landing page: the projects as cards. A project owns its workflows, sessions, agent profiles, images, variables, secrets and injections. */
export function Projects() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const navigate = useNavigate();
  const desktop = inTauri();
  const [projects, setProjects] = useState<ProjectInfo[] | null>(null);
  const [creating, setCreating] = useState<{ name: string; description: string } | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      setProjects(await call<ProjectInfo[]>("projects_list"));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify]);

  useEffect(() => {
    if (!desktop) return;
    void load();
    // the welcome dialog creates the demo project while this page is open
    window.addEventListener("projects-changed", load);
    return () => window.removeEventListener("projects-changed", load);
  }, [desktop, load]);

  const create = async () => {
    if (!creating) return;
    setBusy(true);
    try {
      const id = await call<string>("projects_save", { id: null, name: creating.name, description: creating.description, systemPrompt: "" });
      setCreating(null);
      navigate(projectPath(id, "/workflow"));
    } catch (e) {
      notify(String(e), "error");
    }
    setBusy(false);
  };

  const demo = async () => {
    setBusy(true);
    try {
      await call("provision_starters");
      notify(t("welcome.done"), "success");
      await load();
    } catch (e) {
      notify(String(e), "error");
    }
    setBusy(false);
  };

  return (
    <>
      <PageHeader
        title={t("projects.title")}
        description={desktop ? t("projects.intro") : undefined}
        actions={
          <Button appearance="primary" icon={<Add20Regular />} disabled={!desktop} onClick={() => setCreating({ name: "", description: "" })}>
            {t("projects.new")}
          </Button>
        }
      />
      {!desktop && <Body1 block>{t("app.desktopOnly")}</Body1>}
      {desktop && projects && projects.length === 0 && (
        <EmptyState>{t("projects.empty")}</EmptyState>
      )}
      <div className={styles.grid}>
        {(projects ?? []).map((p) => (
          <Card key={p.id} onClick={() => navigate(projectPath(p.id, "/sessions"))}>
            <CardHeader
              image={<span className={styles.icon}><Folder24Regular /></span>}
              header={<Subtitle2>{p.name}</Subtitle2>}
              description={<span className={styles.desc}>{p.description || t("projects.noDescription")}</span>}
            />
            <div className={styles.counts}>
              <Caption1>{t("projects.workflows", { count: p.workflows })}</Caption1>
              <Caption1>{t("projects.profiles", { count: p.profiles })}</Caption1>
              <Caption1>{t("projects.sessions", { count: p.sessions })}</Caption1>
            </div>
          </Card>
        ))}
      </div>
      {desktop && projects && !projects.some((p) => p.id === DEMO_PROJECT) && (
        <div className={styles.empty} style={{ marginTop: tokens.spacingVerticalXL }}>
          <Caption1 className={styles.desc}>{t("projects.demoHint")}</Caption1>
          <Button disabled={busy} onClick={() => void demo()}>
            {t("projects.demo")}
          </Button>
        </div>
      )}

      <Dialog surfaceMotion={null} open={creating !== null} onOpenChange={(_, d) => !d.open && setCreating(null)}>
        <DialogSurface backdropMotion={null}>
          <DialogBody>
            <DialogTitle>{t("projects.new")}</DialogTitle>
            <DialogContent className={styles.form}>
              {creating && (
                <>
                  <Field label={t("profiles.name")} required>
                    <Input autoFocus value={creating.name} maxLength={80} onKeyDown={(e) => e.key === "Enter" && creating.name.trim() && !busy && void create()} onChange={(_, d) => setCreating({ ...creating, name: d.value })} />
                  </Field>
                  <Field label={t("profiles.description")}>
                    <Textarea rows={3} value={creating.description} onChange={(_, d) => setCreating({ ...creating, description: d.value })} />
                  </Field>
                </>
              )}
            </DialogContent>
            <DialogActions>
              <Button onClick={() => setCreating(null)}>{t("common.cancel")}</Button>
              <Button appearance="primary" disabled={busy || !creating?.name.trim()} onClick={() => void create()}>
                {t("common.save")}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </>
  );
}
