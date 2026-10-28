import type { ReactNode } from "react";
import { DialogSurface, makeStyles, tokens } from "@fluentui/react-components";

const useStyles = makeStyles({
  surface: {
    width: "720px",
    maxWidth: "calc(100vw - 48px)",
    maxHeight: "calc(100vh - 48px)",
    padding: tokens.spacingHorizontalXXL,
    // the title and the buttons stay in view, the form scrolls between them
    "& [class*=fui-DialogBody]": { maxHeight: "calc(100vh - 48px - 2 * " + tokens.spacingHorizontalXXL + ")", gridTemplateRows: "auto minmax(0, 1fr) auto", rowGap: tokens.spacingVerticalL },
    "& [class*=fui-DialogContent]": { overflowY: "auto", paddingRight: tokens.spacingHorizontalM, marginRight: `-${tokens.spacingHorizontalM}` },
    "& [class*=fui-DialogActions]": { paddingTop: tokens.spacingVerticalS, paddingRight: tokens.spacingHorizontalM },
  },
});

/** The surface of a dialog with a form: never taller than the window, the form scrolls and the buttons stay. */
export function FormDialogSurface({ children }: { children: ReactNode }) {
  const styles = useStyles();
  return (
    <DialogSurface backdropMotion={null} className={styles.surface}>
      {children}
    </DialogSurface>
  );
}
