import { useEffect, useState } from "react";
import { Outlet } from "react-router";
import { useTranslation } from "react-i18next";
import { Button, makeStyles, tokens } from "@fluentui/react-components";
import { Navigation24Regular } from "@fluentui/react-icons";
import { useIsMobile } from "../useIsMobile";
import { Sidebar } from "./Sidebar";
import { Welcome } from "./Welcome";

const useStyles = makeStyles({
  root: {
    display: "flex",
    height: "100%",
    backgroundColor: tokens.colorNeutralBackground1,
    color: tokens.colorNeutralForeground1,
  },
  column: { display: "flex", flexDirection: "column", flex: 1, minWidth: 0 },
  mobileBar: { flex: "none", padding: `${tokens.spacingVerticalXS} ${tokens.spacingHorizontalS}`, borderBottom: `1px solid ${tokens.colorNeutralStroke2}` },
  main: { flex: 1, minWidth: 0, overflow: "auto", padding: tokens.spacingHorizontalXXL },
});

/** The sidebar on the left (behind a menu button on mobile) and the routed page. */
export function Layout() {
  const { t } = useTranslation();
  const styles = useStyles();
  const mobile = useIsMobile();
  const [menuOpen, setMenuOpen] = useState(false);

  useEffect(() => {
    if (!mobile) setMenuOpen(false);
  }, [mobile]);

  return (
    <div className={styles.root}>
      <Welcome />
      {/* Hidden on mobile; the overlay version only exists while open. */}
      {(!mobile || menuOpen) && <Sidebar overlay={mobile} open={mobile ? menuOpen : true} onClose={() => setMenuOpen(false)} />}
      <div className={styles.column}>
        {mobile && (
          <div className={styles.mobileBar}>
            <Button appearance="subtle" icon={<Navigation24Regular />} aria-label={t("nav.menu")} onClick={() => setMenuOpen(true)} />
          </div>
        )}
        <main className={styles.main}>
          <Outlet />
        </main>
      </div>
    </div>
  );
}
