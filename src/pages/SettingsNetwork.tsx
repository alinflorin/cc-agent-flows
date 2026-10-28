import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Body1, Button, Field, Input, Textarea, makeStyles, tokens } from "@fluentui/react-components";
import { SettingsIntro } from "../components/SettingsIntro";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";

interface Network {
  allow_hosts: string[];
}

const DEFAULT_HOSTS = ["api.anthropic.com", "statsig.anthropic.com"];

const useStyles = makeStyles({
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalL, maxWidth: "480px" },
  actions: { display: "flex", gap: tokens.spacingHorizontalS },
  mono: { fontFamily: tokens.fontFamilyMonospace },
});

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

const same = (a: string[], b: string[]) => a.length === b.length && a.every((x, i) => x === b[i]);

export function NetworkSettings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();

  // `hosts` is what is in the box (one entry per line), `saved` what is stored.
  const [hosts, setHosts] = useState<string[]>(DEFAULT_HOSTS);
  const [saved, setSaved] = useState<string[]>(DEFAULT_HOSTS);
  const [hook, setHook] = useState<{ port: number; error: string | null } | null>(null);
  const [port, setPort] = useState("");

  const load = useCallback(async () => {
    try {
      const n = await call<Network>("network_get");
      setHosts(n.allow_hosts);
      setSaved(n.allow_hosts);
      const h = await call<{ port: number; error: string | null }>("webhook_get");
      setHook(h);
      setPort(String(h.port));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify]);

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  const save = async () => {
    try {
      const n = await call<Network>("network_set", { network: { allow_hosts: hosts } });
      setHosts(n.allow_hosts);
      setSaved(n.allow_hosts);
      notify(t("settings.saved"), "success");
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const saveHook = async () => {
    try {
      const h = await call<{ port: number; error: string | null }>("webhook_set", { port: Number(port) });
      setHook(h);
      notify(t("settings.saved"), "success");
      // the listener binds a moment later: ask again for a busy port
      setTimeout(() => void call<{ port: number; error: string | null }>("webhook_get").then(setHook), 600);
    } catch (e) {
      notify(String(e), "error");
    }
  };

  return (
    <div className={styles.form}>
      <SettingsIntro text={t("settings.network.intro")}>
        {!desktop && <Body1 block>{t("settings.llm.tokenDesktopOnly")}</Body1>}
      </SettingsIntro>
      <Field label={t("settings.network.allowHosts")} hint={t("settings.network.allowHostsHint")}>
        <Textarea
          rows={8}
          className={styles.mono}
          disabled={!desktop}
          value={hosts.join("\n")}
          onChange={(_, d) => setHosts(d.value === "" ? [] : d.value.split("\n"))}
        />
      </Field>
      <div className={styles.actions}>
        <Button appearance="primary" disabled={!desktop || same(hosts, saved)} onClick={() => void save()}>
          {t("common.save")}
        </Button>
        <Button disabled={!desktop || same(hosts, DEFAULT_HOSTS)} onClick={() => setHosts(DEFAULT_HOSTS)}>
          {t("settings.network.defaults")}
        </Button>
      </div>
      <Field label={t("settings.network.webhookPort")} hint={t("settings.network.webhookPortHint")} validationState={hook?.error ? "error" : "none"} validationMessage={hook?.error ?? undefined}>
        <div className={styles.actions}>
          <Input type="number" min={1024} max={65535} disabled={!desktop} value={port} onChange={(_, d) => setPort(d.value)} />
          <Button disabled={!desktop || !hook || port === String(hook.port)} onClick={() => void saveHook()}>
            {t("common.save")}
          </Button>
        </div>
      </Field>
    </div>
  );
}
