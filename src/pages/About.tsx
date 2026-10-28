import { useEffect, useState } from "react";
import { PageHeader } from "../components/PageHeader";
import { useTranslation } from "react-i18next";
import { Body1, makeStyles, tokens } from "@fluentui/react-components";
import { getVersion } from "@tauri-apps/api/app";
import { inTauri } from "../prefs";

const useStyles = makeStyles({
  logo: { width: "64px", height: "64px", marginBottom: tokens.spacingVerticalL },
});

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
      <PageHeader title={t("title")} description={t("about.tagline")} />
      <Body1 block>{t("about.version", { version: version || "—" })}</Body1>
    </>
  );
}
