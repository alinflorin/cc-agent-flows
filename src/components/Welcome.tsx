import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Body1, Button, Dialog, DialogActions, DialogBody, DialogContent, DialogSurface, DialogTitle } from "@fluentui/react-components";
import { getPref, inTauri, setPref } from "../prefs";
import { useNotify } from "../notifications";

/** Shown once, on the first start of the desktop app: offers the starter images and agent profiles. */
export function Welcome() {
  const { t } = useTranslation();
  const notify = useNotify();
  const [open, setOpen] = useState(() => inTauri() && !getPref<boolean>("welcomed"));
  const [busy, setBusy] = useState(false);

  const accept = async () => {
    setBusy(true);
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("provision_starters");
      setPref("welcomed", true);
      notify(t("welcome.done"), "success");
    } catch (e) {
      // Not remembered: it is offered again on the next start.
      notify(String(e), "error");
    }
    setBusy(false);
    setOpen(false);
  };

  const decline = () => {
    setPref("welcomed", true);
    setOpen(false);
  };

  return (
    <Dialog open={open} modalType="alert">
      <DialogSurface>
        <DialogBody>
          <DialogTitle>{t("welcome.title")}</DialogTitle>
          <DialogContent>
            <Body1>{t("welcome.text")}</Body1>
          </DialogContent>
          <DialogActions>
            <Button appearance="secondary" disabled={busy} onClick={decline}>
              {t("welcome.no")}
            </Button>
            <Button appearance="primary" disabled={busy} onClick={accept}>
              {t("welcome.yes")}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}
