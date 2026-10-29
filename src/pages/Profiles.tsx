import { useCallback, useEffect, useState } from "react";
import { PageHeader } from "../components/PageHeader";
import { useTranslation } from "react-i18next";
import {
  Body1,
  Button,
  Badge,
  Card,
  Subtitle2,
  Combobox,
  Option,
  CardHeader,
  Caption1,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogTitle,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Select,
  Textarea,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { Add20Regular, Copy20Regular, Delete20Regular } from "@fluentui/react-icons";
import { FormDialogSurface } from "../components/FormDialogSurface";
import { ResetDemoButton } from "../components/ResetDemoButton";
import { inTauri } from "../prefs";
import { MODELS } from "../llm";
import { useNotify } from "../notifications";
import { call } from "../tauri";
import { useProjectId } from "../project";

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
  allow_hosts: string[];
}

interface ImageInfo {
  id: string;
  name: string;
  state: string;
}

interface Entry extends Profile {
  id: string;
}

const EMPTY: Profile = { name: "", description: "", system_prompt: "", env: [], model: null, effort: null, cpus: null, memory_mb: 2048, image: "base", allow_hosts: [] };

const useStyles = makeStyles({
  list: { display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))", gap: tokens.spacingHorizontalL },
  card: { cursor: "pointer", transitionProperty: "box-shadow", transitionDuration: "120ms", ":hover": { boxShadow: tokens.shadow8 } },
  chips: { display: "flex", flexWrap: "wrap", gap: tokens.spacingHorizontalXS },
  desc: { color: tokens.colorNeutralForeground3 },
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM },
  envRow: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "flex-start" },
  grow: { flex: 1 },
  mono: { fontFamily: tokens.fontFamilyMonospace },
});

