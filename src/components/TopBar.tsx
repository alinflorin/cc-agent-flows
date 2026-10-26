import { useNavigate } from "react-router";
import { useTranslation } from "react-i18next";
import {
  Button,
  Menu,
  MenuItemRadio,
  MenuList,
  MenuPopover,
  MenuTrigger,
  Tooltip,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import {
  Color24Regular,
  LocalLanguage24Regular,
  Navigation24Regular,
  Settings24Regular,
  WeatherMoon24Regular,
  WeatherSunny24Regular,
} from "@fluentui/react-icons";
import { LANGUAGE_NAMES, SUPPORTED_LANGUAGES, setLanguage, type Language } from "../i18n";
import { THEMES, setTheme, useFluentTheme, useThemePref, type Theme } from "../theme";

const useStyles = makeStyles({
  bar: {
    display: "flex",
    alignItems: "center",
    gap: tokens.spacingHorizontalS,
    height: "48px",
    padding: `0 ${tokens.spacingHorizontalM}`,
    flex: "none",
    backgroundColor: tokens.colorBrandBackground,
    color: tokens.colorNeutralForegroundOnBrand,
  },
  title: {
    fontSize: tokens.fontSizeBase400,
    fontWeight: tokens.fontWeightSemibold,
    flex: 1,
    minWidth: 0,
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
  },
  logo: { width: "24px", height: "24px", flex: "none" },
  // Fluent's subtle buttons are dark-on-light; on the brand bar they need the on-brand colours.
  onBrand: {
    color: tokens.colorNeutralForegroundOnBrand,
    ":hover": { color: tokens.colorNeutralForegroundOnBrand, backgroundColor: "rgba(255,255,255,0.16)" },
    ":hover:active": { color: tokens.colorNeutralForegroundOnBrand, backgroundColor: "rgba(255,255,255,0.24)" },
  },
});

const THEME_ICONS = { auto: <Color24Regular />, light: <WeatherSunny24Regular />, dark: <WeatherMoon24Regular /> };

/** Menu bar across the top: app name on the left; theme, language and settings on the right. */
export function TopBar({ showMenuButton, onMenu }: { showMenuButton: boolean; onMenu: () => void }) {
  const { t, i18n } = useTranslation();
  const styles = useStyles();
  const navigate = useNavigate();
  const themePref = useThemePref();
  const { dark } = useFluentTheme();
  const lang = (SUPPORTED_LANGUAGES as readonly string[]).includes(i18n.language) ? (i18n.language as Language) : "en";

  return (
    <header className={styles.bar}>
      {showMenuButton && (
        <Button
          appearance="transparent"
          className={styles.onBrand}
          icon={<Navigation24Regular />}
          aria-label={t("nav.menu")}
          onClick={onMenu}
        />
      )}
      <img src="/logo.svg" alt="" className={styles.logo} />
      <span className={styles.title}>{t("title")}</span>

      <Menu
        checkedValues={{ theme: [themePref] }}
        onCheckedValueChange={(_, d) => setTheme(d.checkedItems[0] as Theme)}
      >
        <MenuTrigger disableButtonEnhancement>
          <Tooltip content={t("theme.label")} relationship="label">
            <Button
              appearance="transparent"
              className={styles.onBrand}
              icon={themePref === "auto" ? (dark ? THEME_ICONS.dark : THEME_ICONS.light) : THEME_ICONS[themePref]}
            />
          </Tooltip>
        </MenuTrigger>
        <MenuPopover>
          <MenuList>
            {THEMES.map((th) => (
              <MenuItemRadio key={th} name="theme" value={th} icon={THEME_ICONS[th]}>
                {t(`theme.${th}`)}
              </MenuItemRadio>
            ))}
          </MenuList>
        </MenuPopover>
      </Menu>

      <Menu checkedValues={{ lang: [lang] }} onCheckedValueChange={(_, d) => void setLanguage(d.checkedItems[0] as Language)}>
        <MenuTrigger disableButtonEnhancement>
          <Tooltip content={t("language")} relationship="label">
            <Button appearance="transparent" className={styles.onBrand} icon={<LocalLanguage24Regular />} />
          </Tooltip>
        </MenuTrigger>
        <MenuPopover>
          <MenuList>
            {SUPPORTED_LANGUAGES.map((l) => (
              <MenuItemRadio key={l} name="lang" value={l}>
                {LANGUAGE_NAMES[l]}
              </MenuItemRadio>
            ))}
          </MenuList>
        </MenuPopover>
      </Menu>

      <Tooltip content={t("nav.settings")} relationship="label">
        <Button
          appearance="transparent"
          className={styles.onBrand}
          icon={<Settings24Regular />}
          onClick={() => navigate("/settings")}
        />
      </Tooltip>
    </header>
  );
}
