import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Badge, Button, Caption1, Dropdown, Field, Input, Option, Subtitle2, Switch, makeStyles, tokens } from "@fluentui/react-components";
import { DEFAULT_MODEL, DEFAULT_THINKING, MODELS, THINKING_LEVELS } from "../../llm";
import { getPref, inTauri, setPref } from "../../prefs";
import { useNotify } from "../../notifications";
import { call } from "../../tauri";

const useStyles = makeStyles({
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalL, maxWidth: "480px" },
  row: { display: "flex", alignItems: "center", gap: tokens.spacingHorizontalS },
  grow: { flex: 1 },
  account: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalS, padding: tokens.spacingHorizontalM, border: `1px solid ${tokens.colorNeutralStroke2}`, borderRadius: tokens.borderRadiusMedium },
});

interface Account {
  id: string;
  name: string;
  enabled: boolean;
  has_token: boolean;
  rest_until: number | null;
  rest_why: "limited" | "rejected" | null;
}

export function LlmSettings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();

  const [model, setModel] = useState(getPref<string>("llmModel") ?? DEFAULT_MODEL);
  const [thinking, setThinking] = useState(getPref<string>("llmThinking") ?? DEFAULT_THINKING);
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [mode, setMode] = useState<"round_robin" | "drain">("round_robin");
  const [name, setName] = useState("");
  const [token, setToken] = useState("");
  const [renew, setRenew] = useState<{ id: string; token: string } | null>(null);

  const load = useCallback(async () => {
    try {
      setAccounts(await call<Account[]>("accounts_list"));
      setMode(await call<"round_robin" | "drain">("accounts_mode"));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify]);

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  const run = async (cmd: string, args: Record<string, unknown>, ok?: string) => {
    try {
      await call(cmd, args);
      if (ok) notify(ok, "success");
      await load();
      return true;
    } catch (e) {
      notify(t("settings.llm.tokenError", { error: String(e) }), "error");
      return false;
    }
  };

  const state = (a: Account) => {
    if (!a.has_token) return { key: "empty", color: "warning" as const };
    if (!a.enabled) return { key: "disabled", color: "informative" as const };
    if (a.rest_until && a.rest_why) return { key: a.rest_why, color: "danger" as const };
    return { key: "ready", color: "success" as const };
  };

  return (
    <div className={styles.form}>
      <Subtitle2>{t("settings.llm.accounts")}</Subtitle2>
      <Caption1>{desktop ? t("settings.llm.accountsHint") : t("settings.llm.tokenDesktopOnly")}</Caption1>
      {accounts.map((a) => {
        const st = state(a);
        return (
          <div key={a.id} className={styles.account}>
            <div className={styles.row}>
              <Input
                className={styles.grow}
                defaultValue={a.name}
                aria-label={t("settings.llm.accountName")}
                onBlur={(e) => e.target.value.trim() && e.target.value !== a.name && void run("accounts_update", { id: a.id, name: e.target.value, enabled: a.enabled })}
              />
              <Badge appearance="tint" color={st.color}>
                {t(`settings.llm.state.${st.key}`)}
              </Badge>
              <Switch checked={a.enabled} aria-label={t("settings.llm.accountOn")} onChange={(_, d) => void run("accounts_update", { id: a.id, name: a.name, enabled: d.checked })} />
            </div>
            {a.rest_until && a.rest_why && (
              <div className={styles.row}>
                <Caption1 className={styles.grow}>{t("settings.llm.restUntil", { time: new Date(a.rest_until * 1000).toLocaleTimeString() })}</Caption1>
                <Button size="small" onClick={() => void run("accounts_wake", { id: a.id })}>
                  {t("settings.llm.wake")}
                </Button>
              </div>
            )}
            {renew?.id === a.id ? (
              <div className={styles.row}>
                <Input className={styles.grow} type="password" autoComplete="off" autoFocus value={renew.token} placeholder="sk-ant-oat01-…" onChange={(_, d) => setRenew({ id: a.id, token: d.value })} />
                <Button appearance="primary" disabled={!renew.token.trim()} onClick={async () => (await run("accounts_set_token", { id: a.id, token: renew.token }, t("settings.llm.tokenSaved"))) && setRenew(null)}>
                  {t("common.save")}
                </Button>
                <Button onClick={() => setRenew(null)}>{t("common.cancel")}</Button>
              </div>
            ) : (
              <div className={styles.row}>
                <Button size="small" onClick={() => setRenew({ id: a.id, token: "" })}>
                  {a.has_token ? t("settings.llm.replaceToken") : t("settings.llm.setToken")}
                </Button>
                {(a.id !== "default" || a.has_token) && (
                  <Button size="small" onClick={() => void run("accounts_remove", { id: a.id })}>
                    {t("settings.llm.tokenRemove")}
                  </Button>
                )}
              </div>
            )}
          </div>
        );
      })}
      <Field label={t("settings.llm.mode")} hint={t(`settings.llm.modeHint.${mode}`)}>
        <Dropdown
          disabled={!desktop}
          value={t(`settings.llm.modes.${mode}`)}
          selectedOptions={[mode]}
          onOptionSelect={(_, d) => {
            const next = d.optionValue as "round_robin" | "drain";
            setMode(next);
            void run("accounts_set_mode", { mode: next });
          }}
        >
          {(["round_robin", "drain"] as const).map((m) => (
            <Option key={m} value={m}>
              {t(`settings.llm.modes.${m}`)}
            </Option>
          ))}
        </Dropdown>
      </Field>
      <Field label={t("settings.llm.addAccount")} hint={t("settings.llm.tokenHint")}>
        <div className={styles.row}>
          <Input value={name} disabled={!desktop} placeholder={t("settings.llm.accountName")} onChange={(_, d) => setName(d.value)} />
          <Input className={styles.grow} type="password" autoComplete="off" disabled={!desktop} value={token} placeholder="sk-ant-oat01-…" onChange={(_, d) => setToken(d.value)} />
          <Button
            appearance="primary"
            disabled={!desktop || !token.trim() || !name.trim()}
            onClick={async () => {
              if (await run("accounts_add", { name, token }, t("settings.llm.tokenSaved"))) {
                setName("");
                setToken("");
              }
            }}
          >
            {t("settings.llm.add")}
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
