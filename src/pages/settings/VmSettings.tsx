import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Dropdown, Field, Input, Option, Switch, makeStyles, tokens } from "@fluentui/react-components";
import { inTauri } from "../../prefs";
import { useNotify } from "../../notifications";
import { call } from "../../tauri";

type Accel = "auto" | "hardware" | "software";

export interface VmSettings {
  default_cpus: number | null;
  default_memory_mb: number;
  max_vms: number;
  max_vms_per_session: number;
  accel: Accel;
  run_grace_secs: number;
  image_script_mins: number;
  builder_cpus: number;
  builder_memory_mb: number;
  confine: boolean;
}

const useStyles = makeStyles({
  section: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM, maxWidth: "480px" },
  pair: { display: "grid", gridTemplateColumns: "1fr 1fr", alignItems: "start", gap: tokens.spacingHorizontalM },
  actions: { display: "flex", gap: tokens.spacingHorizontalS },
});

const same = (a: VmSettings, b: VmSettings) => JSON.stringify(a) === JSON.stringify(b);

/** How the app starts virtual machines on this computer. The network and the CPU model are not here: they keep the guest sealed and bootable. */
export function VmSettingsSection() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();
  const [saved, setSaved] = useState<VmSettings | null>(null);
  const [v, setV] = useState<VmSettings | null>(null);

  const load = useCallback(async () => {
    try {
      const s = await call<VmSettings>("vm_settings_get");
      setSaved(s);
      setV(s);
    } catch (e) {
      notify(String(e), "error");
    }
  }, [notify]);

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  if (!desktop || !v || !saved) return null;
  const dirty = !same(v, saved);
  const set = (patch: Partial<VmSettings>) => setV({ ...v, ...patch });
  const num = (key: keyof VmSettings, min: number, max: number) => ({
    type: "number" as const,
    min,
    max,
    value: String(v[key] ?? ""),
    onChange: (_: unknown, d: { value: string }) => set({ [key]: Math.min(max, Math.max(min, Math.floor(Number(d.value)) || min)) } as Partial<VmSettings>),
  });

  const save = async () => {
    try {
      const s = await call<VmSettings>("vm_settings_set", { settings: v });
      setSaved(s);
      setV(s);
      notify(t("settings.saved"), "success");
    } catch (e) {
      notify(String(e), "error");
    }
  };

  return (
    <div className={styles.section}>
      <Field label={t("settings.vm.accel")} hint={t(`settings.vm.accelHint.${v.accel}`)}>
        <Dropdown value={t(`settings.vm.accels.${v.accel}`)} selectedOptions={[v.accel]} onOptionSelect={(_, d) => set({ accel: (d.optionValue ?? "auto") as Accel })}>
          {(["auto", "hardware", "software"] as const).map((a) => (
            <Option key={a} value={a}>
              {t(`settings.vm.accels.${a}`)}
            </Option>
          ))}
        </Dropdown>
      </Field>
      <div className={styles.pair}>
        <Field label={t("settings.vm.defaultCpus")} hint={t("settings.vm.defaultCpusHint")}>
          <Input
            type="number"
            min={1}
            max={64}
            placeholder={t("settings.vm.automatic")}
            value={v.default_cpus === null ? "" : String(v.default_cpus)}
            onChange={(_, d) => set({ default_cpus: d.value === "" ? null : Math.min(64, Math.max(1, Math.floor(Number(d.value)) || 1)) })}
          />
        </Field>
        <Field label={t("settings.vm.defaultMemory")} hint={t("settings.vm.defaultMemoryHint")}>
          <Input {...num("default_memory_mb", 1024, 65536)} step={512} />
        </Field>
      </div>
      <div className={styles.pair}>
        <Field label={t("settings.vm.maxVms")} hint={t("settings.vm.maxVmsHint")}>
          <Input {...num("max_vms", 1, 32)} />
        </Field>
        <Field label={t("settings.vm.maxPerSession")}>
          <Input {...num("max_vms_per_session", 1, 16)} />
        </Field>
      </div>
      <div className={styles.pair}>
        <Field label={t("settings.vm.grace")} hint={t("settings.vm.graceHint")}>
          <Input {...num("run_grace_secs", 60, 3600)} step={30} />
        </Field>
        <Field label={t("settings.vm.imageScript")} hint={t("settings.vm.imageScriptHint")}>
          <Input {...num("image_script_mins", 5, 240)} />
        </Field>
      </div>
      <div className={styles.pair}>
        <Field label={t("settings.vm.builderCpus")} hint={t("settings.vm.builderHint")}>
          <Input {...num("builder_cpus", 1, 16)} />
        </Field>
        <Field label={t("settings.vm.builderMemory")}>
          <Input {...num("builder_memory_mb", 1024, 32768)} step={512} />
        </Field>
      </div>
      <Field hint={t("settings.vm.confineHint")}>
        <Switch checked={v.confine} label={t("settings.vm.confine")} onChange={(_, d) => set({ confine: d.checked })} />
      </Field>
      <div className={styles.actions}>
        <Button appearance="primary" disabled={!dirty} onClick={() => void save()}>
          {t("common.save")}
        </Button>
        <Button disabled={!dirty} onClick={() => setV(saved)}>
          {t("common.reset")}
        </Button>
      </div>
    </div>
  );
}