export function Profiles() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const project = useProjectId();
  const desktop = inTauri();

  const [entries, setEntries] = useState<Entry[]>([]);
  const [images, setImages] = useState<ImageInfo[]>([]);
  const [secrets, setSecrets] = useState<string[]>([]);
  const [injections, setInjections] = useState<string[]>([]);
  const [variables, setVariables] = useState<string[]>([]);
  const [editing, setEditing] = useState<{ id: string | null; profile: Profile } | null>(null);
  // closing only hides the dialog: its content stays until the next open, so the title cannot flip to "New profile" while it fades out
  const [open, setOpen] = useState(false);
  const edit = (v: { id: string | null; profile: Profile }) => {
    setEditing(v);
    setOpen(true);
  };

  const load = useCallback(async () => {
    try {
      setEntries(await call<Entry[]>("profiles_list", { project }));
      setImages(await call<ImageInfo[]>("images_list", { project }));
      // what a profile can refer to: the project's own and the global ones
      const both = async <T,>(cmd: string, pick: (x: T) => string) => {
        const [own, global] = await Promise.all([call<T[]>(cmd, { project }), call<T[]>(cmd, { project: null })]);
        return [...new Set([...own, ...global].map(pick))];
      };
      setSecrets(await both<string>("secrets_list", (n) => n));
      setInjections(await both<{ name: string }>("injections_get", (i) => i.name));
      setVariables(await both<{ name: string }>("variables_list", (v) => v.name));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify, project]);

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  const save = async () => {
    if (!editing) return;
    try {
      await call("profiles_save", { project, id: editing.id, profile: editing.profile });
      setOpen(false);
      await load();
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const remove = async (id: string) => {
    try {
      await call("profiles_delete", { project, id });
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
      <PageHeader
        title={t("profiles.title")}
        actions={
          <>
            <ResetDemoButton command="profiles_reset_demo" what="profiles" onDone={load} />
            <Button appearance="primary" icon={<Add20Regular />} disabled={!desktop} onClick={() => edit({ id: null, profile: EMPTY })}>
              {t("profiles.new")}
            </Button>
          </>
        }
      />
      {!desktop && <Body1 block>{t("profiles.desktopOnly")}</Body1>}
      {desktop && entries.length === 0 && <Body1 block>{t("profiles.empty")}</Body1>}
      <div className={styles.list}>
        {entries.map((e) => (
          <Card key={e.id} className={styles.card} onClick={() => edit({ id: e.id, profile: e })}>
            <CardHeader
              header={<Subtitle2>{e.name}</Subtitle2>}
              description={<Caption1 className={styles.desc}>{e.description || t("profiles.noDescription")}</Caption1>}
              action={
                <>
                  <Button
                    appearance="subtle"
                    icon={<Copy20Regular />}
                    aria-label={t("profiles.duplicate")}
                    onClick={(ev) => {
                      ev.stopPropagation();
                      edit({ id: null, profile: { ...e, name: t("profiles.copyName", { name: e.name }) } });
                    }}
                  />
                  <Button
                    appearance="subtle"
                    icon={<Delete20Regular />}
                    aria-label={t("profiles.delete")}
                    onClick={(ev) => {
                      ev.stopPropagation();
                      void remove(e.id);
                    }}
                  />
                </>
              }
            />
            <div className={styles.chips}>
              <Badge appearance="tint" color="brand" size="small">{e.image}</Badge>
              {e.model && <Badge appearance="outline" size="small">{e.model}</Badge>}
            </div>
          </Card>
        ))}
      </div>

      <Dialog surfaceMotion={null} open={open} onOpenChange={(_, d) => setOpen(d.open)}>
        <FormDialogSurface>
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
                          <Combobox
                            className={styles.grow}
                            input={{ className: styles.mono }}
                            freeform
                            placeholder={t("profiles.envValue")}
                            value={v.value}
                            selectedOptions={[]}
                            onChange={(e) => setEnv(i, { value: e.target.value })}
                            onOptionSelect={(_, d) => d.optionValue && setEnv(i, { value: d.optionValue })}
                          >
                            {secrets.map((n) => (
                              <Option key={`s:${n}`} value={`\${secret:${n}}`}>{`\${secret:${n}}`}</Option>
                            ))}
                            {injections.map((n) => (
                              <Option key={`i:${n}`} value={`\${injection:${n}}`}>{`\${injection:${n}}`}</Option>
                            ))}
                            {variables.map((n) => (
                              <Option key={`v:${n}`} value={`\${var:${n}}`}>{`\${var:${n}}`}</Option>
                            ))}
                          </Combobox>
                          <Button
                            appearance="subtle"
                            icon={<Delete20Regular />}
                            aria-label={t("profiles.delete")}
                            onClick={() => set({ env: editing.profile.env.filter((_, j) => j !== i) })}
                          />
                        </div>
                      ))}
                      {editing.profile.env.some((v) => v.value.includes("${secret:")) && (
                        <MessageBar intent="warning">
                          <MessageBarBody>{t("profiles.secretWarning", { names: editing.profile.env.filter((v) => v.value.includes("${secret:")).map((v) => v.name).join(", ") })}</MessageBarBody>
                        </MessageBar>
                      )}
                      <div>
                        <Button icon={<Add20Regular />} onClick={() => set({ env: [...editing.profile.env, { name: "", value: "" }] })}>
                          {t("profiles.addVar")}
                        </Button>
                      </div>
                    </div>
                  </Field>
                  <Field label={t("profiles.allowHosts")} hint={t("profiles.allowHostsHint")}>
                    <Textarea
                      rows={4}
                      className={styles.mono}
                      placeholder={"github.com\n*.githubusercontent.com"}
                      value={editing.profile.allow_hosts.join("\n")}
                      onChange={(_, d) => set({ allow_hosts: d.value === "" ? [] : d.value.split("\n") })}
                    />
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
              <Button appearance="secondary" onClick={() => setOpen(false)}>
                {t("common.cancel")}
              </Button>
              <Button appearance="primary" disabled={!editing?.profile.name.trim() || (editing?.profile.memory_mb ?? 0) < 1024} onClick={save}>
                {t("common.save")}
              </Button>
            </DialogActions>
          </DialogBody>
        </FormDialogSurface>
      </Dialog>
    </>
  );
}
