import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Body1, Button, Dialog, DialogActions, DialogBody, DialogContent, DialogSurface, DialogTitle, makeStyles, tokens } from "@fluentui/react-components";

const useStyles = makeStyles({ empty: { color: tokens.colorNeutralForeground3, marginTop: tokens.spacingVerticalM } });

/** The one "nothing here yet" line, the same on every list. */
export function EmptyState({ children }: { children: ReactNode }) {
  const styles = useStyles();
  return (
    <Body1 block className={styles.empty}>
      {children}
    </Body1>
  );
}

/** Deleting asks first, the same way everywhere: `ask(name, action)` opens the dialog, and `dialog` goes once in the page's JSX. */
export function useConfirmDelete() {
  const { t } = useTranslation();
  const [pending, setPending] = useState<{ name: string; action: () => void | Promise<unknown> } | null>(null);
  const [open, setOpen] = useState(false);
  const ask = (name: string, action: () => void | Promise<unknown>) => {
    setPending({ name, action });
    setOpen(true);
  };
  const dialog = (
    <Dialog surfaceMotion={null} open={open} onOpenChange={(_, d) => setOpen(d.open)}>
      <DialogSurface backdropMotion={null}>
        <DialogBody>
          <DialogTitle>{t("common.deleteTitle")}</DialogTitle>
          <DialogContent>{t("common.deleteText", { name: pending?.name ?? "" })}</DialogContent>
          <DialogActions>
            <Button onClick={() => setOpen(false)}>{t("common.cancel")}</Button>
            <Button
              appearance="primary"
              onClick={() => {
                setOpen(false);
                void pending?.action();
              }}
            >
              {t("common.delete")}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
  return { ask, dialog };
}
