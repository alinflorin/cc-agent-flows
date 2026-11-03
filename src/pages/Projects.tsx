import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { useTranslation } from "react-i18next";
import { Body1, Button, Caption1, Card, Link, MessageBar, MessageBarBody, CardHeader, Dialog, DialogActions, DialogBody, DialogContent, DialogSurface, DialogTitle, Field, Input, Subtitle2, Textarea, makeStyles, tokens } from "@fluentui/react-components";
import { Add20Regular, Folder24Regular } from "@fluentui/react-icons";
import { PageHeader } from "../components/PageHeader";
import { EmptyState } from "../components/Confirm";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";
import { call } from "../tauri";
import { fmtDateTime } from "../text";
import { DEMO_PROJECT, ProjectInfo, projectPath } from "../project";

interface Check {
  id: string;
  level: "ok" | "warn" | "error" | "info";
}

interface ActiveSession {
  project: string;
  projectName: string;
  id: string;
  label: string;
  status: string;
  created: number;
}

const useStyles = makeStyles({
  banner: { marginBottom: tokens.spacingVerticalM },
  activeList: { display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(300px, 1fr))", gap: tokens.spacingHorizontalM, marginBottom: tokens.spacingVerticalXL },
  sectionTitle: { display: "block", marginBottom: tokens.spacingVerticalS },
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
  const [problems, setProblems] = useState<string[]>([]);
  const [active, setActive] = useState<ActiveSession[]>([]);

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

  // the sessions that run or wait for the user, across projects
  const loadActive = useCallback(async (list: ProjectInfo[]) => {
    try {
      const found: ActiveSession[] = [];
      for (const p of list) {
        if (p.sessions === 0) continue;
        const sessions = await call<{ id: string; name: string; task: string; created: number; status: string }[]>("sessions_list", { project: p.id });
        for (const s of sessions) {
          if (s.status !== "running" && s.status !== "waiting") continue;
          const text = s.name || s.task;
          found.push({ project: p.id, projectName: p.name, id: s.id, label: text ? (text.length > 60 ? `${text.slice(0, 60)}…` : text) : t("sessions.untitled"), status: s.status, created: s.created });
        }
      }
      // the ones that wait for the user first
      found.sort((a, b) => Number(b.status === "waiting") - Number(a.status === "waiting") || b.created - a.created);
      setActive(found);
    } catch {
      setActive([]);
    }
  }, [t]);

  useEffect(() => {
    if (projects) void loadActive(projects);
  }, [projects, loadActive]);

  // status changes and sessions started by triggers update the list without a reload
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

  // what keeps this computer from running agents (the About page's readiness checks)
  useEffect(() => {
    if (!desktop) return;
    call<Check[]>("system_status").then(
      (checks) => setProblems(checks.filter((c) => c.level === "error" || c.level === "warn").map((c) => c.id)),
      () => {},
    );
  }, [desktop]);

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
      {desktop && problems.length > 0 && (
        <MessageBar intent="warning" className={styles.banner}>
          <MessageBarBody>
            {t("projects.notReady", { items: problems.map((id) => t(`about.checks.${id}`)).join(", ") })}{" "}
            <Link onClick={() => navigate("/about")}>{t("projects.notReadyLink")}</Link>
          </MessageBarBody>
        </MessageBar>
      )}
      {desktop && active.length > 0 && (
        <>
          <Subtitle2 className={styles.sectionTitle}>{t("projects.active")}</Subtitle2>
          <div className={styles.activeList}>
            {active.map((a) => (
              <Card key={`${a.project}/${a.id}`} onClick={() => navigate(projectPath(a.project, `/sessions/${a.id}`))}>
                <CardHeader
                  header={<Subtitle2>{a.label}</Subtitle2>}
                  description={
                    <Caption1>
                      {a.projectName} · {a.status === "waiting" ? t("projects.activeWaiting") : t("projects.activeRunning")} · {fmtDateTime(a.created)}
                    </Caption1>
                  }
                />
              </Card>
            ))}
          </div>
        </>
      )}
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
