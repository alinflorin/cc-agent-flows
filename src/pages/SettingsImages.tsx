import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Badge,
  Body1,
  Button,
  Caption1,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Field,
  Input,
  Select,
  Textarea,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { Add16Regular, Delete16Regular, Edit16Regular } from "@fluentui/react-icons";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";

interface EnvVar {
  name: string;
  value: string;
}

interface Image {
  id: string;
  builtin: boolean;
  name: string;
  description: string;
  script: string;
  env: EnvVar[];
  state: "missing" | "queued" | "building" | "ready" | "stale";
  can_build: boolean;
  parent: string | null;
  built_at: number | null;
  size: number | null;
  error: string | null;
  log: string[];
}

interface Spec {
  name: string;
  description: string;
  parent: string;
  env: EnvVar[];
  script: string;
}

const useStyles = makeStyles({
  head: { display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: tokens.spacingVerticalM },
  list: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM, maxWidth: "760px" },
  item: {
    display: "flex",
    flexDirection: "column",
    gap: tokens.spacingVerticalS,
    padding: tokens.spacingHorizontalL,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    borderRadius: tokens.borderRadiusMedium,
  },
  row: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "center" },
  grow: { flex: 1 },
  envRow: { display: "flex", gap: tokens.spacingHorizontalS },
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM },
  mono: { fontFamily: tokens.fontFamilyMonospace },
  log: {
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeBase200,
    backgroundColor: tokens.colorNeutralBackground3,
    borderRadius: tokens.borderRadiusMedium,
    padding: tokens.spacingHorizontalM,
    maxHeight: "240px",
    overflow: "auto",
    whiteSpace: "pre-wrap",
    margin: 0,
  },
});

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

const COLORS = { missing: "danger", queued: "informative", building: "informative", ready: "success", stale: "warning" } as const;

