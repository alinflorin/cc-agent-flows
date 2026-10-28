import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Body1, Button, Caption1, Dialog, DialogActions, DialogBody, DialogContent, DialogSurface, DialogTitle, makeStyles, tokens } from "@fluentui/react-components";
import { getPref, inTauri, setPref } from "../prefs";
import { useNotify } from "../notifications";
import { AppearanceFields } from "./AppearanceFields";

const useStyles = makeStyles({ fields: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM, marginTop: tokens.spacingVerticalM } });

/** Shown once, on the first start of the desktop app. Step 1: language and theme. Step 2: the demo content (starter images, agent profiles and the standard workflow). */
export function Welcome() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const [open, setOpen] = useState(() => inTauri() && !getPref<boolean>("welcomed"));
  const [step, setStep] = useState(1);
  const [busy, setBusy] = useState(false);

  const accept = async () => {
    setBusy(true);
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("provision_starters");
      setPref("welcomed", true);
      window.dispatchEvent(new Event("projects-changed"));
      notify(t("welcome.done"), "success");
    } catch (e) {
      // Not remembered: it is offered again on the next start.
      notify(String(e), "error");
    }
    setBusy(false);
    setOpen(false);
  };

  const skip = () => {
    setPref("welcomed", true);
    setOpen(false);
  };

  return (
    <Dialog open={open} modalType="alert" surfaceMotion={null}>
      <DialogSurface backdropMotion={null}>
        <DialogBody>
          <DialogTitle>{step === 1 ? t("welcome.step1Title") : t("welcome.step2Title")}</DialogTitle>
          <DialogContent>
            <Caption1 block>{t("welcome.step", { n: step })}</Caption1>
            {step === 1 ? (
              <>
                <Body1 block>{t("welcome.step1Text")}</Body1>
                <div className={styles.fields}>
                  <AppearanceFields />
                </div>
              </>
            ) : (
              <Body1 block>{t("welcome.text")}</Body1>
            )}
          </DialogContent>
          <DialogActions>
            {step === 1 ? (
              <Button appearance="primary" onClick={() => setStep(2)}>
                {t("welcome.next")}
              </Button>
            ) : (
              <>
                <Button appearance="secondary" disabled={busy} onClick={() => setStep(1)}>
                  {t("welcome.back")}
                </Button>
                <Button appearance="secondary" disabled={busy} onClick={skip}>
                  {t("welcome.no")}
                </Button>
                <Button appearance="primary" disabled={busy} onClick={accept}>
                  {t("welcome.yes")}
                </Button>
              </>
            )}
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}
