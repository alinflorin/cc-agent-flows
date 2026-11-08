import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Badge, Body1, Button, Field, Input, MessageBar, MessageBarBody, Subtitle2, Switch, makeStyles, tokens } from "@fluentui/react-components";
import { Delete16Regular } from "@fluentui/react-icons";
import { SettingsIntro } from "../../components/SettingsIntro";
import { inTauri } from "../../prefs";
import { useNotify } from "../../notifications";
import { call } from "../../tauri";

interface NodeView {
  id: string;
  name: string;
  addr: string;
  fingerprint: string;
  enabled: boolean;
  slots: number;
  running: number;
  online: boolean;
  error: string | null;
  version: string | null;
  arch: string | null;
  os: string | null;
}

interface HubView {
  local: boolean;
  local_slots: number;
  local_running: number;
  nodes: NodeView[];
}

interface RoleView {
  enabled: boolean;
  bind: string;
  port: number;
  key: string;
  fingerprint: string;
  forced: boolean;
  error: string | null;
}

const useStyles = makeStyles({
  page: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalXL, maxWidth: "720px" },
  section: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM },
  row: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "center", flexWrap: "wrap" },
  node: {
    display: "flex",
    flexDirection: "column",
    gap: tokens.spacingVerticalXS,
    padding: tokens.spacingVerticalM,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    borderRadius: tokens.borderRadiusMedium,
  },
  grow: { flexGrow: 1 },
  mono: { fontFamily: tokens.fontFamilyMonospace },
  slots: { width: "88px" },
});

/** Settings > Nodes: the computers that run this app's VMs, and this computer as one of them. */
export function NodesSettings() {
  const { t } = useTranslation();
  const desktop = inTauri();
  if (!desktop) return <Body1 block>{t("settings.llm.tokenDesktopOnly")}</Body1>;
  return <Nodes />;
}

function Nodes() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const [hub, setHub] = useState<HubView | null>(null);

  const load = useCallback(async () => {
    try {
      setHub(await call<HubView>("nodes_get"));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify]);

  useEffect(() => {
    void load();
    // the state of the nodes changes by itself (checked every 10 s, VMs start and end)
    const timer = setInterval(() => void load(), 4000);
    return () => clearInterval(timer);
  }, [load]);

  const run = async (f: () => Promise<unknown>) => {
    try {
      await f();
      await load();
    } catch (e) {
      notify(String(e), "error");
    }
  };

  if (!hub) return null;

  const change = (n: NodeView, patch: Partial<{ enabled: boolean; slots: number }>) =>
    run(() => call("nodes_save", { id: n.id, name: n.name, addr: n.addr, key: null, fingerprint: n.fingerprint, enabled: n.enabled, slots: n.slots, ...patch }));

  return (
    <div className={styles.page}>
      <div className={styles.section}>
        <SettingsIntro text={t("settings.nodes.intro")} />
        <Subtitle2>{t("settings.nodes.hubTitle")}</Subtitle2>
        <div className={styles.row}>
          <Switch checked={hub.local} label={t("settings.nodes.local")} onChange={(_, d) => void run(() => call("nodes_set_local", { local: d.checked }))} />
          <Badge appearance="tint" color="informative">
            {t("settings.nodes.inUse", { running: hub.local_running, slots: hub.local_slots })}
          </Badge>
        </div>
        {hub.nodes.map((n) => (
          <div key={n.id} className={styles.node}>
            <div className={styles.row}>
              <Body1 className={styles.grow}>
                <b>{n.name}</b> <span className={styles.mono}>{n.addr}</span>
              </Body1>
              {n.enabled ? (
                <Badge appearance="filled" color={n.online && !n.error ? "success" : "danger"}>
                  {n.online ? (n.error ? t("settings.nodes.unusable") : t("settings.nodes.online")) : t("settings.nodes.offline")}
                </Badge>
              ) : (
                <Badge appearance="outline">{t("settings.nodes.off")}</Badge>
              )}
              <Button appearance="subtle" icon={<Delete16Regular />} aria-label={t("common.delete")} onClick={() => void run(() => call("nodes_delete", { id: n.id }))} />
            </div>
            <div className={styles.row}>
              <Switch checked={n.enabled} label={t("settings.nodes.enabled")} onChange={(_, d) => void change(n, { enabled: d.checked })} />
              <Field label={t("settings.nodes.slots")} orientation="horizontal">
                <Input
                  className={styles.slots}
                  type="number"
                  min={1}
                  max={32}
                  defaultValue={String(n.slots)}
                  key={`${n.id}-${n.slots}`}
                  onBlur={(e) => {
                    const v = Number(e.target.value);
                    if (v >= 1 && v <= 32 && v !== n.slots) void change(n, { slots: v });
                  }}
                />
              </Field>
              <Badge appearance="tint" color="informative">
                {t("settings.nodes.inUse", { running: n.running, slots: n.slots })}
              </Badge>
              {n.version && (
                <Body1>
                  {n.os} {n.arch} · {n.version}
                </Body1>
              )}
            </div>
            {n.enabled && n.error && (
              <MessageBar intent="error">
                <MessageBarBody>{n.error}</MessageBarBody>
              </MessageBar>
            )}
          </div>
        ))}
        <AddNode onAdded={load} />
      </div>
      <Role />
    </div>
  );
}

