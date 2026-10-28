import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { getPref, inTauri } from "../prefs";

interface Entry {
  kind: string;
  body: Record<string, unknown>;
}

/**
 * Desktop notifications for what needs the user while the app is in the background: an agent
 * waiting for an answer or an approval, and a session that ended. Renders nothing. Switched off
 * by the `notifications` preference (Settings > General).
 */
export function Notifier() {
  const { t } = useTranslation();

  useEffect(() => {
    if (!inTauri()) return;
    let off: (() => void) | undefined;
    let gone = false;
    void (async () => {
      const [{ listen }, { invoke }, notification] = await Promise.all([import("@tauri-apps/api/event"), import("@tauri-apps/api/core"), import("@tauri-apps/plugin-notification")]);
      const show = async (session: string, key: string, vars: Record<string, unknown>) => {
        if (getPref<boolean>("notifications") === false || document.hasFocus()) return;
        try {
          if (!(await notification.isPermissionGranted()) && (await notification.requestPermission()) !== "granted") return;
          const s = await invoke<{ session: { name: string; task: string } }>("session_get", { id: session });
          const name = s.session.name || s.session.task.slice(0, 60);
          notification.sendNotification({ title: name || t("sessions.untitled"), body: t(key, vars) });
        } catch {
          // notifications are a convenience: never in the way
        }
      };
      const u = await listen<{ session: string; entry: Entry }>("session-log", ({ payload: { session, entry } }) => {
        const b = entry.body;
        if (entry.kind === "wait") void show(session, `session.notify.${b.kind === "approve" ? "approve" : "questions"}`, { label: String(b.label ?? "") });
        else if (entry.kind === "status" && ["done", "failed"].includes(String(b.status))) void show(session, `session.notify.${b.status}`, {});
      });
      if (gone) u();
      else off = u;
    })();
    return () => {
      gone = true;
      off?.();
    };
  }, [t]);

  return null;
}
