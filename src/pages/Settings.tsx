import { useMemo } from "react";
import { Controller, useForm } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { z } from "zod";
import { useTranslation } from "react-i18next";
import { Button, Field, Input, Subtitle1, Switch, Title2, makeStyles, tokens } from "@fluentui/react-components";
import { getPref, setPref } from "../prefs";
import { useNotify } from "../notifications";
import { useUpdates } from "../updates";

const NAME_MAX = 40;

const useStyles = makeStyles({
  form: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalL, maxWidth: "480px" },
  section: { marginTop: tokens.spacingVerticalXL },
  actions: { display: "flex", gap: tokens.spacingHorizontalS },
});

export function Settings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const updates = useUpdates();

  // Rebuilt when the language changes so the messages follow it.
  const schema = useMemo(
    () =>
      z.object({
        displayName: z
          .string()
          .trim()
          .min(1, t("validation.required"))
          .max(NAME_MAX, t("validation.max", { max: NAME_MAX })),
        autoUpdate: z.boolean(),
      }),
    [t],
  );
  type Values = z.infer<typeof schema>;

  const {
    control,
    handleSubmit,
    reset,
    formState: { errors, isDirty },
  } = useForm<Values>({
    resolver: zodResolver(schema),
    defaultValues: { displayName: getPref<string>("displayName") ?? "", autoUpdate: updates.autoUpdate },
  });

  const onSubmit = (v: Values) => {
    setPref("displayName", v.displayName);
    updates.setAutoUpdate(v.autoUpdate);
    reset(v);
    notify(t("settings.saved"), "success");
  };

  return (
    <>
      <Title2 as="h1" block>
        {t("settings.title")}
      </Title2>
      <form className={styles.form} onSubmit={handleSubmit(onSubmit)} noValidate>
        <Field
          label={t("settings.displayName")}
          validationState={errors.displayName ? "error" : "none"}
          validationMessage={errors.displayName?.message}
        >
          <Controller
            control={control}
            name="displayName"
            render={({ field }) => (
              <Input {...field} onChange={(_, d) => field.onChange(d.value)} />
            )}
          />
        </Field>
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
