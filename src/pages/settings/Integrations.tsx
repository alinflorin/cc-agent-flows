import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Body1,
  Button,
  Caption1,
  Card,
  CardHeader,
  Combobox,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogTitle,
  Dropdown,
  Field,
  Input,
  Option,
  Subtitle2,
  Textarea,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { Add20Regular, Copy20Regular, Delete20Regular } from "@fluentui/react-icons";
import { SettingsIntro } from "../../components/SettingsIntro";
import { RefCombobox } from "../../components/RefCombobox";
import { FormDialogSurface } from "../../components/FormDialogSurface";
import { inTauri } from "../../prefs";
import { useNotify } from "../../notifications";
import { call } from "../../tauri";
import { useProject } from "../../project";

const NAME = /^[A-Za-z_][A-Za-z0-9_]{0,63}$/;

interface Rule {
  host: string;
  path: string;
  methods: string[];
}

interface Injection {
  name: string;
  rules: Rule[];
  header: string;
  kind: "bearer" | "basic" | "raw";
  username: string;
  secret: string;
  source: "secret" | "var" | "text";
  value: string;
  placeholder: string;
}

interface EnvVar {
  name: string;
  value: string;
}

/** What the backend returns and takes back: values of variables, secrets write-only (`value` is the new one, `set` tells whether one is saved). */
interface Integration {
  name: string;
  description: string;
  hosts: string[];
  env: EnvVar[];
  variables: { name: string; value: string }[];
  secrets: { name: string; set?: boolean; value?: string }[];
  injections: Injection[];
}

const EMPTY: Integration = { name: "", description: "", hosts: [], env: [], variables: [], secrets: [], injections: [] };

const NEW_INJECTION: Injection = {
  name: "",
  rules: [{ host: "", path: "", methods: [] }],
  header: "Authorization",
  kind: "bearer",
  username: "",
  secret: "",
  source: "secret",
  value: "",
  placeholder: "",
};

const useStyles = makeStyles({
  list: { display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))", gap: tokens.spacingHorizontalM, marginTop: tokens.spacingVerticalL },
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM },
  row: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "flex-start" },
  cardRow: { marginTop: "auto", display: "flex", gap: tokens.spacingHorizontalS, alignItems: "flex-start" },
  grow: { flex: 1, minWidth: 0 },
  mono: { fontFamily: tokens.fontFamilyMonospace },
  box: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalS, padding: tokens.spacingHorizontalM, border: `1px solid ${tokens.colorNeutralStroke2}`, borderRadius: tokens.borderRadiusMedium },
  chips: { color: tokens.colorNeutralForeground3 },
});

