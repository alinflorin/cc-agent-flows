import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Badge, Body1, Button, Caption1, makeStyles, tokens } from "@fluentui/react-components";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";

interface Status {
  state: "missing" | "building" | "ready" | "stale";
  built_at: number | null;
  size: number | null;
  error: string | null;
  log: string[];
}

const useStyles = makeStyles({
  box: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM, maxWidth: "720px" },
  row: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "center" },
  log: {
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeBase200,
    backgroundColor: tokens.colorNeutralBackground3,
    borderRadius: tokens.borderRadiusMedium,
    padding: tokens.spacingHorizontalM,
    maxHeight: "320px",
    overflow: "auto",
    whiteSpace: "pre-wrap",
    margin: 0,
  },
});

async function call<T>(cmd: string): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd);
}

const COLORS = { missing: "danger", building: "informative", ready: "success", stale: "warning" } as const;

/** State of the base image (agentd, Claude Code, ripgrep on Alpine) and its rebuild button. */
export function BaseSettings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();
  const [status, setStatus] = useState<Status | null>(null);

  const refresh = useCallback(() => call<Status>("base_status").then(setStatus, () => {}), []);

  useEffect(() => {
    if (!desktop) return;
    void refresh();
    const id = setInterval(refresh, 1500);
    return () => clearInterval(id);
  }, [desktop, refresh]);

  const rebuild = async () => {
    try {
      await call("base_build");
      await refresh();
    } catch (e) {
      notify(String(e), "error");
    }
  };

  if (!desktop) return <Body1>{t("profiles.desktopOnly")}</Body1>;
  if (!status) return null;
  const building = status.state === "building";

  return (
    <div className={styles.box}>
      <Body1>{t("settings.base.intro")}</Body1>
      <div className={styles.row}>
        <Badge appearance="filled" color={COLORS[status.state]}>
          {t(`settings.base.state.${status.state}`)}
        </Badge>
        {status.built_at && (
          <Caption1>
            {t("settings.base.builtAt", { date: new Date(status.built_at * 1000).toLocaleString() })}
            {status.size ? ` · ${(status.size / 1e6).toFixed(0)} MB` : ""}
          </Caption1>
        )}
      </div>
      {status.error && !building && <Body1 style={{ color: tokens.colorPaletteRedForeground1 }}>{status.error}</Body1>}
      <div>
        <Button appearance="primary" disabled={building} onClick={rebuild}>
          {t("settings.base.rebuild")}
        </Button>
      </div>
      {status.log.length > 0 && <pre className={styles.log}>{status.log.join("\n")}</pre>}
    </div>
  );
}
