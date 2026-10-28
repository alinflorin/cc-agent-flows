import type { ReactNode } from "react";
import { Body1, makeStyles, tokens } from "@fluentui/react-components";

const useStyles = makeStyles({
  root: { display: "flex", flexDirection: "column", alignItems: "flex-start", gap: tokens.spacingVerticalM, marginBottom: tokens.spacingVerticalL },
  text: { color: tokens.colorNeutralForeground3, maxWidth: "76ch" },
});

/** What a settings tab is for, then its main action (the primary button) under it: the same on every tab. */
export function SettingsIntro({ text, children }: { text: ReactNode; children?: ReactNode }) {
  const styles = useStyles();
  return (
    <div className={styles.root}>
      <Body1 className={styles.text}>{text}</Body1>
      {children}
    </div>
  );
}
