import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Dropdown, Field, Input, Option, makeStyles, tokens } from "@fluentui/react-components";
import { DEFAULT_MODEL, DEFAULT_THINKING, MODELS, THINKING_LEVELS } from "../llm";
import { getPref, inTauri, setPref } from "../prefs";
import { useNotify } from "../notifications";

const useStyles = makeStyles({
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalL, maxWidth: "480px" },
  row: { display: "flex", gap: tokens.spacingHorizontalS },
  grow: { flex: 1 },
});

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export function LlmSettings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();

  const [model, setModel] = useState(getPref<string>("llmModel") ?? DEFAULT_MODEL);
  const [thinking, setThinking] = useState(getPref<string>("llmThinking") ?? DEFAULT_THINKING);
  const [hasToken, setHasToken] = useState(false);
  const [token, setToken] = useState("");

  useEffect(() => {
    if (desktop) call<boolean>("claude_token_status").then(setHasToken, () => {});
  }, [desktop]);

  const saveToken = async () => {
    try {
      await call("claude_token_set", { token });
      setToken("");
      setHasToken(true);
      notify(t("settings.llm.tokenSaved"), "success");
    } catch (e) {
      notify(t("settings.llm.tokenError", { error: String(e) }), "error");
    }
  };

  const clearToken = async () => {
    try {
      await call("claude_token_clear");
      setHasToken(false);
    } catch (e) {
      notify(t("settings.llm.tokenError", { error: String(e) }), "error");
    }
  };

  return (
    <div className={styles.form}>
      <Field
        label={t("settings.llm.token")}
        hint={desktop ? t("settings.llm.tokenHint") : t("settings.llm.tokenDesktopOnly")}
        validationState={hasToken ? "success" : "none"}
        validationMessage={hasToken ? t("settings.llm.tokenSet") : undefined}
      >
        <div className={styles.row}>
          <Input
            className={styles.grow}
            type="password"
            autoComplete="off"
            disabled={!desktop}
            value={token}
            placeholder={hasToken ? "••••••••" : "sk-ant-oat01-…"}
            onChange={(_, d) => setToken(d.value)}
          />
          <Button appearance="primary" disabled={!desktop || !token.trim()} onClick={() => void saveToken()}>
            {t("common.save")}
          </Button>
          <Button disabled={!hasToken} onClick={() => void clearToken()}>
            {t("settings.llm.tokenRemove")}
          </Button>
        </div>
      </Field>

      <Field label={t("settings.llm.model")}>
        <Dropdown
          value={MODELS.find((m) => m.id === model)?.label ?? model}
          selectedOptions={[model]}
          onOptionSelect={(_, d) => {
            setModel(d.optionValue!);
            setPref("llmModel", d.optionValue);
          }}
        >
          {MODELS.map((m) => (
            <Option key={m.id} value={m.id}>
              {m.label}
            </Option>
          ))}
        </Dropdown>
      </Field>

      <Field label={t("settings.llm.thinking")}>
        <Dropdown
          value={t(`settings.llm.levels.${thinking}`)}
          selectedOptions={[thinking]}
          onOptionSelect={(_, d) => {
            setThinking(d.optionValue!);
            setPref("llmThinking", d.optionValue);
          }}
        >
          {THINKING_LEVELS.map((l) => (
            <Option key={l} value={l}>
              {t(`settings.llm.levels.${l}`)}
            </Option>
          ))}
        </Dropdown>
      </Field>
    </div>
  );
}
