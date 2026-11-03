import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Body1, Button, Field, Input, MessageBar, MessageBarBody, Subtitle1, Switch, Textarea, makeStyles, tokens } from "@fluentui/react-components";
import { SettingsIntro } from "../../components/SettingsIntro";
import { inTauri } from "../../prefs";
import { useNotify } from "../../notifications";
import { call } from "../../tauri";

interface McpConfig {
  enabled: boolean;
  port: number;
  token: string;
  write: boolean;
  write_global: boolean;
  runs: boolean;
}
interface McpStatus {
  config: McpConfig;
  url: string;
  error: string | null;
}

const useStyles = makeStyles({
  section: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM, maxWidth: "640px" },
  row: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "center" },
  grow: { flexGrow: 1 },
  mono: { fontFamily: tokens.fontFamilyMonospace, fontSize: tokens.fontSizeBase200 },
});

type Edit = Pick<McpConfig, "enabled" | "port" | "write" | "write_global" | "runs">;
const pick = (c: McpConfig): Edit => ({ enabled: c.enabled, port: c.port, write: c.write, write_global: c.write_global, runs: c.runs });

/** Settings > MCP: lets the user's own agent (Claude Code or any MCP client) configure projects and global settings through `ccaf`. */
export function McpSettings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();
  const [status, setStatus] = useState<McpStatus | null>(null);
  const [edit, setEdit] = useState<Edit | null>(null);
  const [port, setPort] = useState("");
  const [show, setShow] = useState(false);

  const load = useCallback(async () => {
    try {
      const s = await call<McpStatus>("mcp_get");
      setStatus(s);
      setEdit(pick(s.config));
      setPort(String(s.config.port));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify]);

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  if (!desktop) return <Body1 block>{t("settings.llm.tokenDesktopOnly")}</Body1>;
  if (!status || !edit) return null;

  const values: Edit = { ...edit, port: Number(port) || 0 };
  const dirty = JSON.stringify(values) !== JSON.stringify(pick(status.config));
  const set = (patch: Partial<Edit>) => setEdit({ ...edit, ...patch });

  const save = async () => {
    try {
      const s = await call<McpStatus>("mcp_set", { enabled: values.enabled, port: values.port, write: values.write, writeGlobal: values.write_global, runs: values.runs });
      setStatus(s);
      setEdit(pick(s.config));
      setPort(String(s.config.port));
      notify(t("settings.saved"), "success");
      // the listener binds a moment later: ask again for a busy port
      setTimeout(() => void call<McpStatus>("mcp_get").then(setStatus), 600);
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const newToken = async () => {
    try {
      setStatus(await call<McpStatus>("mcp_new_token"));
      notify(t("settings.mcp.tokenChanged"), "success");
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const copy = (text: string) => {
    navigator.clipboard.writeText(text).then(
      () => notify(t("settings.mcp.copied"), "success"),
      (e) => notify(String(e), "error"),
    );
  };

  const quickAdd = async (target: "claude_code" | "claude_desktop" | "codex") => {
    try {
      notify(await call<string>("mcp_quick_add", { target }), "success");
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const token = status.config.token;
  const ready = status.config.enabled && !!token && !dirty;
  const shown = (real: string) => (show || !token ? real : real.split(token).join("••••••••"));
  const cli = `claude mcp add --transport http ccaf ${status.url} --header "Authorization: Bearer ${token}"`;
  const mcpJson = JSON.stringify({ mcpServers: { ccaf: { type: "http", url: status.url, headers: { Authorization: `Bearer ${token}` } } } }, null, 2);

  return (
    <div className={styles.section}>
      <SettingsIntro text={t("settings.mcp.intro")} />
      {status.error && (
        <MessageBar intent="error">
          <MessageBarBody>{status.error}</MessageBarBody>
        </MessageBar>
      )}
      <Switch checked={edit.enabled} label={t("settings.mcp.enabled")} onChange={(_, d) => set({ enabled: d.checked })} />
      <Field label={t("settings.mcp.port")} hint={t("settings.mcp.portHint")}>
        <Input type="number" min={1024} max={65535} value={port} onChange={(_, d) => setPort(d.value)} />
      </Field>
      <Subtitle1 block>{t("settings.mcp.permissions")}</Subtitle1>
      <Field hint={t("settings.mcp.writeHint")}>
        <Switch checked={edit.write} label={t("settings.mcp.write")} onChange={(_, d) => set({ write: d.checked })} />
      </Field>
      <Field hint={t("settings.mcp.writeGlobalHint")}>
        <Switch checked={edit.write && edit.write_global} disabled={!edit.write} label={t("settings.mcp.writeGlobal")} onChange={(_, d) => set({ write_global: d.checked })} />
      </Field>
      <Field hint={t("settings.mcp.runsHint")}>
        <Switch checked={edit.runs} label={t("settings.mcp.runs")} onChange={(_, d) => set({ runs: d.checked })} />
      </Field>
      <div className={styles.row}>
        <Button appearance="primary" disabled={!dirty || !(values.port >= 1024 && values.port <= 65535)} onClick={() => void save()}>
          {t("common.save")}
        </Button>
        <Button disabled={!dirty} onClick={() => { setEdit(pick(status.config)); setPort(String(status.config.port)); }}>
          {t("common.reset")}
        </Button>
      </div>

      <Subtitle1 block>{t("settings.mcp.connect")}</Subtitle1>
      <Field label={t("settings.mcp.token")} hint={t("settings.mcp.tokenHint")}>
        <div className={styles.row}>
          <Input className={styles.grow} readOnly type={show ? "text" : "password"} value={token} placeholder={t("settings.mcp.noToken")} />
          <Button disabled={!token} onClick={() => setShow(!show)}>{show ? t("settings.mcp.hide") : t("settings.mcp.show")}</Button>
          <Button disabled={!token} onClick={() => copy(token)}>{t("settings.mcp.copy")}</Button>
          <Button disabled={!token} onClick={() => void newToken()}>{t("settings.mcp.newToken")}</Button>
        </div>
      </Field>
      <Field label={t("settings.mcp.quickAdd")} hint={ready ? t("settings.mcp.quickAddHint") : t("settings.mcp.quickAddOff")}>
        <div className={styles.row}>
          <Button disabled={!ready} onClick={() => void quickAdd("claude_code")}>{t("settings.mcp.claudeCode")}</Button>
          <Button disabled={!ready} onClick={() => void quickAdd("claude_desktop")}>{t("settings.mcp.claudeDesktop")}</Button>
          <Button disabled={!ready} onClick={() => void quickAdd("codex")}>{t("settings.mcp.codex")}</Button>
        </div>
      </Field>
      {token && (
        <>
          <Field label={t("settings.mcp.claudeCode")}>
            <Textarea readOnly resize="vertical" rows={3} className={styles.mono} value={shown(cli)} />
          </Field>
          <div className={styles.row}>
            <Button onClick={() => copy(cli)}>{t("settings.mcp.copy")}</Button>
          </div>
          <Field label={t("settings.mcp.otherClients")}>
            <Textarea readOnly resize="vertical" rows={9} className={styles.mono} value={shown(mcpJson)} />
          </Field>
          <div className={styles.row}>
            <Button onClick={() => copy(mcpJson)}>{t("settings.mcp.copy")}</Button>
          </div>
        </>
      )}
    </div>
  );
}
