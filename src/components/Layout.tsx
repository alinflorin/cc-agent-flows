import { useEffect, useState } from "react";
import { Outlet } from "react-router";
import { makeStyles, tokens } from "@fluentui/react-components";
import { useIsMobile } from "../useIsMobile";
import { Sidebar } from "./Sidebar";
import { TopBar } from "./TopBar";
import { Welcome } from "./Welcome";

const useStyles = makeStyles({
  root: {
    display: "flex",
    flexDirection: "column",
    height: "100%",
    backgroundColor: tokens.colorNeutralBackground1,
    color: tokens.colorNeutralForeground1,
  },
  body: { display: "flex", flex: 1, minHeight: 0 },
  main: { flex: 1, minWidth: 0, overflow: "auto", padding: tokens.spacingHorizontalXXL },
});

/** Top menu bar, sidebar on the left (behind a menu button on mobile) and the routed page. */
export function Layout() {
  const styles = useStyles();
  const mobile = useIsMobile();
  const [menuOpen, setMenuOpen] = useState(false);

  useEffect(() => {
    if (!mobile) setMenuOpen(false);
  }, [mobile]);

  return (
    <div className={styles.root}>
      <Welcome />
      <TopBar showMenuButton={mobile} onMenu={() => setMenuOpen(true)} />
      <div className={styles.body}>
        {/* Hidden on mobile; the overlay version only exists while open. */}
        {(!mobile || menuOpen) && (
          <Sidebar overlay={mobile} open={mobile ? menuOpen : true} onClose={() => setMenuOpen(false)} />
        )}
        <main className={styles.main}>
          <Outlet />
        </main>
      </div>
    </div>
  );
}
