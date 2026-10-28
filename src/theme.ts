import { useSyncExternalStore } from "react";
import { teamsDarkTheme, teamsLightTheme, type Theme as FluentTheme } from "@fluentui/react-components";
import { getPref, setPref } from "./prefs";

export type Theme = "auto" | "light" | "dark";
export const THEMES: Theme[] = ["auto", "light", "dark"];

const KEY = "theme";

function getTheme(): Theme {
  const saved = getPref<string>(KEY);
  return saved && (THEMES as string[]).includes(saved) ? (saved as Theme) : "auto";
}

const listeners = new Set<() => void>();

/** For `useSyncExternalStore`: lets the UI follow theme changes. */
function subscribeTheme(cb: () => void) {
  listeners.add(cb);
  return () => void listeners.delete(cb);
}

export function setTheme(theme: Theme) {
  setPref(KEY, theme);
  listeners.forEach((cb) => cb());
}

const osDark = () => window.matchMedia("(prefers-color-scheme: dark)");

function subscribeOs(cb: () => void) {
  const mq = osDark();
  mq.addEventListener("change", cb);
  return () => mq.removeEventListener("change", cb);
}

/** The saved preference ("auto" follows the OS). */
export function useThemePref(): Theme {
  return useSyncExternalStore(subscribeTheme, getTheme);
}

/** The Teams theme to render with, resolving "auto" against the OS setting. */
export function useFluentTheme(): { theme: FluentTheme; dark: boolean } {
  const pref = useThemePref();
  const os = useSyncExternalStore(subscribeOs, () => osDark().matches);
  const dark = pref === "auto" ? os : pref === "dark";
  return { theme: dark ? teamsDarkTheme : teamsLightTheme, dark };
}
