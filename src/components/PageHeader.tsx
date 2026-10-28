import type { ReactNode } from "react";
import { Body1, Title2, makeStyles, tokens } from "@fluentui/react-components";

const useStyles = makeStyles({
  root: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalXS, marginBottom: tokens.spacingVerticalXL },
  row: { display: "flex", alignItems: "center", justifyContent: "space-between", gap: tokens.spacingHorizontalM, minHeight: "40px" },
  title: { fontWeight: tokens.fontWeightSemibold, letterSpacing: "-0.01em" },
  actions: { display: "flex", alignItems: "center", gap: tokens.spacingHorizontalS, flex: "none" },
  description: { color: tokens.colorNeutralForeground3 },
});

/** The title of a page: the same height and spacing everywhere, an optional line under it and the page's actions on the right. */
export function PageHeader({ title, description, actions }: { title: ReactNode; description?: ReactNode; actions?: ReactNode }) {
  const styles = useStyles();
  return (
    <header className={styles.root}>
      <div className={styles.row}>
        <Title2 as="h1" className={styles.title}>{title}</Title2>
        {actions && <div className={styles.actions}>{actions}</div>}
      </div>
      {description && <Body1 className={styles.description}>{description}</Body1>}
    </header>
  );
}
