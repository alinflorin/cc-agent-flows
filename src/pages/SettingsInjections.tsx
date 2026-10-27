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
  DialogSurface,
  DialogTitle,
  Dropdown,
  Field,
  Input,
  Option,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { Add16Regular, Delete16Regular } from "@fluentui/react-icons";
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
  grow: { flex: 1 },
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
  const [methods, setMethods] = useState<string[]>([]);

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
    setMethods(value.rules.map((r) => r.methods.join(", ")));
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
    const value = {
      ...editing.value,
      rules: editing.value.rules.map((r, i) => ({
        ...r,
        methods: (methods[i] ?? "").split(/[\s,]+/).filter(Boolean),
      })),
    };
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
      <Body1 block>{t("settings.injections.intro")}</Body1>
      {!desktop && <Body1 block>{t("settings.injections.desktopOnly")}</Body1>}
      <div style={{ marginTop: 16 }}>
        <Button appearance="primary" icon={<Add16Regular />} disabled={!desktop} onClick={() => edit(null, EMPTY)}>
          {t("settings.injections.new")}
        </Button>
      </div>
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
              <Button size="small" onClick={() => edit(i, x)}>
                {t("settings.injections.edit")}
              </Button>
              <Button size="small" appearance="subtle" icon={<Delete16Regular />} aria-label={t("settings.injections.delete")} onClick={() => void persist(list.filter((_, j) => j !== i))} />
            </div>
          </Card>
        ))}
      </div>

      <Dialog open={open} onOpenChange={(_, d) => setOpen(d.open)}>
        <DialogSurface>
          <DialogBody>
            <DialogTitle>{editing?.at === null ? t("settings.injections.new") : t("settings.injections.editTitle")}</DialogTitle>
            <DialogContent>
              {v && (
                <div className={styles.form}>
                  <Field label={t("settings.injections.name")} hint={t("settings.injections.nameHint")}>
                    <Input className={styles.mono} value={v.name} disabled={editing?.at !== null} onChange={(_, d) => set({ name: d.value })} />
                  </Field>

                  <Field label={t("settings.injections.rules")} hint={t("settings.injections.rulesHint")}>
                    <div className={styles.form}>
                      {v.rules.map((r, i) => (
                        <div key={i} className={styles.row}>
                          <Input className={`${styles.grow} ${styles.mono}`} placeholder="api.github.com" value={r.host} onChange={(_, d) => setRule(i, { host: d.value })} />
                          <Input className={`${styles.grow} ${styles.mono}`} placeholder="/*" value={r.path} onChange={(_, d) => setRule(i, { path: d.value })} />
                          <Input
                            style={{ width: 120 }}
                            className={styles.mono}
                            placeholder={t("settings.injections.allMethods")}
                            value={methods[i] ?? ""}
                            onChange={(_, d) => setMethods(methods.map((m, j) => (j === i ? d.value : m)))}
                          />
                          <Button
                            appearance="subtle"
                            icon={<Delete16Regular />}
                            aria-label={t("settings.injections.delete")}
                            disabled={v.rules.length < 2}
                            onClick={() => {
                              set({ rules: v.rules.filter((_, j) => j !== i) });
                              setMethods(methods.filter((_, j) => j !== i));
                            }}
                          />
                        </div>
                      ))}
                      <div>
                        <Button
                          size="small"
                          icon={<Add16Regular />}
                          onClick={() => {
                            set({ rules: [...v.rules, { host: "", path: "", methods: [] }] });
                            setMethods([...methods, ""]);
                          }}
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
        </DialogSurface>
      </Dialog>
    </>
  );
}
