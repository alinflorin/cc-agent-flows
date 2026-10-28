import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Dialog, DialogActions, DialogBody, DialogContent, DialogSurface, DialogTitle, DialogTrigger } from "@fluentui/react-components";
import { ArrowReset20Regular } from "@fluentui/react-icons";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";

/** A small button that puts one kind of demo content back (what the welcome dialog creates), after a confirmation that says what is lost. `what` is the key under `reset.` in the translations. */
export function ResetDemoButton({ command, what, onDone }: { command: string; what: "images" | "profiles" | "secrets" | "variables" | "injections"; onDone?: () => void }) {
  const { t } = useTranslation();
  const notify = useNotify();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);

  const run = async () => {
    setBusy(true);
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke(command);
      notify(t("reset.done"), "success");
      onDone?.();
      setOpen(false);
    } catch (e) {
      notify(String(e), "error");
    }
    setBusy(false);
  };

  return (
    <Dialog open={open} onOpenChange={(_, d) => setOpen(d.open)}>
      <DialogTrigger disableButtonEnhancement>
        <Button size="small" appearance="subtle" icon={<ArrowReset20Regular />} disabled={!inTauri()}>
          {t("reset.button")}
        </Button>
      </DialogTrigger>
      <DialogSurface>
        <DialogBody>
          <DialogTitle>{t("reset.title")}</DialogTitle>
          <DialogContent>{t(`reset.${what}`)}</DialogContent>
          <DialogActions>
            <DialogTrigger disableButtonEnhancement>
              <Button disabled={busy}>{t("common.cancel")}</Button>
            </DialogTrigger>
            <Button appearance="primary" disabled={busy} onClick={() => void run()}>
              {t("reset.confirm")}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}
