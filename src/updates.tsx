import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { check } from "@tauri-apps/plugin-updater";
import { openUrl } from "@tauri-apps/plugin-opener";
import { relaunch } from "@tauri-apps/plugin-process";
import { getPref, inTauri, setPref } from "./prefs";
import { useNotify } from "./notifications";

const RELEASES_URL = "https://github.com/alinflorin/cc-agent-flows/releases/latest";

/** False when the installation is managed by a package manager (a Linux .deb or .rpm). */
const canSelfUpdate = () => invoke<boolean>("can_self_update");

const errorText = (e: unknown) => (e instanceof Error ? e.message : typeof e === "string" ? e : JSON.stringify(e));

type Updates = {
  autoUpdate: boolean;
  setAutoUpdate: (on: boolean) => void;
  /** Version that was installed and waits for a restart. */
  ready: string | null;
  checking: boolean;
  checkForUpdates: () => Promise<void>;
  restartToUpdate: () => void;
};

const Ctx = createContext<Updates | null>(null);

export function useUpdates() {
  const v = useContext(Ctx);
  if (!v) throw new Error("useUpdates outside UpdatesProvider");
  return v;
}

export function UpdatesProvider({ children }: { children: ReactNode }) {
  const { t } = useTranslation();
  const notify = useNotify();
  const [autoUpdate, setAutoUpdateState] = useState(() => getPref<boolean>("autoUpdate") !== false);
  const [ready, setReady] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);

  const setAutoUpdate = (on: boolean) => {
    setPref("autoUpdate", on);
    setAutoUpdateState(on);
  };

  const restartToUpdate = () => void relaunch();

  // `silent` is the startup check: no toasts unless an update was installed.
  // Installing never restarts by itself on a silent check.
  const run = useCallback(
    async (silent: boolean) => {
      if (!inTauri()) return;
      setChecking(true);
      try {
        const update = await check();
        if (!update) return silent ? undefined : notify(t("updates.upToDate"));
        if (!(await canSelfUpdate())) {
          // installed through a package manager: tell, but leave the install to it
          notify(t("updates.managed", { version: update.version }), "warning");
          if (!silent) await openUrl(RELEASES_URL);
          return;
        }
        if (!silent) notify(t("updates.downloading", { version: update.version }));
        await update.downloadAndInstall();
        setReady(update.version);
        if (silent) notify(t("updates.ready", { version: update.version }), "success");
        else relaunch();
      } catch (e) {
        if (!silent) notify(errorText(e), "error");
      } finally {
        setChecking(false);
      }
    },
    [notify, t],
  );

  const startup = useRef(autoUpdate);
  useEffect(() => {
    if (!inTauri() || !import.meta.env.PROD || !startup.current) return;
    const id = setTimeout(() => void run(true), 5000);
    return () => clearTimeout(id);
    // once per launch
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <Ctx.Provider value={{ autoUpdate, setAutoUpdate, ready, checking, checkForUpdates: () => run(false), restartToUpdate }}>
      {children}
    </Ctx.Provider>
  );
}
