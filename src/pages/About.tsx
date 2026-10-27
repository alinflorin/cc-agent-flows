import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Badge,
  Body1,
  Button,
  Caption1,
  Spinner,
  Subtitle1,
  Title2,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { ArrowClockwise16Regular } from "@fluentui/react-icons";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { inTauri } from "../prefs";

const useStyles = makeStyles({
  logo: { width: "64px", height: "64px", marginBottom: tokens.spacingVerticalM },
  section: { marginTop: tokens.spacingVerticalXL },
  row: { display: "flex", alignItems: "center", gap: tokens.spacingHorizontalS, flexWrap: "wrap" },
});

type BrokerStatus = { state: "starting" | "running" | "failed"; port: number | null; error: string | null };

const COLORS = { starting: "warning", running: "success", failed: "danger" } as const;

function NatsStatus() {
  const { t } = useTranslation();
  const styles = useStyles();
  const [status, setStatus] = useState<BrokerStatus | null>(null);
  const [version, setVersion] = useState("");
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => invoke<BrokerStatus>("broker_status").then(setStatus, () => {}), []);

  useEffect(() => {
    void refresh();
    invoke<string>("nats_version").then(setVersion, () => {});
  }, [refresh]);

  // The broker starts in the background, so poll until it settles.
  useEffect(() => {
    if (status?.state !== "starting" || busy) return;
    const id = setTimeout(() => void refresh(), 500);
    return () => clearTimeout(id);
  }, [status, busy, refresh]);

  const restart = async () => {
    setBusy(true);
    setStatus({ state: "starting", port: null, error: null });
    try {
      setStatus(await invoke<BrokerStatus>("broker_restart"));
    } catch (e) {
      setStatus({ state: "failed", port: null, error: String(e) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={styles.section}>
      <Subtitle1 block>{t("about.nats.title")}</Subtitle1>
      <div className={styles.row}>
        {status ? (
          <Badge appearance="filled" color={COLORS[status.state]}>
            {t(`about.nats.${status.state}`)}
          </Badge>
        ) : (
          <Spinner size="tiny" />
        )}
        {version && <Caption1>{version}</Caption1>}
        {status?.port && <Caption1>{t("about.nats.port", { port: status.port })}</Caption1>}
        <Button size="small" icon={<ArrowClockwise16Regular />} disabled={busy || !status} onClick={() => void restart()}>
          {t("about.nats.restart")}
        </Button>
      </div>
      {status?.error && <Caption1 block>{status.error}</Caption1>}
    </div>
  );
}

export function About() {
  const { t } = useTranslation();
  const styles = useStyles();
  const [version, setVersion] = useState("");
  const desktop = inTauri();
  useEffect(() => {
    if (desktop) getVersion().then(setVersion).catch(() => {});
  }, [desktop]);
  return (
    <>
      <img src="/logo.svg" alt="" className={styles.logo} />
      <Title2 as="h1" block>
        {t("title")}
      </Title2>
      <Body1 block>{t("about.version", { version: version || "—" })}</Body1>
      <Caption1 block>{t("about.tagline")}</Caption1>
      {desktop && <NatsStatus />}
    </>
  );
}
