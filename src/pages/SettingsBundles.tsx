import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Body1, Button, Caption1, Checkbox, MessageBar, MessageBarBody, Subtitle1, makeStyles, tokens } from "@fluentui/react-components";
import { ArrowDownload20Regular, ArrowUpload20Regular } from "@fluentui/react-icons";
import { SettingsIntro } from "../components/SettingsIntro";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";

interface Item {
  id: string;
  name: string;
}
interface Report {
  added: string[];
  replaced: string[];
  skipped: string[];
  errors: string[];
}
/** What a bundle file holds, read here to show the user before anything is imported. */
interface Peek {
  text: string;
  workflows: number;
  profiles: number;
  images: number;
  scripts: number;
  hosts: string[];
}

const useStyles = makeStyles({
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalL, maxWidth: "640px" },
  group: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalXS },
  cols: { display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))", gap: tokens.spacingHorizontalL },
  actions: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "center", flexWrap: "wrap" },
  mono: { fontFamily: tokens.fontFamilyMonospace, overflowWrap: "anywhere" },
  muted: { color: tokens.colorNeutralForeground3 },
});

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

function peek(text: string): Peek {
  const b = JSON.parse(text) as { format?: string; workflows?: unknown[]; profiles?: { profile: { allow_hosts?: string[] } }[]; images?: { spec: { script?: string } }[] };
  if (b.format !== "ccaf-bundle") throw new Error("not a bundle");
  return {
    text,
    workflows: b.workflows?.length ?? 0,
    profiles: b.profiles?.length ?? 0,
    images: b.images?.length ?? 0,
    scripts: (b.images ?? []).filter((i) => (i.spec.script ?? "").trim()).length,
    hosts: [...new Set((b.profiles ?? []).flatMap((p) => p.profile.allow_hosts ?? []))],
  };
}

/** Settings > Bundles: share workflows, agent profiles and images as one file, and import such a file. */
export function BundlesSettings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();
  const file = useRef<HTMLInputElement>(null);
  const [all, setAll] = useState<{ workflows: Item[]; profiles: Item[]; images: Item[] }>({ workflows: [], profiles: [], images: [] });
  const [pick, setPick] = useState<{ workflows: string[]; profiles: string[]; images: string[] }>({ workflows: [], profiles: [], images: [] });
  const [deps, setDeps] = useState(true);
  const [saved, setSaved] = useState("");
  const [incoming, setIncoming] = useState<Peek | null>(null);
  const [overwrite, setOverwrite] = useState(false);
  const [report, setReport] = useState<Report | null>(null);

  const load = useCallback(async () => {
    try {
      const [w, p, i] = await Promise.all([call<Item[]>("workflows_list"), call<Item[]>("profiles_list"), call<(Item & { builtin: boolean })[]>("images_list")]);
      setAll({ workflows: w, profiles: p, images: i.filter((x) => !x.builtin) });
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify]);
  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  const toggle = (k: "workflows" | "profiles" | "images", id: string, on: boolean) => setPick((p) => ({ ...p, [k]: on ? [...p[k], id] : p[k].filter((x) => x !== id) }));
  const nothing = pick.workflows.length + pick.profiles.length + pick.images.length === 0;

  const exportIt = async () => {
    try {
      setSaved(await call<string>("bundle_export", { selection: { ...pick, with_dependencies: deps } }));
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const choose = async (f: File | undefined) => {
    setReport(null);
    if (!f) return;
    try {
      setIncoming(peek(await f.text()));
    } catch {
      setIncoming(null);
      notify(t("settings.bundles.notBundle"), "error");
    }
  };

  const importIt = async () => {
    if (!incoming) return;
    try {
      setReport(await call<Report>("bundle_import", { text: incoming.text, overwrite }));
      setIncoming(null);
      if (file.current) file.current.value = "";
      await load();
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const groups = [["workflows", all.workflows], ["profiles", all.profiles], ["images", all.images]] as const;
  return (
    <div className={styles.form}>
      <SettingsIntro text={t("settings.bundles.intro")}>{!desktop && <Body1 block>{t("settings.llm.tokenDesktopOnly")}</Body1>}</SettingsIntro>

      <Subtitle1>{t("settings.bundles.export")}</Subtitle1>
      <div className={styles.cols}>
        {groups.map(([k, items]) => (
          <div key={k} className={styles.group}>
            <Caption1><b>{t(`settings.bundles.${k}`)}</b></Caption1>
            {items.length === 0 && <Caption1 className={styles.muted}>–</Caption1>}
            {items.map((i) => (
              <Checkbox key={i.id} label={i.name} checked={pick[k].includes(i.id)} onChange={(_, d) => toggle(k, i.id, !!d.checked)} />
            ))}
          </div>
        ))}
      </div>
      <Checkbox label={t("settings.bundles.deps")} checked={deps} onChange={(_, d) => setDeps(!!d.checked)} />
      <Caption1 className={styles.muted}>{t("settings.bundles.exportNote")}</Caption1>
      <div className={styles.actions}>
        <Button appearance="primary" icon={<ArrowDownload20Regular />} disabled={!desktop || nothing} onClick={() => void exportIt()}>
          {t("settings.bundles.exportButton")}
        </Button>
        {saved && <Caption1 className={styles.mono}>{t("settings.bundles.savedTo", { path: saved })}</Caption1>}
      </div>

      <Subtitle1>{t("settings.bundles.import")}</Subtitle1>
      <input ref={file} type="file" accept=".json,application/json" disabled={!desktop} onChange={(e) => void choose(e.target.files?.[0])} />
      {incoming && (
        <>
          <MessageBar intent="warning">
            <MessageBarBody>
              <div>{t("settings.bundles.contains", { workflows: incoming.workflows, profiles: incoming.profiles, images: incoming.images })}</div>
              {incoming.scripts > 0 && <div>{t("settings.bundles.warnScripts", { count: incoming.scripts })}</div>}
              {incoming.hosts.length > 0 && <div>{t("settings.bundles.warnHosts", { hosts: incoming.hosts.join(", ") })}</div>}
              <div>{t("settings.bundles.warnTrust")}</div>
            </MessageBarBody>
          </MessageBar>
          <Checkbox label={t("settings.bundles.overwrite")} checked={overwrite} onChange={(_, d) => setOverwrite(!!d.checked)} />
          <div className={styles.actions}>
            <Button appearance="primary" icon={<ArrowUpload20Regular />} onClick={() => void importIt()}>
              {t("settings.bundles.importButton")}
            </Button>
            <Button onClick={() => { setIncoming(null); if (file.current) file.current.value = ""; }}>{t("common.cancel")}</Button>
          </div>
        </>
      )}
      {report && (
        <MessageBar intent={report.errors.length ? "error" : "success"}>
          <MessageBarBody>
            <div>{t("settings.bundles.result", { added: report.added.length, replaced: report.replaced.length, skipped: report.skipped.length })}</div>
            {report.errors.map((e) => <div key={e}>{e}</div>)}
            {report.added.some((a) => a.startsWith("image ")) && <div>{t("settings.bundles.buildNote")}</div>}
          </MessageBarBody>
        </MessageBar>
      )}
    </div>
  );
}
