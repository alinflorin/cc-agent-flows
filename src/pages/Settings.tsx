import { useState } from "react";
import { Controller, useForm } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { z } from "zod";
import { PageHeader } from "../components/PageHeader";
import { useTranslation } from "react-i18next";
import { Button, Subtitle1, Switch, Tab, TabList, makeStyles, tokens } from "@fluentui/react-components";
import { useNotify } from "../notifications";
import { getPref, setPref } from "../prefs";
import { useUpdates } from "../updates";
import { AppearanceFields } from "../components/AppearanceFields";
import { BundlesSettings } from "./SettingsBundles";
import { ImagesSettings } from "./SettingsImages";
import { LlmSettings } from "./SettingsLlm";
import { NetworkSettings } from "./SettingsNetwork";
import { SecretsSettings } from "./SettingsSecrets";
import { VariablesSettings } from "./SettingsVariables";
import { InjectionsSettings } from "./SettingsInjections";

const useStyles = makeStyles({
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalL, maxWidth: "480px" },
  section: { marginTop: tokens.spacingVerticalXXL, paddingTop: tokens.spacingVerticalXL, borderTop: `1px solid ${tokens.colorNeutralStroke2}`, display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM, maxWidth: "480px" },
  tabs: { marginBottom: tokens.spacingVerticalXL, marginLeft: `-${tokens.spacingHorizontalM}` },
  actions: { display: "flex", gap: tokens.spacingHorizontalS },
});

function GeneralSettings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const updates = useUpdates();

  const schema = z.object({ autoUpdate: z.boolean(), notifications: z.boolean() });
  type Values = z.infer<typeof schema>;

  const {
    control,
    handleSubmit,
    reset,
    formState: { isDirty },
  } = useForm<Values>({
    resolver: zodResolver(schema),
    defaultValues: { autoUpdate: updates.autoUpdate, notifications: getPref<boolean>("notifications") !== false },
  });

  const onSubmit = (v: Values) => {
    updates.setAutoUpdate(v.autoUpdate);
    setPref("notifications", v.notifications);
    reset(v);
    notify(t("settings.saved"), "success");
  };

  return (
    <>
      <div className={styles.form}>
        <AppearanceFields />
      </div>
      <form className={styles.form} onSubmit={handleSubmit(onSubmit)} noValidate style={{ marginTop: 16 }}>
        <Controller
          control={control}
          name="autoUpdate"
          render={({ field }) => (
            <Switch
              label={t("settings.autoUpdate")}
              checked={field.value}
              onChange={(_, d) => field.onChange(d.checked)}
            />
          )}
        />
        <Controller
          control={control}
          name="notifications"
          render={({ field }) => (
            <Switch label={t("settings.notifications")} checked={field.value} onChange={(_, d) => field.onChange(d.checked)} />
          )}
        />
        <div className={styles.actions}>
          <Button appearance="primary" type="submit" disabled={!isDirty}>
            {t("common.save")}
          </Button>
          <Button type="button" disabled={!isDirty} onClick={() => reset()}>
            {t("common.reset")}
          </Button>
        </div>
      </form>

      <div className={styles.section}>
        <Subtitle1 block>{t("updates.title")}</Subtitle1>
        <div className={styles.actions}>
          {updates.ready ? (
            <Button appearance="primary" onClick={updates.restartToUpdate}>
              {t("updates.restart", { version: updates.ready })}
            </Button>
          ) : (
            <Button disabled={updates.checking} onClick={() => void updates.checkForUpdates()}>
              {t("updates.check")}
            </Button>
          )}
        </div>
      </div>
    </>
  );
}

export function Settings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const [tab, setTab] = useState("general");

  return (
    <>
      <PageHeader title={t("settings.title")} />
      <TabList className={styles.tabs} selectedValue={tab} onTabSelect={(_, d) => setTab(d.value as string)}>
        <Tab value="general">{t("settings.tabs.general")}</Tab>
        <Tab value="llm">{t("settings.tabs.llm")}</Tab>
        <Tab value="images">{t("settings.tabs.images")}</Tab>
        <Tab value="network">{t("settings.tabs.network")}</Tab>
        <Tab value="secrets">{t("settings.tabs.secrets")}</Tab>
        <Tab value="variables">{t("settings.tabs.variables")}</Tab>
        <Tab value="injections">{t("settings.tabs.injections")}</Tab>
        <Tab value="bundles">{t("settings.tabs.bundles")}</Tab>
      </TabList>
      {tab === "general" ? <GeneralSettings /> : tab === "llm" ? <LlmSettings /> : tab === "network" ? <NetworkSettings /> : tab === "secrets" ? <SecretsSettings /> : tab === "variables" ? <VariablesSettings /> : tab === "injections" ? <InjectionsSettings /> : tab === "bundles" ? <BundlesSettings /> : <ImagesSettings />}
    </>
  );
}