function AddNode({ onAdded }: { onAdded: () => Promise<void> }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [addr, setAddr] = useState("");
  const [key, setKey] = useState("");
  const [fingerprint, setFingerprint] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const reset = () => {
    setOpen(false);
    setName("");
    setAddr("");
    setKey("");
    setFingerprint(null);
  };

  const check = async () => {
    setBusy(true);
    try {
      setFingerprint(await call<string>("nodes_probe", { addr: addr.trim() }));
    } catch (e) {
      notify(String(e), "error");
    } finally {
      setBusy(false);
    }
  };

  const add = async () => {
    setBusy(true);
    try {
      await call("nodes_save", { id: null, name, addr: addr.trim(), key, fingerprint, enabled: true, slots: 2 });
      reset();
      await onAdded();
    } catch (e) {
      notify(String(e), "error");
    } finally {
      setBusy(false);
    }
  };

  if (!open) {
    return (
      <div>
        <Button onClick={() => setOpen(true)}>{t("settings.nodes.add")}</Button>
      </div>
    );
  }
  return (
    <div className={styles.node}>
      <Field label={t("settings.nodes.name")}>
        <Input value={name} onChange={(_, d) => setName(d.value)} />
      </Field>
      <Field label={t("settings.nodes.address")} hint={t("settings.nodes.addressHint")}>
        <Input
          value={addr}
          onChange={(_, d) => {
            setAddr(d.value);
            setFingerprint(null);
          }}
        />
      </Field>
      <Field label={t("settings.nodes.key")} hint={t("settings.nodes.keyHint")}>
        <Input type="password" autoComplete="off" value={key} onChange={(_, d) => setKey(d.value)} />
      </Field>
      {fingerprint ? (
        <Field label={t("settings.nodes.fingerprint")} hint={t("settings.nodes.fingerprintHint")}>
          <Input readOnly className={styles.mono} value={fingerprint} />
        </Field>
      ) : null}
      <div className={styles.row}>
        {fingerprint ? (
          <Button appearance="primary" disabled={busy || !name.trim() || !key.trim()} onClick={() => void add()}>
            {t("settings.nodes.trust")}
          </Button>
        ) : (
          <Button appearance="primary" disabled={busy || !addr.trim()} onClick={() => void check()}>
            {t("settings.nodes.check")}
          </Button>
        )}
        <Button onClick={reset}>{t("common.cancel")}</Button>
      </div>
    </div>
  );
}

/** This computer as a node for other computers. */
function Role() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const [role, setRole] = useState<RoleView | null>(null);
  const [enabled, setEnabled] = useState(false);
  const [bind, setBind] = useState("");
  const [port, setPort] = useState("");

  const take = (r: RoleView) => {
    setRole(r);
    setEnabled(r.enabled);
    setBind(r.bind);
    setPort(String(r.port));
  };

  useEffect(() => {
    call<RoleView>("node_get").then(take).catch((e) => notify(String(e), "error"));
  }, [notify]);

  if (!role) return null;
  const dirty = enabled !== role.enabled || bind !== role.bind || port !== String(role.port);
  const p = Number(port);
  const valid = p >= 1024 && p <= 65535 && !!bind.trim();

  const save = async () => {
    try {
      take(await call<RoleView>("node_set", { enabled, bind: bind.trim(), port: p }));
      notify(t("settings.saved"), "success");
      // the listener restarts a moment later: ask again for a busy port
      setTimeout(() => void call<RoleView>("node_get").then(setRole).catch(() => {}), 1200);
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const newKey = async () => {
    try {
      setRole(await call<RoleView>("node_new_key"));
    } catch (e) {
      notify(String(e), "error");
    }
  };

  return (
    <div className={styles.section}>
      <Subtitle2>{t("settings.nodes.roleTitle")}</Subtitle2>
      <Body1>{t("settings.nodes.roleIntro")}</Body1>
      {role.error && (
        <MessageBar intent="error">
          <MessageBarBody>{role.error}</MessageBarBody>
        </MessageBar>
      )}
      {role.forced && (
        <MessageBar intent="info">
          <MessageBarBody>{t("settings.nodes.forced")}</MessageBarBody>
        </MessageBar>
      )}
      <Switch checked={enabled} label={t("settings.nodes.accept")} onChange={(_, d) => setEnabled(d.checked)} />
      <Field label={t("settings.nodes.bind")} hint={t("settings.nodes.bindHint")}>
        <Input value={bind} onChange={(_, d) => setBind(d.value)} />
      </Field>
      <Field label={t("settings.nodes.port")}>
        <Input type="number" min={1024} max={65535} value={port} onChange={(_, d) => setPort(d.value)} />
      </Field>
      <div className={styles.row}>
        <Button appearance="primary" disabled={!dirty || !valid} onClick={() => void save()}>
          {t("common.save")}
        </Button>
        <Button disabled={!dirty} onClick={() => take(role)}>
          {t("common.reset")}
        </Button>
      </div>
      <Field label={t("settings.nodes.key")} hint={t("settings.nodes.yourKeyHint")}>
        <div className={styles.row}>
          <Input readOnly className={`${styles.mono} ${styles.grow}`} value={role.key} />
          <Button onClick={() => void navigator.clipboard?.writeText(role.key)}>{t("common.copy")}</Button>
          <Button onClick={() => void newKey()}>{t("settings.nodes.newKey")}</Button>
        </div>
      </Field>
      <Field label={t("settings.nodes.fingerprint")} hint={t("settings.nodes.yourFingerprintHint")}>
        <Input readOnly className={styles.mono} value={role.fingerprint} />
      </Field>
    </div>
  );
}
