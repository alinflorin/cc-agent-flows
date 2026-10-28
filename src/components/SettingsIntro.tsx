import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Body1, Caption1, makeStyles, tokens } from "@fluentui/react-components";
import { useProject } from "../project";

const useStyles = makeStyles({
  root: { display: "flex", flexDirection: "column", alignItems: "flex-start", gap: tokens.spacingVerticalM, marginBottom: tokens.spacingVerticalL },
  text: { color: tokens.colorNeutralForeground3, maxWidth: "76ch" },
  note: { color: tokens.colorNeutralForeground4, maxWidth: "76ch" },
});

/** What a settings tab is for, then a line on where it applies (global or the project), then its main action (the primary button) under it: the same on every tab. */
export function SettingsIntro({ text, children }: { text: ReactNode; children?: ReactNode }) {
  const styles = useStyles();
  const { t } = useTranslation();
  const project = useProject();
  return (
    <div className={styles.root}>
      <Body1 className={styles.text}>{text}</Body1>
      <Caption1 className={styles.note}>{t(project === null ? "settings.globalNote" : "projects.scopeNote")}</Caption1>
      {children}
    </div>
  );
}
