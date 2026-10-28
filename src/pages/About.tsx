import { useCallback, useEffect, useState } from "react";
import { PageHeader } from "../components/PageHeader";
import { useTranslation } from "react-i18next";
import { Badge, Body1, Button, Caption1, Spinner, Subtitle2, makeStyles, tokens } from "@fluentui/react-components";
import { ArrowClockwise20Regular, CheckmarkCircle20Filled, DismissCircle20Filled, Info20Filled, Warning20Filled } from "@fluentui/react-icons";
import { getVersion } from "@tauri-apps/api/app";
import { inTauri } from "../prefs";
import { call } from "../tauri";

interface Check {
  id: string;
  level: "ok" | "warn" | "error" | "info";
  detail: string;
}

const useStyles = makeStyles({
  logo: { width: "64px", height: "64px", marginBottom: tokens.spacingVerticalL },
  head: { display: "flex", alignItems: "center", gap: tokens.spacingHorizontalM, marginTop: tokens.spacingVerticalXL, marginBottom: tokens.spacingVerticalS },
  list: { display: "flex", flexDirection: "column", maxWidth: "720px", borderTop: `1px solid ${tokens.colorNeutralStroke2}` },
  row: { display: "grid", gridTemplateColumns: "20px 200px minmax(0, 1fr)", columnGap: tokens.spacingHorizontalM, alignItems: "center", padding: `${tokens.spacingVerticalS} 0`, borderBottom: `1px solid ${tokens.colorNeutralStroke2}` },
  ok: { color: tokens.colorPaletteGreenForeground1 },
  warn: { color: tokens.colorPaletteMarigoldForeground1 },
  error: { color: tokens.colorPaletteRedForeground1 },
  info: { color: tokens.colorNeutralForeground3 },
  muted: { color: tokens.colorNeutralForeground3, overflowWrap: "anywhere" },
});

const icons = { ok: CheckmarkCircle20Filled, warn: Warning20Filled, error: DismissCircle20Filled, info: Info20Filled };

export function About() {
  const { t } = useTranslation();
  const styles = useStyles();
  const [version, setVersion] = useState("");
  const [checks, setChecks] = useState<Check[] | null>(null);
  const desktop = inTauri();

  const run = useCallback(async () => {
    setChecks(null);
    try {
      setChecks(await call<Check[]>("system_status"));
    } catch (e) {
      setChecks([{ id: "qemu", level: "error", detail: String(e) }]);
    }
  }, []);

  useEffect(() => {
    if (!desktop) return;
    getVersion().then(setVersion).catch(() => {});
    void run();
  }, [desktop, run]);

  const ready = checks?.every((c) => c.level !== "error" && c.level !== "warn");
  return (
    <>
      <img src="/logo.svg" alt="" className={styles.logo} />
      <PageHeader title={t("title")} description={t("about.tagline")} />
      <Body1 block>
        <Badge appearance="tint" color="brand" size="large">
          {t("about.version", { version: version || "—" })}
        </Badge>
      </Body1>

      {desktop && (
        <>
          <div className={styles.head}>
            <Subtitle2>{t("about.readiness")}</Subtitle2>
            {checks && (
              <Badge appearance="filled" color={ready ? "success" : "warning"}>
                {ready ? t("about.allOk") : t("about.notReady")}
              </Badge>
            )}
            <Button size="small" appearance="subtle" icon={checks ? <ArrowClockwise20Regular /> : <Spinner size="extra-tiny" />} disabled={!checks} onClick={run}>
              {checks ? t("about.refresh") : t("about.checking")}
            </Button>
          </div>
          {checks && (
            <div className={styles.list}>
              {checks.map((c) => {
                const Icon = icons[c.level];
                return (
                  <div key={c.id} className={styles.row}>
                    <Icon className={styles[c.level]} />
                    <Body1>
                      <b>{t(`about.checks.${c.id}`)}</b>
                    </Body1>
                    <Caption1 className={styles.muted}>{t(`about.status.${c.id}.${c.level}`, { detail: c.detail })}</Caption1>
                  </div>
                );
              })}
            </div>
          )}
        </>
      )}
    </>
  );
}
