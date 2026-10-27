import { useCallback, useEffect, useState } from "react";
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
  Select,
  Textarea,
  Title2,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { Add16Regular, Delete16Regular } from "@fluentui/react-icons";
import { inTauri } from "../prefs";
import { MODELS } from "../llm";
import { useNotify } from "../notifications";

interface EnvVar {
  name: string;
  value: string;
}

interface Profile {
  name: string;
  description: string;
  system_prompt: string;
  env: EnvVar[];
  model: string | null;
  effort: string | null;
  cpus: number | null;
  memory_mb: number;
  image: string;
}

interface ImageInfo {
  id: string;
  name: string;
  state: string;
}

interface Entry extends Profile {
  id: string;
}

const EMPTY: Profile = { name: "", description: "", system_prompt: "", env: [], model: null, effort: null, cpus: null, memory_mb: 2048, image: "base" };

const useStyles = makeStyles({
  head: { display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: tokens.spacingVerticalL },
  list: { display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))", gap: tokens.spacingHorizontalM },
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM },
  envRow: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "flex-start" },
  grow: { flex: 1 },
  mono: { fontFamily: tokens.fontFamilyMonospace },
});

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export function Profiles() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();

  const [entries, setEntries] = useState<Entry[]>([]);
  const [images, setImages] = useState<ImageInfo[]>([]);
  const [editing, setEditing] = useState<{ id: string | null; profile: Profile } | null>(null);

  const load = useCallback(async () => {
    try {
      setEntries(await call<Entry[]>("profiles_list"));
      setImages(await call<ImageInfo[]>("images_list"));
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
      await call("profiles_save", { id: editing.id, profile: editing.profile });
      setEditing(null);
      await load();
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const remove = async (id: string) => {
    try {
      await call("profiles_delete", { id });
      await load();
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const set = (patch: Partial<Profile>) => editing && setEditing({ ...editing, profile: { ...editing.profile, ...patch } });
  const setEnv = (i: number, patch: Partial<EnvVar>) =>
    set({ env: editing!.profile.env.map((v, j) => (j === i ? { ...v, ...patch } : v)) });

  return (
    <>
      <div className={styles.head}>
        <Title2 as="h1">{t("profiles.title")}</Title2>
        <Button appearance="primary" icon={<Add16Regular />} disabled={!desktop} onClick={() => setEditing({ id: null, profile: EMPTY })}>
          {t("profiles.new")}
        </Button>
      </div>
      {!desktop && <Body1 block>{t("profiles.desktopOnly")}</Body1>}
      {desktop && entries.length === 0 && <Body1 block>{t("profiles.empty")}</Body1>}
      <div className={styles.list}>
        {entries.map((e) => (
          <Card key={e.id} onClick={() => setEditing({ id: e.id, profile: e })}>
            <CardHeader
              header={<Body1><b>{e.name}</b></Body1>}
              description={<Caption1>{e.description || t("profiles.noDescription")}</Caption1>}
              action={
                <Button
                  appearance="subtle"
                  icon={<Delete16Regular />}
                  aria-label={t("profiles.delete")}
                  onClick={(ev) => {
                    ev.stopPropagation();
                    void remove(e.id);
                  }}
                />
              }
            />
          </Card>
        ))}
      </div>

      <Dialog open={editing !== null} onOpenChange={(_, d) => !d.open && setEditing(null)}>
        <DialogSurface>
          <DialogBody>
            <DialogTitle>{editing?.id ? t("profiles.edit") : t("profiles.new")}</DialogTitle>
            <DialogContent className={styles.form}>
              {editing && (
                <>
                  <Field label={t("profiles.name")} required>
                    <Input value={editing.profile.name} maxLength={80} onChange={(_, d) => set({ name: d.value })} />
                  </Field>
                  <Field label={t("profiles.description")}>
                    <Input value={editing.profile.description} onChange={(_, d) => set({ description: d.value })} />
                  </Field>
                  <Field label={t("profiles.systemPrompt")} hint={t("profiles.systemPromptHint")}>
                    <Textarea rows={5} value={editing.profile.system_prompt} onChange={(_, d) => set({ system_prompt: d.value })} />
                  </Field>
                  <Field label={t("profiles.env")} hint={t("profiles.envHint")}>
                    <div className={styles.form}>
                      {editing.profile.env.map((v, i) => (
                        <div key={i} className={styles.envRow}>
                          <Input
                            className={styles.mono}
                            placeholder="NAME"
                            value={v.name}
                            onChange={(_, d) => setEnv(i, { name: d.value })}
                          />
                          <Input className={`${styles.grow} ${styles.mono}`} value={v.value} onChange={(_, d) => setEnv(i, { value: d.value })} />
                          <Button
                            appearance="subtle"
                            icon={<Delete16Regular />}
                            aria-label={t("profiles.delete")}
                            onClick={() => set({ env: editing.profile.env.filter((_, j) => j !== i) })}
                          />
                        </div>
                      ))}
                      <div>
                        <Button size="small" icon={<Add16Regular />} onClick={() => set({ env: [...editing.profile.env, { name: "", value: "" }] })}>
                          {t("profiles.addVar")}
                        </Button>
                      </div>
                    </div>
                  </Field>
                  <Field label={t("profiles.model")} hint={t("profiles.modelHint")}>
                    <Select value={editing.profile.model ?? ""} onChange={(_, d) => set({ model: d.value || null })}>
                      <option value="">{t("profiles.modelDefault")}</option>
                      {editing.profile.model && !MODELS.some((m) => m.id === editing.profile.model) && (
                        <option value={editing.profile.model}>{editing.profile.model}</option>
                      )}
                      {MODELS.map((m) => (
                        <option key={m.id} value={m.id}>
                          {m.label}
                        </option>
                      ))}
                    </Select>
                  </Field>
                  <Field label={t("profiles.effort")} hint={t("profiles.effortHint")}>
                    <Select value={editing.profile.effort ?? ""} onChange={(_, d) => set({ effort: d.value || null })}>
                      <option value="">{t("profiles.effortDefault")}</option>
                      {["low", "medium", "high", "xhigh", "max"].map((e) => (
                        <option key={e} value={e}>
                          {e}
                        </option>
                      ))}
                    </Select>
                  </Field>
                  <Field label={t("profiles.cpus")} hint={t("profiles.cpusHint")}>
                    <Input
                      type="number"
                      min={1}
                      max={256}
                      placeholder={t("profiles.cpusAll")}
                      value={editing.profile.cpus === null ? "" : String(editing.profile.cpus)}
                      onChange={(_, d) => set({ cpus: d.value === "" ? null : Math.min(256, Math.max(1, Math.floor(Number(d.value)) || 1)) })}
                    />
                  </Field>
                  <Field label={t("profiles.memory")} hint={t("profiles.memoryHint")}>
                    <Input
                      type="number"
                      min={1024}
                      step={512}
                      value={String(editing.profile.memory_mb)}
                      onChange={(_, d) => set({ memory_mb: Math.floor(Number(d.value)) || 0 })}
                    />
                  </Field>
                  <Field label={t("profiles.image")} hint={t("profiles.imageHint")}>
                    <Select value={editing.profile.image} onChange={(_, d) => set({ image: d.value })}>
                      {images.map((i) => (
                        <option key={i.id} value={i.id}>
                          {i.name}
                        </option>
                      ))}
                    </Select>
                  </Field>
                </>
              )}
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={() => setEditing(null)}>
                {t("common.cancel")}
              </Button>
              <Button appearance="primary" disabled={!editing?.profile.name.trim() || (editing?.profile.memory_mb ?? 0) < 1024} onClick={save}>
                {t("common.save")}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </>
  );
}
