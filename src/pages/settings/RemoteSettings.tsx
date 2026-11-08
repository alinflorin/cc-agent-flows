import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Body1, Button, Field, Input, MessageBar, MessageBarBody, Switch, makeStyles, tokens } from "@fluentui/react-components";
import { SettingsIntro } from "../../components/SettingsIntro";
import { inTauri } from "../../prefs";
import { useNotify } from "../../notifications";
import { call } from "../../tauri";

interface RemoteStatus {
  enabled: boolean;
  bind: string;
  port: number;
  auth: boolean;
  user: string;
  has_password: boolean;
  forced: boolean;
  url: string;
  error: string | null;
}

const useStyles = makeStyles({
  section: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM, maxWidth: "640px" },
  row: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "center" },
  mono: { fontFamily: tokens.fontFamilyMonospace },
});

interface Edit {
  enabled: boolean;
  bind: string;
  port: string;
  auth: boolean;
  user: string;
  password: string;
}
const pick = (s: RemoteStatus): Edit => ({ enabled: s.enabled, bind: s.bind, port: String(s.port), auth: s.auth, user: s.user, password: "" });

const isLoopback = (ip: string) => ip === "::1" || ip.startsWith("127.");

/** Settings > Remote control: serves the app's UI and commands to a browser, started with the app. */
export function RemoteSettings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();
  const [status, setStatus] = useState<RemoteStatus | null>(null);
  const [edit, setEdit] = useState<Edit | null>(null);

  const load = useCallback(async () => {
    try {
      const s = await call<RemoteStatus>("remote_get");
      setStatus(s);
      setEdit(pick(s));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify]);

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  if (!desktop) return <Body1 block>{t("settings.llm.tokenDesktopOnly")}</Body1>;
  if (!status || !edit) return null;

  const dirty = JSON.stringify(edit) !== JSON.stringify(pick(status));
  const set = (patch: Partial<Edit>) => setEdit({ ...edit, ...patch });
  const port = Number(edit.port);
  const needsLogin = edit.auth && (!edit.user.trim() || (!edit.password && !status.has_password));
  const valid = port >= 1024 && port <= 65535 && !!edit.bind.trim() && !needsLogin;
  const open = !isLoopback(edit.bind.trim());

  const save = async () => {
    try {
      const s = await call<RemoteStatus>("remote_set", { enabled: edit.enabled, bind: edit.bind.trim(), port, auth: edit.auth, user: edit.user, password: edit.password || null });
      setStatus(s);
      setEdit(pick(s));
      notify(t("settings.saved"), "success");
      // the listener restarts a moment later: ask again for a busy port
      setTimeout(() => void call<RemoteStatus>("remote_get").then(setStatus).catch(() => {}), 1200);
    } catch (e) {
      notify(String(e), "error");
    }
  };

  return (
    <div className={styles.section}>
      <SettingsIntro text={t("settings.remote.intro")} />
      {status.error && (
        <MessageBar intent="error">
          <MessageBarBody>{status.error}</MessageBarBody>
        </MessageBar>
      )}
      {status.forced && (
        <MessageBar intent="info">
          <MessageBarBody>{t("settings.remote.forced")}</MessageBarBody>
        </MessageBar>
      )}
      <Switch checked={edit.enabled} label={t("settings.remote.enabled")} onChange={(_, d) => set({ enabled: d.checked })} />
      <Field label={t("settings.remote.bind")} hint={t("settings.remote.bindHint")}>
        <Input value={edit.bind} onChange={(_, d) => set({ bind: d.value })} />
      </Field>
      <Field label={t("settings.remote.port")}>
        <Input type="number" min={1024} max={65535} value={edit.port} onChange={(_, d) => set({ port: d.value })} />
      </Field>
      <Switch checked={edit.auth} label={t("settings.remote.auth")} onChange={(_, d) => set({ auth: d.checked })} />
      {edit.auth && (
        <>
          <Field label={t("settings.remote.user")}>
            <Input value={edit.user} autoComplete="off" onChange={(_, d) => set({ user: d.value })} />
          </Field>
          <Field label={t("settings.remote.password")} hint={status.has_password ? t("settings.remote.passwordKept") : undefined}>
            <Input type="password" value={edit.password} autoComplete="new-password" onChange={(_, d) => set({ password: d.value })} />
          </Field>
        </>
      )}
      {open && !edit.auth && (
        <MessageBar intent="warning">
          <MessageBarBody>{t("settings.remote.openWarning")}</MessageBarBody>
        </MessageBar>
      )}
      <div className={styles.row}>
        <Button appearance="primary" disabled={!dirty || !valid} onClick={() => void save()}>
          {t("common.save")}
        </Button>
        <Button disabled={!dirty} onClick={() => setEdit(pick(status))}>
          {t("common.reset")}
        </Button>
      </div>
      {(status.enabled || status.forced) && !status.error && (
        <Field label={t("settings.remote.address")} hint={t("settings.remote.addressHint")}>
          <Input readOnly className={styles.mono} value={status.url} />
        </Field>
      )}
    </div>
  );
}
