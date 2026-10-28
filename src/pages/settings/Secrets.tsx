import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Body1, Button, Caption1, Field, Input, makeStyles, tokens } from "@fluentui/react-components";
import { Delete20Regular } from "@fluentui/react-icons";
import { SettingsIntro } from "../../components/SettingsIntro";
import { ResetDemoButton } from "../../components/ResetDemoButton";
import { inTauri } from "../../prefs";
import { useNotify } from "../../notifications";
import { call } from "../../tauri";
import { useProject } from "../../project";

const NAME = /^[A-Za-z_][A-Za-z0-9_]{0,63}$/;

const useStyles = makeStyles({
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalL, maxWidth: "760px" },
  row: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "center" },
  grow: { flex: 1 },
  name: { flex: "1 1 0", minWidth: 0, overflowWrap: "anywhere", fontFamily: tokens.fontFamilyMonospace },
  mono: { fontFamily: tokens.fontFamilyMonospace },
});

export function SecretsSettings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();
  const project = useProject();

  const [names, setNames] = useState<string[]>([]);
  const [name, setName] = useState("");
  const [value, setValue] = useState("");
  // the secret whose value is being replaced, with the new value
  const [replacing, setReplacing] = useState<{ name: string; value: string } | null>(null);

  const load = useCallback(async () => {
    try {
      setNames(await call<string[]>("secrets_list", { project }));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify, project]);

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  const save = async (n: string, v: string) => {
    try {
      await call("secrets_set", { project, name: n, value: v });
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
      await call("secrets_delete", { project, name: n });
      await load();
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const nameOk = NAME.test(name);

  return (
    <div className={styles.form}>
      <SettingsIntro text={t("settings.secrets.intro")}>
        {!desktop && <Body1 block>{t("settings.secrets.desktopOnly")}</Body1>}
        <ResetDemoButton command="secrets_reset_demo" what="secrets" onDone={() => void load()} />
      </SettingsIntro>

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
          <Body1 className={styles.name}>{`\${secret:${n}}`}</Body1>
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
          <Button appearance="subtle" icon={<Delete20Regular />} aria-label={t("settings.secrets.delete")} onClick={() => void remove(n)} />
        </div>
      ))}
    </div>
  );
}
