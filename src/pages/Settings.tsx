import type { ReactElement } from "react";
import { useParams } from "react-router";
import { Controller, useForm } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { z } from "zod";
import { PageHeader } from "../components/PageHeader";
import { useTranslation } from "react-i18next";
import { Button, Subtitle1, Switch, makeStyles, tokens } from "@fluentui/react-components";
import { useNotify } from "../notifications";
import { getPref, setPref } from "../prefs";
import { useUpdates } from "../updates";
import { AppearanceFields } from "../components/AppearanceFields";
import { ImagesSettings } from "./settings/Images";
import { LlmSettings } from "./settings/Llm";
import { NetworkSettings } from "./settings/Network";
import { SecretsSettings } from "./settings/Secrets";
import { VariablesSettings } from "./settings/Variables";
import { InjectionsSettings } from "./settings/Injections";

const useStyles = makeStyles({
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalL, maxWidth: "480px" },
  section: { marginTop: tokens.spacingVerticalXXL, paddingTop: tokens.spacingVerticalXL, borderTop: `1px solid ${tokens.colorNeutralStroke2}`, display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM, maxWidth: "480px" },
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

/** One page of the global settings; the sidebar picks it (`/settings/:tab`). */
export function Settings() {
  const { t } = useTranslation();
  const { tab = "general" } = useParams();
  const pages: Record<string, ReactElement> = {
    general: <GeneralSettings />,
    llm: <LlmSettings />,
    images: <ImagesSettings />,
    network: <NetworkSettings />,
    secrets: <SecretsSettings />,
    variables: <VariablesSettings />,
    injections: <InjectionsSettings />,
  };
  const title = tab === "general" || tab === "llm" ? t(`settings.tabs.${tab}`) : t(`settings.globalTabs.${tab}`);

  return (
    <>
      <PageHeader title={title} description={t("settings.globalIntro")} />
      {pages[tab] ?? pages.general}
    </>
  );
}
