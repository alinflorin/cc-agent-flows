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
  Dropdown,
  Field,
  Input,
  Option,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { Add20Regular, Delete20Regular } from "@fluentui/react-icons";
import { SettingsIntro } from "../components/SettingsIntro";
import { FormDialogSurface } from "../components/FormDialogSurface";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";

export interface InjectionRule {
  host: string;
  path: string;
  methods: string[];
}

export interface Injection {
  name: string;
  rules: InjectionRule[];
  header: string;
  kind: "bearer" | "basic" | "raw";
  username: string;
  secret: string;
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
  placeholder: "",
};

const useStyles = makeStyles({
  list: { display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))", gap: tokens.spacingHorizontalM, marginTop: tokens.spacingVerticalL },
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM },
  row: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "flex-start" },
  grow: { flex: 1, minWidth: 0 },
  rule: { display: "grid", gridTemplateColumns: "minmax(0, 1fr) minmax(0, 1fr) 108px auto", gap: tokens.spacingHorizontalS, alignItems: "start" },
  mono: { fontFamily: tokens.fontFamilyMonospace },
});

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export function InjectionsSettings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();

  const [list, setList] = useState<Injection[]>([]);
  const [secrets, setSecrets] = useState<string[]>([]);
  // the injection being edited and its position in the list (null = new); kept after closing so the dialog does not flicker while it fades out
  const [editing, setEditing] = useState<{ at: number | null; value: Injection } | null>(null);
  const [open, setOpen] = useState(false);

  const load = useCallback(async () => {
    try {
      setList(await call<Injection[]>("injections_get"));
      setSecrets(await call<string[]>("secrets_list"));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify]);

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  const edit = (at: number | null, value: Injection) => {
    setEditing({ at, value });
    setOpen(true);
  };

  const persist = async (next: Injection[]) => {
    try {
      setList(await call<Injection[]>("injections_save", { injections: next }));
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
    if (await persist(next)) {
      setOpen(false);
      notify(t("settings.injections.saved"), "success");
    }
  };

  const set = (patch: Partial<Injection>) => editing && setEditing({ ...editing, value: { ...editing.value, ...patch } });
  const setRule = (i: number, patch: Partial<InjectionRule>) =>
    set({ rules: editing!.value.rules.map((r, j) => (j === i ? { ...r, ...patch } : r)) });

  const v = editing?.value;
  const ready = !!v && !!v.name && !!v.secret && !!v.header && v.rules.length > 0 && v.rules.every((r) => r.host.trim()) && (v.kind !== "basic" || !!v.username);

  return (
    <>
      <SettingsIntro text={t("settings.injections.intro")}>
        {!desktop && <Body1 block>{t("settings.injections.desktopOnly")}</Body1>}
        <Button appearance="primary" icon={<Add20Regular />} disabled={!desktop} onClick={() => edit(null, EMPTY)}>
          {t("settings.injections.new")}
        </Button>
      </SettingsIntro>
      {desktop && list.length === 0 && <Caption1 block style={{ marginTop: 16 }}>{t("settings.injections.empty")}</Caption1>}
      <div className={styles.list}>
        {list.map((x, i) => (
          <Card key={x.name}>
            <CardHeader
              header={<Body1 className={styles.mono}>{`\${injection:${x.name}}`}</Body1>}
              description={<Caption1>{x.rules.map((r) => r.host + (r.path || "")).join(", ")}</Caption1>}
            />
            <Caption1>{t("settings.injections.summary", { header: x.header, kind: t(`settings.injections.kinds.${x.kind}`), secret: x.secret })}</Caption1>
            <div className={styles.row}>
              <Button onClick={() => edit(i, x)}>
                {t("settings.injections.edit")}
              </Button>
              <Button appearance="subtle" icon={<Delete20Regular />} aria-label={t("settings.injections.delete")} onClick={() => void persist(list.filter((_, j) => j !== i))} />
            </div>
          </Card>
        ))}
      </div>

      <Dialog surfaceMotion={null} open={open} onOpenChange={(_, d) => setOpen(d.open)}>
        <FormDialogSurface>
          <DialogBody>
            <DialogTitle>{editing?.at === null ? t("settings.injections.new") : t("settings.injections.editTitle")}</DialogTitle>
            <DialogContent style={{ overflowX: "hidden" }}>
              {v && (
                <div className={styles.form}>
                  <Field label={t("settings.injections.name")} hint={t("settings.injections.nameHint")}>
                    <Input className={styles.mono} value={v.name} disabled={editing?.at !== null} onChange={(_, d) => set({ name: d.value })} />
                  </Field>

                  <Field label={t("settings.injections.rules")} hint={t("settings.injections.rulesHint")}>
                    <div className={styles.form}>
                      {v.rules.map((r, i) => (
                        <div key={i} className={styles.rule}>
                          <Input className={styles.mono} placeholder="api.github.com" value={r.host} onChange={(_, d) => setRule(i, { host: d.value })} />
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
                    <Field label={t("settings.injections.username")}>
                      <Input value={v.username} onChange={(_, d) => set({ username: d.value })} />
                    </Field>
                  )}

                  <Field label={t("settings.injections.secret")} hint={secrets.length === 0 ? t("settings.injections.noSecrets") : undefined}>
                    <Dropdown value={v.secret} selectedOptions={[v.secret]} onOptionSelect={(_, d) => set({ secret: d.optionValue ?? "" })}>
                      {secrets.map((n) => (
                        <Option key={n} value={n}>
                          {n}
                        </Option>
                      ))}
                    </Dropdown>
                  </Field>

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
