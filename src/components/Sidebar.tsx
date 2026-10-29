import { useEffect, useState, type ReactElement } from "react";
import { useLocation, useNavigate } from "react-router";
import { call } from "../tauri";
import { useTranslation } from "react-i18next";
import {
  NavDrawer,
  NavDrawerBody,
  NavDrawerHeader,
  NavItem,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import {
  ArrowLeft24Regular,
  ArrowSwap24Filled,
  ArrowSwap24Regular,
  Bot24Filled,
  Bot24Regular,
  Chat24Filled,
  Chat24Regular,
  DataBarVertical24Filled,
  DataBarVertical24Regular,
  Flowchart24Filled,
  Flowchart24Regular,
  Folder24Filled,
  Folder24Regular,
  Info24Filled,
  Info24Regular,
  Key24Filled,
  Key24Regular,
  Play24Filled,
  Play24Regular,
  Box24Filled,
  Box24Regular,
  Braces24Filled,
  Braces24Regular,
  PlugConnected24Filled,
  PlugConnected24Regular,
  Brain24Filled,
  Brain24Regular,
  Globe24Filled,
  Globe24Regular,
  Link24Filled,
  Link24Regular,
  Options24Filled,
  Options24Regular,
  Settings24Filled,
  Settings24Regular,
  bundleIcon,
  Server24Filled,
  Server24Regular,
} from "@fluentui/react-icons";

const ProjectsIcon = bundleIcon(Folder24Filled, Folder24Regular);
const ImagesIcon = bundleIcon(Box24Filled, Box24Regular);
const VariablesIcon = bundleIcon(Braces24Filled, Braces24Regular);
const SecretsIcon = bundleIcon(Key24Filled, Key24Regular);
const InjectionsIcon = bundleIcon(
  PlugConnected24Filled,
  PlugConnected24Regular,
);
const IntegrationsIcon = bundleIcon(Link24Filled, Link24Regular);
const VmIcon = bundleIcon(Server24Filled, Server24Regular);
const TransferIcon = bundleIcon(ArrowSwap24Filled, ArrowSwap24Regular);
const WorkflowIcon = bundleIcon(Flowchart24Filled, Flowchart24Regular);
const SessionsIcon = bundleIcon(Chat24Filled, Chat24Regular);
const AnalyticsIcon = bundleIcon(
  DataBarVertical24Filled,
  DataBarVertical24Regular,
);
const RunsIcon = bundleIcon(Play24Filled, Play24Regular);
const ProfilesIcon = bundleIcon(Bot24Filled, Bot24Regular);
const AboutIcon = bundleIcon(Info24Filled, Info24Regular);
const LlmIcon = bundleIcon(Brain24Filled, Brain24Regular);
const NetworkIcon = bundleIcon(Globe24Filled, Globe24Regular);
const GeneralIcon = bundleIcon(Options24Filled, Options24Regular);
const SettingsIcon = bundleIcon(Settings24Filled, Settings24Regular);

const SIDEBAR_WIDTH = 240;

const useStyles = makeStyles({
  // one surface for header, items and footer, and the footer pinned to the bottom
  drawer: {
    width: `${SIDEBAR_WIDTH}px`,
    height: "100%",
    display: "flex",
    flexDirection: "column",
    backgroundColor: tokens.colorNeutralBackground2,
    borderRight: `1px solid ${tokens.colorNeutralStroke2}`,
    "& .fui-NavItem, & .fui-NavSectionHeader": {
      backgroundColor: "transparent",
    },
    "& .fui-NavItem:hover": {
      backgroundColor: tokens.colorNeutralBackground2Hover,
    },
    "& .fui-NavItem[aria-current=page]": {
      backgroundColor: tokens.colorNeutralBackground2Selected,
    },
  },
  body: {
    flex: 1,
    minHeight: 0,
    overflowY: "auto",
    backgroundColor: "transparent",
  },
  footer: {
    flex: "none",
    borderTop: `1px solid ${tokens.colorNeutralStroke2}`,
    backgroundColor: "transparent",
    padding: `${tokens.spacingVerticalS} 4px ${tokens.spacingVerticalS} 10px`,
  },
  brand: {
    display: "flex",
    alignItems: "center",
    gap: tokens.spacingHorizontalS,
    padding: `${tokens.spacingVerticalM} ${tokens.spacingHorizontalM}`,
  },
  logo: { width: "28px", height: "28px", flex: "none" },
  project: {
    padding: `${tokens.spacingVerticalXS} ${tokens.spacingHorizontalM} ${tokens.spacingVerticalS}`,
    fontWeight: tokens.fontWeightSemibold,
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
  },
  name: {
    fontSize: tokens.fontSizeBase400,
    fontWeight: tokens.fontWeightSemibold,
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
  },
});

/** The name of a project, for the heading of its navigation. */
function useProjectName(pid: string | null) {
  const [name, setName] = useState("");
  useEffect(() => {
    setName("");
    if (!pid) return;
    const load = () => {
      call<{ name: string }>("projects_get", { id: pid }).then(
        (p) => setName(p.name),
        () => {},
      );
    };
    load();
    window.addEventListener("projects-changed", load);
    return () => window.removeEventListener("projects-changed", load);
  }, [pid]);
  return name;
}

/** The left navigation, with Settings at the bottom. Inline beside the page on desktop, an overlay opened from a menu button on mobile. Inside a project it lists the project's pages; otherwise the projects and About. */
export function Sidebar({
  overlay,
  open,
  onClose,
}: {
  overlay: boolean;
  open: boolean;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const styles = useStyles();
  const navigate = useNavigate();
  const { pathname } = useLocation();
  const inProject = /^\/p\/([^/]+)/.exec(pathname);
  const pid = inProject?.[1] ?? null;
  const name = useProjectName(pid);
  // /p/<id>/sessions/<sid> belongs to Sessions
  const section = pid ? (pathname.split("/")[3] ?? "") : "";
  const inSettings = pathname.startsWith("/settings");
  const selected = pid
    ? `/p/${pid}/${section}`
    : pathname === "/"
      ? "/"
      : pathname;
  const setting = (tab: string, label: string, icon: ReactElement) => (
    <NavItem value={`/settings/${tab}`} icon={icon}>
      {label}
    </NavItem>
  );
  const item = (path: string, label: string, icon: ReactElement) => (
    <NavItem value={`/p/${pid}/${path}`} icon={icon}>
      {label}
    </NavItem>
  );

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
        {pid ? (
          <>
            <NavItem value="/" icon={<ArrowLeft24Regular />}>
              {t("nav.back")}
            </NavItem>
            <div className={styles.project} title={name}>
              {name}
            </div>
            {item("settings", t("settings.tabs.general"), <GeneralIcon />)}
            {item("workflow", t("nav.workflow"), <WorkflowIcon />)}
            {item("sessions", t("nav.sessions"), <SessionsIcon />)}
            {item("profiles", t("nav.profiles"), <ProfilesIcon />)}
            {item("runs", t("nav.runs"), <RunsIcon />)}
            {item("analytics", t("nav.analytics"), <AnalyticsIcon />)}
            {item("images", t("settings.tabs.images"), <ImagesIcon />)}
            {item("variables", t("settings.tabs.variables"), <VariablesIcon />)}
            {item("secrets", t("settings.tabs.secrets"), <SecretsIcon />)}
            {item(
              "injections",
              t("settings.tabs.injections"),
              <InjectionsIcon />,
            )}
            {item(
              "integrations",
              t("settings.tabs.integrations"),
              <IntegrationsIcon />,
            )}
            {item("network", t("settings.tabs.network"), <NetworkIcon />)}
          </>
        ) : inSettings ? (
          <>
            <NavItem value="/" icon={<ArrowLeft24Regular />}>
              {t("nav.back")}
            </NavItem>
            <div className={styles.project}>{t("nav.settings")}</div>
            {setting("general", t("settings.tabs.general"), <GeneralIcon />)}
            {setting("llm", t("settings.tabs.llm"), <LlmIcon />)}
            {setting("vm", t("settings.tabs.vm"), <VmIcon />)}
            {setting("images", t("settings.globalTabs.images"), <ImagesIcon />)}
            {setting(
              "network",
              t("settings.globalTabs.network"),
              <NetworkIcon />,
            )}
            {setting(
              "variables",
              t("settings.globalTabs.variables"),
              <VariablesIcon />,
            )}
            {setting(
              "secrets",
              t("settings.globalTabs.secrets"),
              <SecretsIcon />,
            )}
            {setting(
              "injections",
              t("settings.globalTabs.injections"),
              <InjectionsIcon />,
            )}
            {setting(
              "integrations",
              t("settings.globalTabs.integrations"),
              <IntegrationsIcon />,
            )}
            {setting("transfer", t("settings.tabs.transfer"), <TransferIcon />)}
          </>
        ) : (
          <>
            <NavItem value="/" icon={<ProjectsIcon />}>
              {t("nav.projects")}
            </NavItem>
            <NavItem value="/settings/general" icon={<SettingsIcon />}>
              {t("nav.settings")}
            </NavItem>
            <NavItem value="/analytics" icon={<AnalyticsIcon />}>
              {t("nav.analytics")}
            </NavItem>
            <NavItem value="/about" icon={<AboutIcon />}>
              {t("nav.about")}
            </NavItem>
          </>
        )}
      </NavDrawerBody>
    </NavDrawer>
  );
}