/** The images agents can run on: the built-in base and the user's own, each with its build state. */
export function ImagesSettings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();
  const [images, setImages] = useState<Image[] | null>(null);
  const [editing, setEditing] = useState<{ id: string | null; spec: Spec } | null>(null);

  const refresh = useCallback(() => call<Image[]>("images_list").then(setImages, () => {}), []);

  useEffect(() => {
    if (!desktop) return;
    void refresh();
    const id = setInterval(refresh, 1500);
    return () => clearInterval(id);
  }, [desktop, refresh]);

  const run = async (cmd: string, args: Record<string, unknown>) => {
    try {
      await call(cmd, args);
      await refresh();
      return true;
    } catch (e) {
      notify(String(e), "error");
      return false;
    }
  };

  if (!desktop) return <Body1>{t("profiles.desktopOnly")}</Body1>;
  if (!images) return null;
  const nameOf = (id: string) => (id === "base" ? t("settings.images.base") : (images.find((x) => x.id === id)?.name ?? id));
  const setEnv = (n: number, patch: Partial<EnvVar>) =>
    setEditing((e) => e && { ...e, spec: { ...e.spec, env: e.spec.env.map((v, j) => (j === n ? { ...v, ...patch } : v)) } });
  const busy = images.some((i) => i.state === "building");

  return (
    <>
      <div className={styles.head}>
        <Body1>{t("settings.images.intro")}</Body1>
        <Button icon={<Add16Regular />} onClick={() => setEditing({ id: null, spec: { name: "", description: "", parent: "base", env: [], script: "" } })}>
          {t("settings.images.new")}
        </Button>
      </div>
      <div className={styles.list}>
        {images.map((i) => (
          <div key={i.id} className={styles.item}>
            <div className={styles.row}>
              <Body1 className={styles.grow}>
                <b>{i.builtin ? t("settings.images.base") : i.name}</b>
              </Body1>
              <Badge appearance="filled" color={COLORS[i.state]}>
                {t(`settings.images.state.${i.state}`)}
              </Badge>
              <Button size="small" disabled={busy || !i.can_build} title={i.can_build ? undefined : t("settings.images.needBase")} onClick={() => run("images_build", { id: i.id })}>
                {i.state === "missing" ? t("settings.images.build") : t("settings.images.rebuild")}
              </Button>
              {!i.builtin && (
                <>
                  <Button
                    size="small"
                    appearance="subtle"
                    icon={<Edit16Regular />}
                    aria-label={t("profiles.edit")}
                    onClick={() => setEditing({ id: i.id, spec: { name: i.name, description: i.description, parent: i.parent ?? "base", env: i.env, script: i.script } })}
                  />
                  <Button
                    size="small"
                    appearance="subtle"
                    icon={<Delete16Regular />}
                    aria-label={t("profiles.delete")}
                    disabled={i.state === "building"}
                    onClick={() => run("images_delete", { id: i.id })}
                  />
                </>
              )}
            </div>
            <Caption1>{i.builtin ? t("settings.images.baseDescription") : i.description}</Caption1>
            {i.parent && <Caption1>{t("settings.images.basedOn", { name: nameOf(i.parent) })}</Caption1>}
            {i.built_at && (
              <Caption1>
                {t("settings.images.builtAt", { date: new Date(i.built_at * 1000).toLocaleString() })}
                {i.size ? ` · ${(i.size / 1e6).toFixed(0)} MB` : ""}
              </Caption1>
            )}
            {i.error && i.state !== "building" && <Body1 style={{ color: tokens.colorPaletteRedForeground1 }}>{i.error}</Body1>}
            {i.log.length > 0 && (i.state === "building" || i.error) && <pre className={styles.log}>{i.log.join("\n")}</pre>}
          </div>
        ))}
      </div>

      <Dialog open={editing !== null} onOpenChange={(_, d) => !d.open && setEditing(null)}>
        <DialogSurface>
          <DialogBody>
            <DialogTitle>{editing?.id ? t("settings.images.edit") : t("settings.images.new")}</DialogTitle>
            <DialogContent className={styles.form}>
              {editing && (
                <>
                  <Field label={t("profiles.name")} required>
                    <Input value={editing.spec.name} maxLength={80} onChange={(_, d) => setEditing({ ...editing, spec: { ...editing.spec, name: d.value } })} />
                  </Field>
                  <Field label={t("profiles.description")}>
                    <Input value={editing.spec.description} onChange={(_, d) => setEditing({ ...editing, spec: { ...editing.spec, description: d.value } })} />
                  </Field>
                  <Field label={t("settings.images.parent")} hint={t("settings.images.parentHint")}>
                    <Select value={editing.spec.parent} onChange={(_, d) => setEditing({ ...editing, spec: { ...editing.spec, parent: d.value } })}>
                      {images
                        .filter((x) => x.id !== editing.id)
                        .map((x) => (
                          <option key={x.id} value={x.id}>
                            {nameOf(x.id)}
                          </option>
                        ))}
                    </Select>
                  </Field>
                  <Field label={t("settings.images.env")} hint={t("settings.images.envHint")}>
                    <div className={styles.form}>
                      {editing.spec.env.map((v, n) => (
                        <div key={n} className={styles.envRow}>
                          <Input
                            className={styles.mono}
                            placeholder="NAME"
                            value={v.name}
                            onChange={(_, d) => setEnv(n, { name: d.value })}
                          />
                          <Input className={`${styles.grow} ${styles.mono}`} value={v.value} onChange={(_, d) => setEnv(n, { value: d.value })} />
                          <Button
                            appearance="subtle"
                            icon={<Delete16Regular />}
                            aria-label={t("profiles.delete")}
                            onClick={() => setEditing({ ...editing, spec: { ...editing.spec, env: editing.spec.env.filter((_, j) => j !== n) } })}
                          />
                        </div>
                      ))}
                      <div>
                        <Button
                          size="small"
                          icon={<Add16Regular />}
                          onClick={() => setEditing({ ...editing, spec: { ...editing.spec, env: [...editing.spec.env, { name: "", value: "" }] } })}
                        >
                          {t("profiles.addVar")}
                        </Button>
                      </div>
                    </div>
                  </Field>
                  <Field label={t("settings.images.script")} hint={t("settings.images.scriptHint")}>
                    <Textarea
                      rows={10}
                      className={styles.mono}
                      resize="vertical"
                      value={editing.spec.script}
                      onChange={(_, d) => setEditing({ ...editing, spec: { ...editing.spec, script: d.value } })}
                    />
                  </Field>
                </>
              )}
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={() => setEditing(null)}>
                {t("common.cancel")}
              </Button>
              <Button
                appearance="primary"
                disabled={!editing?.spec.name.trim()}
                onClick={async () => editing && (await run("images_save", { id: editing.id, spec: editing.spec })) && setEditing(null)}
              >
                {t("common.save")}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </>
  );
}
