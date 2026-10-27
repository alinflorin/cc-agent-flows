import { useLocation, useNavigate } from "react-router";
import { useTranslation } from "react-i18next";
import { NavDrawer, NavDrawerBody, NavItem, makeStyles, tokens } from "@fluentui/react-components";
import { Bot24Filled, Bot24Regular, Home24Filled, Home24Regular, Info24Filled, Info24Regular, bundleIcon } from "@fluentui/react-icons";

const HomeIcon = bundleIcon(Home24Filled, Home24Regular);
const ProfilesIcon = bundleIcon(Bot24Filled, Bot24Regular);
const AboutIcon = bundleIcon(Info24Filled, Info24Regular);

export const SIDEBAR_WIDTH = 240;

const useStyles = makeStyles({
  drawer: {
    width: `${SIDEBAR_WIDTH}px`,
    backgroundColor: tokens.colorNeutralBackground2,
    borderRight: `1px solid ${tokens.colorNeutralStroke2}`,
  },
});

/** The left navigation. Inline beside the page on desktop, an overlay opened from the top bar on mobile. */
export function Sidebar({ overlay, open, onClose }: { overlay: boolean; open: boolean; onClose: () => void }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const navigate = useNavigate();
  const { pathname } = useLocation();

  return (
    <NavDrawer
      type={overlay ? "overlay" : "inline"}
      open={open}
      onOpenChange={(_, d) => !d.open && onClose()}
      selectedValue={pathname}
      onNavItemSelect={(_, d) => {
        navigate(d.value);
        onClose();
      }}
      className={styles.drawer}
    >
      <NavDrawerBody>
        <NavItem value="/" icon={<HomeIcon />}>
          {t("nav.home")}
        </NavItem>
        <NavItem value="/profiles" icon={<ProfilesIcon />}>
          {t("nav.profiles")}
        </NavItem>
        <NavItem value="/about" icon={<AboutIcon />}>
          {t("nav.about")}
        </NavItem>
      </NavDrawerBody>
    </NavDrawer>
  );
}
