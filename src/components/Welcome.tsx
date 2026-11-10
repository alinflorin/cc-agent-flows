import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Body1, Button, Caption1, Dialog, DialogActions, DialogBody, DialogContent, DialogSurface, DialogTitle, makeStyles, tokens } from "@fluentui/react-components";
import { getPref, native, setPref } from "../prefs";
import { AppearanceFields } from "./AppearanceFields";
import { HelpLink } from "./Help";

const useStyles = makeStyles({
  // The buttons wrap instead of leaving the dialog when a translation is long.
  actions: { flexWrap: "wrap", justifyContent: "flex-end", minWidth: 0 },
  fields: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM, marginTop: tokens.spacingVerticalM },
});

/** Shown once, on the first start of the desktop app. Step 1: what the app is, with a link to the help. Step 2: language and theme. */
export function Welcome() {
  const { t } = useTranslation();
  const styles = useStyles();
  const [open, setOpen] = useState(() => native() && !getPref<boolean>("welcomed"));
  const [step, setStep] = useState(1);

  const finish = () => {
    setPref("welcomed", true);
    setOpen(false);
  };

  return (
    <Dialog open={open} modalType="alert" surfaceMotion={null}>
      <DialogSurface backdropMotion={null}>
        <DialogBody>
          <DialogTitle>{t(["welcome.step0Title", "welcome.step1Title"][step - 1])}</DialogTitle>
          <DialogContent>
            <Caption1 block>{t("welcome.step", { n: step })}</Caption1>
            {step === 1 && (
              <>
                <Body1 block>{t("welcome.step0Text")}</Body1>
                <div className={styles.fields}>
                  <HelpLink label={t("welcome.readHelp")} />
                </div>
              </>
            )}
            {step === 2 && (
              <>
                <Body1 block>{t("welcome.step1Text")}</Body1>
                <div className={styles.fields}>
                  <AppearanceFields />
                </div>
              </>
            )}
          </DialogContent>
          <DialogActions className={styles.actions}>
            {step === 1 ? (
              <Button appearance="primary" onClick={() => setStep(2)}>
                {t("welcome.next")}
              </Button>
            ) : (
              <>
                <Button appearance="secondary" onClick={() => setStep(1)}>
                  {t("welcome.back")}
                </Button>
                <Button appearance="primary" onClick={finish}>
                  {t("welcome.finish")}
                </Button>
              </>
            )}
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}
