import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Outlet } from "react-router";
import { useTranslation } from "react-i18next";
import { Button, makeStyles, tokens } from "@fluentui/react-components";
import { Navigation24Regular } from "@fluentui/react-icons";
import { useIsMobile } from "../useIsMobile";
import { Notifier } from "./Notifier";
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
  main: { flex: 1, minWidth: 0, overflow: "auto", padding: `${tokens.spacingVerticalXXL} ${tokens.spacingHorizontalXXXL}` },
  content: { maxWidth: "1400px", margin: "0 auto" },
});

/** The sidebar on the left (behind a menu button on mobile) and the routed page. */
export function Layout() {
  const { t } = useTranslation();
  const styles = useStyles();
  const mobile = useIsMobile();
  const [menuOpen, setMenuOpen] = useState(false);

  // Bumped when an agent changes things through the MCP server; the keyed
  // subtrees remount and so reload their data.
  const [rev, setRev] = useState(0);
  const timer = useRef<number>(0);
  useEffect(() => {
    let off: (() => void) | undefined;
    let gone = false;
    listen("config-changed", () => {
      window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => setRev((r) => r + 1), 150);
    })
      .then((u) => (gone ? u() : (off = u)))
      .catch(() => {});
    return () => {
      gone = true;
      off?.();
      window.clearTimeout(timer.current);
    };
  }, []);

  useEffect(() => {
    if (!mobile) setMenuOpen(false);
  }, [mobile]);

  return (
    <div className={styles.root}>
      <Welcome />
      <Notifier />
      {/* Hidden on mobile; the overlay version only exists while open. */}
      {(!mobile || menuOpen) && <Sidebar key={rev} overlay={mobile} open={mobile ? menuOpen : true} onClose={() => setMenuOpen(false)} />}
      <div className={styles.column}>
        {mobile && (
          <div className={styles.mobileBar}>
            <Button appearance="subtle" icon={<Navigation24Regular />} aria-label={t("nav.menu")} onClick={() => setMenuOpen(true)} />
          </div>
        )}
        <main className={styles.main}>
          <div className={styles.content} key={rev}>
            <Outlet />
          </div>
        </main>
      </div>
    </div>
  );
}
