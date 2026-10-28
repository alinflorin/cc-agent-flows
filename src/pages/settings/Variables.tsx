import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Body1, Button, Caption1, Field, Input, makeStyles, tokens } from "@fluentui/react-components";
import { Delete20Regular } from "@fluentui/react-icons";
import { SettingsIntro } from "../../components/SettingsIntro";
import { ResetDemoButton } from "../../components/ResetDemoButton";
import { inTauri } from "../../prefs";
import { useNotify } from "../../notifications";
import { call } from "../../tauri";

const NAME = /^[A-Za-z_][A-Za-z0-9_]{0,63}$/;

interface Variable {
  name: string;
  value: string;
}

const useStyles = makeStyles({
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalL, maxWidth: "760px" },
  row: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "center" },
  grow: { flex: 1 },
  mono: { fontFamily: tokens.fontFamilyMonospace },
  name: { width: "260px", fontFamily: tokens.fontFamilyMonospace },
});

export function VariablesSettings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();

  const [vars, setVars] = useState<Variable[]>([]);
  const [name, setName] = useState("");
  const [value, setValue] = useState("");
  // values being edited, by name
  const [edits, setEdits] = useState<Record<string, string>>({});

  const load = useCallback(async () => {
    try {
      setVars(await call<Variable[]>("variables_list"));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify]);

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  const save = async (n: string, v: string) => {
    try {
      await call("variables_set", { name: n, value: v });
      notify(t("settings.variables.saved"), "success");
      await load();
      return true;
    } catch (e) {
      notify(String(e), "error");
      return false;
    }
  };

  const remove = async (n: string) => {
    try {
      await call("variables_delete", { name: n });
      await load();
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const nameOk = NAME.test(name);

  return (
    <div className={styles.form}>
      <SettingsIntro text={t("settings.variables.intro")}>
        {!desktop && <Body1 block>{t("settings.variables.desktopOnly")}</Body1>}
        <ResetDemoButton command="variables_reset_demo" what="variables" onDone={() => void load()} />
      </SettingsIntro>

      <Field
        label={t("settings.variables.new")}
        hint={t("settings.variables.newHint")}
        validationState={name && !nameOk ? "error" : "none"}
        validationMessage={name && !nameOk ? t("settings.variables.badName") : undefined}
      >
        <div className={styles.row}>
          <Input className={styles.name} placeholder="ATLASSIAN_HOST" disabled={!desktop} value={name} onChange={(_, d) => setName(d.value)} />
          <Input className={styles.grow} disabled={!desktop} placeholder={t("settings.variables.value")} value={value} onChange={(_, d) => setValue(d.value)} />
          <Button
            appearance="primary"
            disabled={!desktop || !nameOk || !value}
            onClick={() => void save(name, value).then((ok) => ok && (setName(""), setValue("")))}
          >
            {t("common.save")}
          </Button>
        </div>
      </Field>

      {desktop && vars.length === 0 && <Caption1>{t("settings.variables.empty")}</Caption1>}
      {vars.map((x) => {
        const edited = edits[x.name];
        const dirty = edited !== undefined && edited !== x.value;
        return (
          <div key={x.name} className={styles.row}>
            <Body1 className={styles.name}>{`\${var:${x.name}}`}</Body1>
            <Input
              className={styles.grow}
              value={edited ?? x.value}
              onChange={(_, d) => setEdits({ ...edits, [x.name]: d.value })}
            />
            <Button
              appearance="primary"
              disabled={!dirty || !edited}
              onClick={() =>
                void save(x.name, edited ?? "").then((ok) => {
                  if (ok) {
                    const { [x.name]: _gone, ...rest } = edits;
                    setEdits(rest);
                  }
                })
              }
            >
              {t("common.save")}
            </Button>
            <Button appearance="subtle" icon={<Delete20Regular />} aria-label={t("settings.variables.delete")} onClick={() => void remove(x.name)} />
          </div>
        );
      })}
    </div>
  );
}
