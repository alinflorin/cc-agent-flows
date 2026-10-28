import { useLocation, useNavigate } from "react-router";
import { useTranslation } from "react-i18next";
import { NavDrawer, NavDrawerBody, NavDrawerFooter, NavDrawerHeader, NavItem, makeStyles, tokens } from "@fluentui/react-components";
import {
  Bot24Filled,
  Bot24Regular,
  Chat24Filled,
  Chat24Regular,
  DataBarVertical24Filled,
  DataBarVertical24Regular,
  Flowchart24Filled,
  Flowchart24Regular,
  Home24Filled,
  Home24Regular,
  Info24Filled,
  Info24Regular,
  Folder24Filled,
  Folder24Regular,
  Play24Filled,
  Play24Regular,
  Settings24Filled,
  Settings24Regular,
  bundleIcon,
} from "@fluentui/react-icons";

const HomeIcon = bundleIcon(Home24Filled, Home24Regular);
const WorkflowIcon = bundleIcon(Flowchart24Filled, Flowchart24Regular);
const SessionsIcon = bundleIcon(Chat24Filled, Chat24Regular);
const ProjectsIcon = bundleIcon(Folder24Filled, Folder24Regular);
const AnalyticsIcon = bundleIcon(DataBarVertical24Filled, DataBarVertical24Regular);
const RunsIcon = bundleIcon(Play24Filled, Play24Regular);
const ProfilesIcon = bundleIcon(Bot24Filled, Bot24Regular);
const AboutIcon = bundleIcon(Info24Filled, Info24Regular);
const SettingsIcon = bundleIcon(Settings24Filled, Settings24Regular);

export const SIDEBAR_WIDTH = 240;

const useStyles = makeStyles({
  // one surface for header, items and footer, and the footer pinned to the bottom
  drawer: {
    width: `${SIDEBAR_WIDTH}px`,
    height: "100%",
    display: "flex",
    flexDirection: "column",
    backgroundColor: tokens.colorNeutralBackground2,
    borderRight: `1px solid ${tokens.colorNeutralStroke2}`,
    "& .fui-NavItem, & .fui-NavSectionHeader": { backgroundColor: "transparent" },
    "& .fui-NavItem:hover": { backgroundColor: tokens.colorNeutralBackground2Hover },
    "& .fui-NavItem[aria-current=page]": { backgroundColor: tokens.colorNeutralBackground2Selected },
  },
  body: { flex: 1, minHeight: 0, overflowY: "auto", backgroundColor: "transparent" },
  footer: { flex: "none", borderTop: `1px solid ${tokens.colorNeutralStroke2}`, backgroundColor: "transparent", padding: `${tokens.spacingVerticalS} 4px ${tokens.spacingVerticalS} 10px` },
  brand: { display: "flex", alignItems: "center", gap: tokens.spacingHorizontalS, padding: `${tokens.spacingVerticalM} ${tokens.spacingHorizontalM}` },
  logo: { width: "28px", height: "28px", flex: "none" },
  name: { fontSize: tokens.fontSizeBase400, fontWeight: tokens.fontWeightSemibold, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" },
});

/** The left navigation, with Settings at the bottom. Inline beside the page on desktop, an overlay opened from a menu button on mobile. */
export function Sidebar({ overlay, open, onClose }: { overlay: boolean; open: boolean; onClose: () => void }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const navigate = useNavigate();
  const { pathname } = useLocation();
  // /sessions/<id> belongs to Sessions
  const selected = pathname.startsWith("/sessions") ? "/sessions" : pathname;

  return (
    <NavDrawer
      type={overlay ? "overlay" : "inline"}
      open={open}
      onOpenChange={(_, d) => !d.open && onClose()}
      selectedValue={selected}
      onNavItemSelect={(_, d) => {
        navigate(d.value);
        onClose();
      }}
      className={styles.drawer}
    >
      <NavDrawerHeader>
        <div className={styles.brand}>
          <img src="/logo.svg" alt="" className={styles.logo} />
          <span className={styles.name}>{t("title")}</span>
        </div>
      </NavDrawerHeader>
      <NavDrawerBody className={styles.body}>
        <NavItem value="/" icon={<HomeIcon />}>
          {t("nav.home")}
        </NavItem>
        <NavItem value="/workflow" icon={<WorkflowIcon />}>
          {t("nav.workflow")}
        </NavItem>
        <NavItem value="/projects" icon={<ProjectsIcon />}>
          {t("nav.projects")}
        </NavItem>
        <NavItem value="/sessions" icon={<SessionsIcon />}>
          {t("nav.sessions")}
        </NavItem>
        <NavItem value="/profiles" icon={<ProfilesIcon />}>
          {t("nav.profiles")}
        </NavItem>
        <NavItem value="/runs" icon={<RunsIcon />}>
          {t("nav.runs")}
        </NavItem>
        <NavItem value="/analytics" icon={<AnalyticsIcon />}>
          {t("nav.analytics")}
        </NavItem>
        <NavItem value="/about" icon={<AboutIcon />}>
          {t("nav.about")}
        </NavItem>
      </NavDrawerBody>
      <NavDrawerFooter className={styles.footer}>
        <NavItem value="/settings" icon={<SettingsIcon />}>
          {t("nav.settings")}
        </NavItem>
      </NavDrawerFooter>
    </NavDrawer>
  );
}
