import { useCallback, useEffect, useState } from "react";
import { capitalize } from "../../text";
import { useTranslation } from "react-i18next";
import { Body1, Button, Field, Input, Subtitle1, Switch, Textarea, makeStyles, tokens } from "@fluentui/react-components";
import { SettingsIntro } from "../../components/SettingsIntro";
import { inTauri } from "../../prefs";
import { useNotify } from "../../notifications";
import { call } from "../../tauri";
import { useProject } from "../../project";

interface Network {
  allow_hosts: string[];
  max_connections: number;
  leak_detection: boolean;
}

const DEFAULT_HOSTS = ["api.anthropic.com", "statsig.anthropic.com"];

const useStyles = makeStyles({
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalL, maxWidth: "480px" },
  actions: { display: "flex", gap: tokens.spacingHorizontalS },
  section: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM, paddingTop: tokens.spacingVerticalXL, borderTop: `1px solid ${tokens.colorNeutralStroke2}` },
  mono: { fontFamily: tokens.fontFamilyMonospace },
});

const same = (a: string[], b: string[]) => a.length === b.length && a.every((x, i) => x === b[i]);

export function NetworkSettings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();
  const project = useProject();

  // `hosts` is what is in the box (one entry per line), `saved` what is stored.
  const [hosts, setHosts] = useState<string[]>(DEFAULT_HOSTS);
  const [saved, setSaved] = useState<string[]>(DEFAULT_HOSTS);
  const [hook, setHook] = useState<{ port: number; error: string | null } | null>(null);
  const [port, setPort] = useState("");
  // the global limits that are stored with the hosts
  const [limits, setLimits] = useState({ max_connections: 64, leak_detection: true });
  const [savedLimits, setSavedLimits] = useState({ max_connections: 64, leak_detection: true });

  const load = useCallback(async () => {
    try {
      const n = await call<Network>("network_get", { project });
      setHosts(n.allow_hosts);
      setSaved(n.allow_hosts);
      if (project === null) {
        const l = { max_connections: n.max_connections, leak_detection: n.leak_detection };
        setLimits(l);
        setSavedLimits(l);
        const h = await call<{ port: number; error: string | null }>("webhook_get");
        setHook(h);
        setPort(String(h.port));
      }
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify, project]);

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  const save = async () => {
    try {
      const n = await call<Network>("network_set", { project, network: { allow_hosts: hosts, ...limits } });
      setHosts(n.allow_hosts);
      setSaved(n.allow_hosts);
      setSavedLimits({ max_connections: n.max_connections, leak_detection: n.leak_detection });
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
      <SettingsIntro text={t(project === null ? "settings.network.intro" : "settings.network.projectIntro")}>
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
      {project === null && (
        <>
          <Field label={t("settings.network.maxConnections")} hint={t("settings.network.maxConnectionsHint")}>
            <Input
              type="number"
              min={1}
              max={1000}
              disabled={!desktop}
              value={String(limits.max_connections)}
              onChange={(_, d) => setLimits({ ...limits, max_connections: Math.min(1000, Math.max(1, Math.floor(Number(d.value)) || 1)) })}
            />
          </Field>
          <Field hint={t("settings.network.leakHint")}>
            <Switch disabled={!desktop} checked={limits.leak_detection} label={t("settings.network.leak")} onChange={(_, d) => setLimits({ ...limits, leak_detection: d.checked })} />
          </Field>
        </>
      )}
      <div className={styles.actions}>
        <Button appearance="primary" disabled={!desktop || (same(hosts, saved) && JSON.stringify(limits) === JSON.stringify(savedLimits))} onClick={() => void save()}>
          {t("common.save")}
        </Button>
        {project === null && (
          <Button disabled={!desktop || same(hosts, DEFAULT_HOSTS)} onClick={() => setHosts(DEFAULT_HOSTS)}>
            {t("settings.network.defaults")}
          </Button>
        )}
      </div>
      {project === null && (
      <div className={styles.section}>
      <Subtitle1 block>{t("settings.network.webhook")}</Subtitle1>
      <Field label={t("settings.network.webhookPort")} hint={t("settings.network.webhookPortHint")} validationState={hook?.error ? "error" : "none"} validationMessage={hook?.error ? capitalize(hook.error) : undefined}>
        <div className={styles.actions}>
          <Input type="number" min={1024} max={65535} disabled={!desktop} value={port} onChange={(_, d) => setPort(d.value)} />
          <Button disabled={!desktop || !hook || port === String(hook.port)} onClick={() => void saveHook()}>
            {t("common.save")}
          </Button>
        </div>
      </Field>
      </div>
      )}
    </div>
  );
}
