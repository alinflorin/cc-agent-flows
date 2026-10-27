import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Body1, Button, Caption1, Field, Input, makeStyles, tokens } from "@fluentui/react-components";
import { Delete16Regular } from "@fluentui/react-icons";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";

const NAME = /^[A-Za-z_][A-Za-z0-9_]{0,63}$/;

const useStyles = makeStyles({
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalL, maxWidth: "560px" },
  row: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "center" },
  grow: { flex: 1 },
  mono: { fontFamily: tokens.fontFamilyMonospace },
});

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export function SecretsSettings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();

  const [names, setNames] = useState<string[]>([]);
  const [name, setName] = useState("");
  const [value, setValue] = useState("");
  // the secret whose value is being replaced, with the new value
  const [replacing, setReplacing] = useState<{ name: string; value: string } | null>(null);

  const load = useCallback(async () => {
    try {
      setNames(await call<string[]>("secrets_list"));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify]);

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  const save = async (n: string, v: string) => {
    try {
      await call("secrets_set", { name: n, value: v });
      notify(t("settings.secrets.saved"), "success");
      await load();
      return true;
    } catch (e) {
      notify(String(e), "error");
      return false;
    }
  };

  const remove = async (n: string) => {
    try {
      await call("secrets_delete", { name: n });
      await load();
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const nameOk = NAME.test(name);

  return (
    <div className={styles.form}>
      <Body1 block>{t("settings.secrets.intro")}</Body1>
      {!desktop && <Body1 block>{t("settings.secrets.desktopOnly")}</Body1>}

      <Field
        label={t("settings.secrets.new")}
        hint={t("settings.secrets.newHint")}
        validationState={name && !nameOk ? "error" : "none"}
        validationMessage={name && !nameOk ? t("settings.secrets.badName") : undefined}
      >
        <div className={styles.row}>
          <Input className={styles.mono} placeholder="GITHUB_TOKEN" disabled={!desktop} value={name} onChange={(_, d) => setName(d.value)} />
          <Input
            className={styles.grow}
            type="password"
            autoComplete="off"
            disabled={!desktop}
            placeholder={t("settings.secrets.value")}
            value={value}
            onChange={(_, d) => setValue(d.value)}
          />
          <Button
            appearance="primary"
            disabled={!desktop || !nameOk || !value}
            onClick={() => void save(name, value).then((ok) => ok && (setName(""), setValue("")))}
          >
            {t("common.save")}
          </Button>
        </div>
      </Field>

      {desktop && names.length === 0 && <Caption1>{t("settings.secrets.empty")}</Caption1>}
      {names.map((n) => (
        <div key={n} className={styles.row}>
          <Body1 className={`${styles.mono} ${styles.grow}`}>{`\${secret:${n}}`}</Body1>
          {replacing?.name === n ? (
            <>
              <Input
                type="password"
                autoComplete="off"
                autoFocus
                placeholder={t("settings.secrets.value")}
                value={replacing.value}
                onChange={(_, d) => setReplacing({ name: n, value: d.value })}
              />
              <Button
                appearance="primary"
                disabled={!replacing.value}
                onClick={() => void save(n, replacing.value).then((ok) => ok && setReplacing(null))}
              >
                {t("common.save")}
              </Button>
              <Button onClick={() => setReplacing(null)}>{t("common.cancel")}</Button>
            </>
          ) : (
            <Button onClick={() => setReplacing({ name: n, value: "" })}>{t("settings.secrets.replace")}</Button>
          )}
          <Button appearance="subtle" icon={<Delete16Regular />} aria-label={t("settings.secrets.delete")} onClick={() => void remove(n)} />
        </div>
      ))}
    </div>
  );
}