export function IntegrationsSettings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();
  const project = useProject();

  const [list, setList] = useState<Integration[]>([]);
  // `previous` = the saved name being edited (null = new); kept after closing so the dialog does not flicker while it fades out
  const [editing, setEditing] = useState<{ previous: string | null; value: Integration } | null>(null);
  const [open, setOpen] = useState(false);

  const load = useCallback(async () => {
    try {
      setList(await call<Integration[]>("integrations_list", { project }));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify, project]);

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  const edit = (previous: string | null, value: Integration) => {
    setEditing({ previous, value });
    setOpen(true);
  };

  const v = editing?.value;
  const set = (patch: Partial<Integration>) => editing && setEditing({ ...editing, value: { ...editing.value, ...patch } });
  const setInj = (i: number, patch: Partial<Injection>) => set({ injections: v!.injections.map((x, j) => (j === i ? { ...x, ...patch } : x)) });
  const setRule = (i: number, r: number, patch: Partial<Rule>) =>
    setInj(i, { rules: v!.injections[i].rules.map((x, j) => (j === r ? { ...x, ...patch } : x)) });

  const save = async () => {
    if (!editing || !v) return;
    const integration = {
      ...v,
      hosts: v.hosts.map((h) => h.trim()).filter(Boolean),
      secrets: v.secrets.map(({ name, value }) => ({ name, value: value ? value : null })),
    };
    try {
      await call("integrations_save", { project, previous: editing.previous, integration });
      setOpen(false);
      notify(t("settings.integrations.saved"), "success");
      void load();
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const remove = async (name: string) => {
    try {
      await call("integrations_delete", { project, name });
      void load();
    } catch (e) {
      notify(String(e), "error");
    }
  };

  // what an environment variable value can refer to: this integration's own variables, secrets and injections
  const refs = v ? [...v.variables.map((x) => `\${var:${x.name}}`), ...v.injections.map((x) => `\${injection:${x.name}}`), ...v.secrets.map((x) => `\${secret:${x.name}}`)].filter((r) => !/:\}/.test(r)) : [];
  const userRefs = v ? [...v.variables.map((x) => `\${var:${x.name}}`), ...v.secrets.map((x) => `\${secret:${x.name}}`)] : [];

  const names = (xs: { name: string }[]) => xs.map((x) => x.name);
  const dup = (xs: string[]) => new Set(xs).size !== xs.length;
  const ready =
    !!v &&
    !!v.name.trim() &&
    v.variables.every((x) => NAME.test(x.name) && x.value.trim()) &&
    v.secrets.every((x) => NAME.test(x.name) && (x.set || !!x.value)) &&
    v.injections.every((x) => NAME.test(x.name) && x.rules.every((r) => r.host.trim()) && (x.source !== "secret" || !!x.secret) && (x.kind !== "basic" || !!x.username)) &&
    v.env.every((x) => x.name.trim()) &&
    !dup(names(v.variables)) &&
    !dup(names(v.secrets)) &&
    !dup(names(v.injections));

  return (
    <>
      <SettingsIntro text={t("settings.integrations.intro")}>
        {!desktop && <Body1 block>{t("settings.integrations.desktopOnly")}</Body1>}
        <Button appearance="primary" icon={<Add20Regular />} disabled={!desktop} onClick={() => edit(null, EMPTY)}>
          {t("settings.integrations.new")}
        </Button>
      </SettingsIntro>
      {desktop && list.length === 0 && (
        <Caption1 block style={{ marginTop: 16 }}>
          {t("settings.integrations.empty")}
        </Caption1>
      )}
      <div className={styles.list}>
        {list.map((x) => (
          <Card key={x.name} style={{ height: "100%" }}>
            <CardHeader header={<Subtitle2>{x.name}</Subtitle2>} description={<Caption1>{x.description || t("settings.integrations.noDescription")}</Caption1>} />
            <Caption1 className={styles.chips}>
              {t("settings.integrations.summary", { variables: x.variables.length, secrets: x.secrets.length, injections: x.injections.length, hosts: x.hosts.length })}
            </Caption1>
            <div className={styles.cardRow}>
              <Button onClick={() => edit(x.name, x)}>{t("settings.integrations.edit")}</Button>
              <Button
                appearance="subtle"
                icon={<Copy20Regular />}
                aria-label={t("settings.integrations.duplicate")}
                // a copy keeps the variables' values, injections and env, but not the secrets' values: they are asked again
                onClick={() => edit(null, { ...x, name: t("settings.integrations.copyName", { name: x.name }), secrets: x.secrets.map((s) => ({ name: s.name })) })}
              />
              <Button appearance="subtle" icon={<Delete20Regular />} aria-label={t("settings.integrations.delete")} onClick={() => void remove(x.name)} />
            </div>
          </Card>
        ))}
      </div>

      <Dialog surfaceMotion={null} open={open} onOpenChange={(_, d) => setOpen(d.open)}>
        <FormDialogSurface>
          <DialogBody>
            <DialogTitle>{editing?.previous === null ? t("settings.integrations.new") : t("settings.integrations.editTitle")}</DialogTitle>
            <DialogContent style={{ overflowX: "hidden" }}>
              {v && (
                <div className={styles.form}>
                  <Field required label={t("settings.integrations.name")}>
                    <Input value={v.name} onChange={(_, d) => set({ name: d.value })} />
                  </Field>
                  <Field label={t("settings.integrations.description")}>
                    <Input value={v.description} onChange={(_, d) => set({ description: d.value })} />
                  </Field>

                  <Field label={t("settings.integrations.variables")} hint={t("settings.integrations.variablesHint")}>
                    <div className={styles.form}>
                      {v.variables.map((x, i) => (
                        <div key={i} className={styles.row}>
                          <Input className={styles.mono} placeholder="NAME" value={x.name} onChange={(_, d) => set({ variables: v.variables.map((y, j) => (j === i ? { ...y, name: d.value } : y)) })} />
                          <Input className={`${styles.grow} ${styles.mono}`} placeholder={t("settings.integrations.value")} value={x.value} onChange={(_, d) => set({ variables: v.variables.map((y, j) => (j === i ? { ...y, value: d.value } : y)) })} />
                          <Button appearance="subtle" icon={<Delete20Regular />} aria-label={t("settings.integrations.delete")} onClick={() => set({ variables: v.variables.filter((_, j) => j !== i) })} />
                        </div>
                      ))}
                      <div>
                        <Button icon={<Add20Regular />} onClick={() => set({ variables: [...v.variables, { name: "", value: "" }] })}>
                          {t("settings.integrations.addVariable")}
                        </Button>
                      </div>
                    </div>
                  </Field>

                  <Field label={t("settings.integrations.secrets")} hint={t("settings.integrations.secretsHint")}>
                    <div className={styles.form}>
                      {v.secrets.map((x, i) => (
                        <div key={i} className={styles.row}>
                          <Input className={styles.mono} placeholder="NAME" value={x.name} disabled={!!x.set} onChange={(_, d) => set({ secrets: v.secrets.map((y, j) => (j === i ? { ...y, name: d.value } : y)) })} />
                          <Input
                            className={styles.grow}
                            type="password"
                            autoComplete="off"
                            placeholder={x.set ? t("settings.integrations.secretKeep") : t("settings.integrations.value")}
                            value={x.value ?? ""}
                            onChange={(_, d) => set({ secrets: v.secrets.map((y, j) => (j === i ? { ...y, value: d.value } : y)) })}
                          />
                          <Button appearance="subtle" icon={<Delete20Regular />} aria-label={t("settings.integrations.delete")} onClick={() => set({ secrets: v.secrets.filter((_, j) => j !== i) })} />
                        </div>
                      ))}
                      <div>
                        <Button icon={<Add20Regular />} onClick={() => set({ secrets: [...v.secrets, { name: "", value: "" }] })}>
                          {t("settings.integrations.addSecret")}
                        </Button>
                      </div>
                    </div>
                  </Field>

                  <Field label={t("settings.integrations.injections")} hint={t("settings.integrations.injectionsHint")}>
                    <div className={styles.form}>
                      {v.injections.map((x, i) => (
                        <div key={i} className={styles.box}>
                          <div className={styles.row}>
                            <Input className={`${styles.grow} ${styles.mono}`} placeholder="NAME" value={x.name} onChange={(_, d) => setInj(i, { name: d.value })} />
                            <Button appearance="subtle" icon={<Delete20Regular />} aria-label={t("settings.integrations.delete")} onClick={() => set({ injections: v.injections.filter((_, j) => j !== i) })} />
                          </div>
                          {x.rules.map((r, k) => (
                            <div key={k} className={styles.row}>
                              <div className={styles.grow}><RefCombobox placeholder="api.example.com" value={r.host} options={v.variables.filter((y) => y.name).map((y) => `\${var:${y.name}}`)} onChange={(host) => setRule(i, k, { host })} /></div>
                              <Input className={`${styles.grow} ${styles.mono}`} placeholder="/*" value={r.path} onChange={(_, d) => setRule(i, k, { path: d.value })} />
                              <Button appearance="subtle" icon={<Delete20Regular />} aria-label={t("settings.integrations.delete")} disabled={x.rules.length < 2} onClick={() => setInj(i, { rules: x.rules.filter((_, j) => j !== k) })} />
                            </div>
                          ))}
                          <div>
                            <Button size="small" icon={<Add20Regular />} onClick={() => setInj(i, { rules: [...x.rules, { host: "", path: "", methods: [] }] })}>
                              {t("settings.injections.addRule")}
                            </Button>
                          </div>
                          <div className={styles.row}>
                            <Input className={`${styles.grow} ${styles.mono}`} placeholder={t("settings.injections.header")} value={x.header} onChange={(_, d) => setInj(i, { header: d.value })} />
                            <Dropdown className={styles.grow} value={t(`settings.injections.kinds.${x.kind}`)} selectedOptions={[x.kind]} onOptionSelect={(_, d) => setInj(i, { kind: d.optionValue as Injection["kind"] })}>
                              {(["bearer", "basic", "raw"] as const).map((k) => (
                                <Option key={k} value={k}>
                                  {t(`settings.injections.kinds.${k}`)}
                                </Option>
                              ))}
                            </Dropdown>
                          </div>
                          {x.kind === "basic" && (
                            <Combobox
                              freeform
                              placeholder={t("settings.injections.username")}
                              input={{ className: styles.mono }}
                              value={x.username}
                              selectedOptions={[x.username]}
                              onChange={(e) => setInj(i, { username: e.target.value })}
                              onOptionSelect={(_, d) => setInj(i, { username: d.optionValue ?? "" })}
                            >
                              {userRefs.map((r) => (
                                <Option key={r} value={r}>
                                  {r}
                                </Option>
                              ))}
                            </Combobox>
                          )}
                          <Dropdown
                            placeholder={t("settings.integrations.injectionSecret")}
                            value={x.source === "secret" ? x.secret : x.source === "var" ? `\${var:${x.value}}` : t("settings.injections.fixedText")}
                            selectedOptions={x.source === "secret" ? [x.secret] : []}
                            onOptionSelect={(_, d) => setInj(i, { source: "secret", secret: d.optionValue ?? "" })}
                          >
                            {v.secrets.filter((s) => s.name).map((s) => (
                              <Option key={s.name} value={s.name}>
                                {s.name}
                              </Option>
                            ))}
                          </Dropdown>
                        </div>
                      ))}
                      <div>
                        <Button icon={<Add20Regular />} onClick={() => set({ injections: [...v.injections, NEW_INJECTION] })}>
                          {t("settings.integrations.addInjection")}
                        </Button>
                      </div>
                    </div>
                  </Field>

                  <Field label={t("profiles.allowHosts")} hint={t("settings.integrations.hostsHint")}>
                    <Textarea rows={3} className={styles.mono} placeholder={"api.example.com\n${var:HOST}"} value={v.hosts.join("\n")} onChange={(_, d) => set({ hosts: d.value === "" ? [] : d.value.split("\n") })} />
                  </Field>

                  <Field label={t("settings.integrations.env")} hint={t("settings.integrations.envHint")}>
                    <div className={styles.form}>
                      {v.env.map((x, i) => (
                        <div key={i} className={styles.row}>
                          <Input className={styles.mono} placeholder="NAME" value={x.name} onChange={(_, d) => set({ env: v.env.map((y, j) => (j === i ? { ...y, name: d.value } : y)) })} />
                          <Combobox
                            className={styles.grow}
                            input={{ className: styles.mono }}
                            freeform
                            placeholder={t("profiles.envValue")}
                            value={x.value}
                            selectedOptions={[]}
                            onChange={(e) => set({ env: v.env.map((y, j) => (j === i ? { ...y, value: e.target.value } : y)) })}
                            onOptionSelect={(_, d) => d.optionValue && set({ env: v.env.map((y, j) => (j === i ? { ...y, value: d.optionValue! } : y)) })}
                          >
                            {refs.map((r) => (
                              <Option key={r} value={r}>
                                {r}
                              </Option>
                            ))}
                          </Combobox>
                          <Button appearance="subtle" icon={<Delete20Regular />} aria-label={t("settings.integrations.delete")} onClick={() => set({ env: v.env.filter((_, j) => j !== i) })} />
                        </div>
                      ))}
                      <div>
                        <Button icon={<Add20Regular />} onClick={() => set({ env: [...v.env, { name: "", value: "" }] })}>
                          {t("profiles.addVar")}
                        </Button>
                      </div>
                    </div>
                  </Field>
                </div>
              )}
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={() => setOpen(false)}>
                {t("common.cancel")}
              </Button>
              <Button appearance="primary" disabled={!ready} onClick={() => void save()}>
                {t("common.save")}
              </Button>
            </DialogActions>
          </DialogBody>
        </FormDialogSurface>
      </Dialog>
    </>
  );
}
