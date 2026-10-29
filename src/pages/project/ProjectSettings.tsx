import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { useTranslation } from "react-i18next";
import {
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  DialogTrigger,
  Field,
  Input,
  Subtitle1,
  Textarea,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { Delete20Regular } from "@fluentui/react-icons";
import { PageHeader } from "../../components/PageHeader";
import { NetworkSettings } from "../settings/Network";
import { inTauri } from "../../prefs";
import { useNotify } from "../../notifications";
import { call } from "../../tauri";
import { useProjectId } from "../../project";

const useStyles = makeStyles({
  form: {
    display: "flex",
    flexDirection: "column",
    gap: tokens.spacingVerticalL,
    maxWidth: "620px",
  },
  section: {
    marginTop: tokens.spacingVerticalXXL,
    paddingTop: tokens.spacingVerticalXL,
    borderTop: `1px solid ${tokens.colorNeutralStroke2}`,
    display: "flex",
    flexDirection: "column",
    gap: tokens.spacingVerticalM,
    maxWidth: "620px",
  },
  actions: { display: "flex", gap: tokens.spacingHorizontalS },
});

interface Values {
  name: string;
  description: string;
  system_prompt: string;
}

/** The project's name, description and system prompt and, at the bottom, deleting it. */
export function ProjectSettings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const navigate = useNavigate();
  const project = useProjectId();
  const desktop = inTauri();
  const [saved, setSaved] = useState<Values | null>(null);
  const [v, setV] = useState<Values | null>(null);
  const [confirm, setConfirm] = useState(false);
  const [saving, setSaving] = useState(false);

  const load = useCallback(async () => {
    try {
      const p = await call<Values>("projects_get", { id: project });
      setSaved(p);
      setV(p);
    } catch (e) {
      notify(String(e), "error");
    }
  }, [project, notify]);

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  const save = async () => {
    if (!v) return;
    setSaving(true);
    try {
      await call("projects_save", {
        id: project,
        name: v.name,
        description: v.description,
        systemPrompt: v.system_prompt,
      });
      notify(t("settings.saved"), "success");
      await load();
      window.dispatchEvent(new Event("projects-changed"));
    } catch (e) {
      notify(String(e), "error");
    }
    setSaving(false);
  };

  const remove = async () => {
    try {
      await call("projects_delete", { id: project });
      navigate("/");
    } catch (e) {
      notify(String(e), "error");
      setConfirm(false);
    }
  };

  const dirty =
    !!v &&
    !!saved &&
    (v.name !== saved.name ||
      v.description !== saved.description ||
      v.system_prompt !== saved.system_prompt);

  return (
    <>
      <PageHeader title={t("settings.tabs.general")} />
      {v && (
        <div className={styles.form}>
          <Field label={t("profiles.name")} required>
            <Input
              value={v.name}
              maxLength={80}
              onChange={(_, d) => setV({ ...v, name: d.value })}
            />
          </Field>
          <Field label={t("profiles.description")}>
            <Textarea
              rows={3}
              value={v.description}
              onChange={(_, d) => setV({ ...v, description: d.value })}
            />
          </Field>
          <Field
            label={t("projects.systemPrompt")}
            hint={t("projects.systemPromptHint")}
          >
            <Textarea
              rows={8}
              resize="vertical"
              value={v.system_prompt}
              onChange={(_, d) => setV({ ...v, system_prompt: d.value })}
            />
          </Field>
          <div className={styles.actions}>
            <Button
              appearance="primary"
              disabled={saving || !dirty || !v.name.trim()}
              onClick={() => void save()}
            >
              {t("common.save")}
            </Button>
            <Button disabled={!dirty} onClick={() => setV(saved)}>
              {t("common.reset")}
            </Button>
          </div>
        </div>
      )}

      <div className={styles.section}>
        <Subtitle1 block>{t("projects.danger")}</Subtitle1>
        <div className={styles.actions}>
          <Dialog
            surfaceMotion={null}
            open={confirm}
            onOpenChange={(_, d) => setConfirm(d.open)}
          >
            <DialogTrigger disableButtonEnhancement>
              <Button icon={<Delete20Regular />}>{t("projects.delete")}</Button>
            </DialogTrigger>
            <DialogSurface backdropMotion={null}>
              <DialogBody>
                <DialogTitle>{t("projects.delete")}</DialogTitle>
                <DialogContent>{t("projects.deleteText")}</DialogContent>
                <DialogActions>
                  <Button onClick={() => setConfirm(false)}>
                    {t("common.cancel")}
                  </Button>
                  <Button appearance="primary" onClick={() => void remove()}>
                    {t("projects.deleteConfirm")}
                  </Button>
                </DialogActions>
              </DialogBody>
            </DialogSurface>
          </Dialog>
        </div>
      </div>
    </>
  );
}

/** The project's allowed hosts. */
export function ProjectNetwork() {
  const { t } = useTranslation();
  return (
    <>
      <PageHeader title={t("settings.tabs.network")} />
      <NetworkSettings />
    </>
  );
}
