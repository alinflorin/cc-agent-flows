import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Body1,
  Button,
  Caption1,
  Card,
  CardHeader,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogTitle,
  Combobox,
  Dropdown,
  Field,
  Input,
  Option,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { Add20Regular, Delete20Regular } from "@fluentui/react-icons";
import { SettingsIntro } from "../../components/SettingsIntro";
import { EmptyState, useConfirmDelete } from "../../components/Confirm";
import { FormDialogSurface } from "../../components/FormDialogSurface";
import { RefCombobox } from "../../components/RefCombobox";
import { ResetDemoButton } from "../../components/ResetDemoButton";
import { inTauri } from "../../prefs";
import { useNotify } from "../../notifications";
import { call } from "../../tauri";
import { useProject } from "../../project";

interface InjectionRule {
  host: string;
  path: string;
  methods: string[];
}

interface Injection {
  name: string;
  rules: InjectionRule[];
  header: string;
  kind: "bearer" | "basic" | "raw";
  username: string;
  secret: string;
  source: "secret" | "var" | "text";
  value: string;
  placeholder: string;
}

const METHODS = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

const EMPTY: Injection = {
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
  introActions: { display: "flex", alignItems: "center", gap: tokens.spacingHorizontalS },
  list: { display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))", gap: tokens.spacingHorizontalM, marginTop: tokens.spacingVerticalL },
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM },
  row: { marginTop: "auto", display: "flex", gap: tokens.spacingHorizontalS, alignItems: "flex-start" },
  grow: { flex: 1, minWidth: 0 },
  rule: { display: "grid", gridTemplateColumns: "minmax(0, 1fr) minmax(0, 1fr) 140px auto", gap: tokens.spacingHorizontalS, alignItems: "start" },
  mono: { fontFamily: tokens.fontFamilyMonospace },
});

export function InjectionsSettings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();
  const project = useProject();
  const confirm = useConfirmDelete();
  const [saving, setSaving] = useState(false);

  const [list, setList] = useState<Injection[]>([]);
  const [secrets, setSecrets] = useState<string[]>([]);
  const [variables, setVariables] = useState<string[]>([]);
  // the injection being edited and its position in the list (null = new); kept after closing so the dialog does not flicker while it fades out
  const [editing, setEditing] = useState<{ at: number | null; value: Injection } | null>(null);
  const [open, setOpen] = useState(false);

  const load = useCallback(async () => {
    try {
      setList(await call<Injection[]>("injections_get", { project }));
      setSecrets(await call<string[]>("secrets_list", { project }));
      setVariables((await call<{ name: string }[]>("variables_list", { project })).map((x) => x.name));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify, project]);

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  const edit = (at: number | null, value: Injection) => {
    setEditing({ at, value });
    setOpen(true);
  };

  const persist = async (next: Injection[]) => {
    try {
      setList(await call<Injection[]>("injections_save", { project, injections: next }));
      return true;
    } catch (e) {
      notify(String(e), "error");
      return false;
    }
  };

  const save = async () => {
    if (!editing) return;
    const value = editing.value;
    const next = editing.at === null ? [...list, value] : list.map((x, i) => (i === editing.at ? value : x));
    setSaving(true);
    if (await persist(next)) {
      setOpen(false);
      notify(t("settings.injections.saved"), "success");
    }
    setSaving(false);
  };

  const set = (patch: Partial<Injection>) => editing && setEditing({ ...editing, value: { ...editing.value, ...patch } });
  const setRule = (i: number, patch: Partial<InjectionRule>) =>
    set({ rules: editing!.value.rules.map((r, j) => (j === i ? { ...r, ...patch } : r)) });

  const v = editing?.value;
  const ready = !!v && !!v.name && (v.source === "secret" ? !!v.secret : !!v.value.trim()) && !!v.header && v.rules.length > 0 && v.rules.every((r) => r.host.trim()) && (v.kind !== "basic" || !!v.username);

  return (
    <>
      <SettingsIntro text={t("settings.injections.intro")}>
        {!desktop && <Body1 block>{t("settings.injections.desktopOnly")}</Body1>}
        <div className={styles.introActions}>
          <Button appearance="primary" icon={<Add20Regular />} disabled={!desktop} onClick={() => edit(null, EMPTY)}>
            {t("settings.injections.new")}
          </Button>
          <ResetDemoButton command="injections_reset_demo" what="injections" onDone={() => void load()} />
        </div>
      </SettingsIntro>
      {desktop && list.length === 0 && <EmptyState>{t("settings.injections.empty")}</EmptyState>}
      <div className={styles.list}>
        {list.map((x, i) => (
          <Card key={x.name} style={{ height: "100%" }} onClick={() => edit(i, x)}>
            <CardHeader
              header={<Body1 className={styles.mono}>{`\${injection:${x.name}}`}</Body1>}
              description={<Caption1>{x.rules.map((r) => r.host + (r.path || "")).join(", ")}</Caption1>}
            />
            <Caption1>{t("settings.injections.summary", { header: x.header, kind: t(`settings.injections.kinds.${x.kind}`), secret: x.source === "secret" ? x.secret : x.source === "var" ? `\${var:${x.value}}` : t("settings.injections.fixedText") })}</Caption1>
            <div className={styles.row}>
              <Button onClick={(ev) => { ev.stopPropagation(); edit(i, x); }}>
                {t("settings.injections.edit")}
              </Button>
              <Button appearance="subtle" icon={<Delete20Regular />} aria-label={t("settings.injections.delete")} onClick={(ev) => { ev.stopPropagation(); confirm.ask(x.name, () => persist(list.filter((_, j) => j !== i))); }} />
            </div>
          </Card>
        ))}
      </div>

      {confirm.dialog}
      <Dialog surfaceMotion={null} open={open} onOpenChange={(_, d) => setOpen(d.open)}>
        <FormDialogSurface>
          <DialogBody>
            <DialogTitle>{editing?.at === null ? t("settings.injections.new") : t("settings.injections.editTitle")}</DialogTitle>
            <DialogContent style={{ overflowX: "hidden" }}>
              {v && (
                <div className={styles.form}>
                  <Field required label={t("settings.injections.name")} hint={t("settings.injections.nameHint")}>
                    <Input className={styles.mono} value={v.name} disabled={editing?.at !== null} onChange={(_, d) => set({ name: d.value })} />
                  </Field>

                  <Field required label={t("settings.injections.rules")} hint={t("settings.injections.rulesHint")}>
                    <div className={styles.form}>
                      {v.rules.map((r, i) => (
                        <div key={i} className={styles.rule}>
                          <RefCombobox placeholder="api.github.com" value={r.host} options={variables.map((n) => `\${var:${n}}`)} onChange={(host) => setRule(i, { host })} />
                          <Input className={styles.mono} placeholder="/*" value={r.path} onChange={(_, d) => setRule(i, { path: d.value })} />
                          <Dropdown
                            multiselect
                            style={{ minWidth: 0 }}
                            button={{ style: { whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", textAlign: "start" } }}
                            placeholder={t("settings.injections.allMethods")}
                            value={r.methods.join(", ")}
                            selectedOptions={r.methods}
                            onOptionSelect={(_, d) => setRule(i, { methods: METHODS.filter((m) => d.selectedOptions.includes(m)) })}
                          >
                            {METHODS.map((m) => (
                              <Option key={m} value={m}>
                                {m}
                              </Option>
                            ))}
                          </Dropdown>
                          <Button
                            appearance="subtle"
                            icon={<Delete20Regular />}
                            aria-label={t("settings.injections.delete")}
                            disabled={v.rules.length < 2}
                            onClick={() => set({ rules: v.rules.filter((_, j) => j !== i) })}
                          />
                        </div>
                      ))}
                      <div>
                        <Button
                          icon={<Add20Regular />}
                          onClick={() => set({ rules: [...v.rules, { host: "", path: "", methods: [] }] })}
                        >
                          {t("settings.injections.addRule")}
                        </Button>
                      </div>
                    </div>
                  </Field>

                  <div className={styles.row}>
                    <Field className={styles.grow} label={t("settings.injections.header")}>
                      <Input className={styles.mono} value={v.header} onChange={(_, d) => set({ header: d.value })} />
                    </Field>
                    <Field className={styles.grow} label={t("settings.injections.kind")}>
                      <Dropdown
                        value={t(`settings.injections.kinds.${v.kind}`)}
                        selectedOptions={[v.kind]}
                        onOptionSelect={(_, d) => set({ kind: d.optionValue as Injection["kind"] })}
                      >
                        {(["bearer", "basic", "raw"] as const).map((k) => (
                          <Option key={k} value={k}>
                            {t(`settings.injections.kinds.${k}`)}
                          </Option>
                        ))}
                      </Dropdown>
                    </Field>
                  </div>
                  {v.kind === "basic" && (
                    <Field required label={t("settings.injections.username")} hint={t("settings.injections.usernameHint")}>
                      <Combobox
                        freeform
                        value={v.username}
                        selectedOptions={[]}
                        onChange={(e) => set({ username: e.target.value })}
                        onOptionSelect={(_, d) => d.optionValue && set({ username: d.optionValue })}
                      >
                        {variables.map((n) => (
                          <Option key={`var:${n}`} value={`\${var:${n}}`} text={`\${var:${n}}`}>
                            {t("settings.injections.userVar", { name: n })}
                          </Option>
                        ))}
                        {secrets.map((n) => (
                          <Option key={`secret:${n}`} value={`\${secret:${n}}`} text={`\${secret:${n}}`}>
                            {t("settings.injections.userSecret", { name: n })}
                          </Option>
                        ))}
                      </Combobox>
                    </Field>
                  )}

                  <Field required label={t("settings.injections.source")}>
                    <Dropdown value={t(`settings.injections.sources.${v.source}`)} selectedOptions={[v.source]} onOptionSelect={(_, d) => set({ source: (d.optionValue ?? "secret") as Injection["source"] })}>
                      {(["secret", "var", "text"] as const).map((k) => (
                        <Option key={k} value={k}>
                          {t(`settings.injections.sources.${k}`)}
                        </Option>
                      ))}
                    </Dropdown>
                  </Field>

                  {v.source === "secret" && (
                    <Field required label={t("settings.injections.secret")} hint={secrets.length === 0 ? t("settings.injections.noSecrets") : undefined}>
                      <Dropdown value={v.secret} selectedOptions={[v.secret]} onOptionSelect={(_, d) => set({ secret: d.optionValue ?? "" })}>
                        {secrets.map((n) => (
                          <Option key={n} value={n}>
                            {n}
                          </Option>
                        ))}
                      </Dropdown>
                    </Field>
                  )}
                  {v.source === "var" && (
                    <Field required label={t("settings.injections.variable")} hint={variables.length === 0 ? t("settings.injections.noVariables") : undefined}>
                      <Dropdown value={v.value} selectedOptions={[v.value]} onOptionSelect={(_, d) => set({ value: d.optionValue ?? "" })}>
                        {variables.map((n) => (
                          <Option key={n} value={n}>
                            {n}
                          </Option>
                        ))}
                      </Dropdown>
                    </Field>
                  )}
                  {v.source === "text" && (
                    <Field required label={t("settings.injections.text")} hint={t("settings.injections.textHint")}>
                      <Input className={styles.mono} value={v.value} onChange={(_, d) => set({ value: d.value })} />
                    </Field>
                  )}

                  <Field label={t("settings.injections.placeholder")} hint={t("settings.injections.placeholderHint")}>
                    <Input className={styles.mono} placeholder="ccaf-injected" value={v.placeholder} onChange={(_, d) => set({ placeholder: d.value })} />
                  </Field>
                </div>
              )}
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={() => setOpen(false)}>
                {t("common.cancel")}
              </Button>
              <Button appearance="primary" disabled={!ready || saving} onClick={() => void save()}>
                {t("common.save")}
              </Button>
            </DialogActions>
          </DialogBody>
        </FormDialogSurface>
      </Dialog>
    </>
  );
}
