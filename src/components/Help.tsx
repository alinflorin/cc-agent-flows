import { useTranslation } from "react-i18next";
import { Body1, Button, Dialog, DialogActions, DialogBody, DialogContent, DialogSurface, DialogTitle, DialogTrigger, Link, Subtitle2, makeStyles, tokens } from "@fluentui/react-components";

const SECTIONS = ["overview", "projects", "agents", "workflow", "sessions", "security", "start"] as const;

const useStyles = makeStyles({
  surface: { width: "640px", maxWidth: "calc(100vw - 48px)", maxHeight: "calc(100vh - 48px)" },
  content: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM, overflowY: "auto" },
  text: { color: tokens.colorNeutralForeground2 },
});

/** A link that opens the help document in a dialog: the same on the welcome screen and the About page. */
export function HelpLink({ label }: { label?: string }) {
  const { t } = useTranslation();
  const styles = useStyles();
  return (
    <Dialog surfaceMotion={null}>
      <DialogTrigger disableButtonEnhancement>
        <Link as="button" type="button">{label ?? t("help.link")}</Link>
      </DialogTrigger>
      <DialogSurface backdropMotion={null} className={styles.surface}>
        <DialogBody>
          <DialogTitle>{t("help.title")}</DialogTitle>
          <DialogContent className={styles.content}>
            {SECTIONS.map((s) => (
              <section key={s}>
                <Subtitle2 block>{t(`help.${s}.title`)}</Subtitle2>
                <Body1 block className={styles.text}>{t(`help.${s}.text`)}</Body1>
              </section>
            ))}
          </DialogContent>
          <DialogActions>
            <DialogTrigger disableButtonEnhancement>
              <Button appearance="primary">{t("help.close")}</Button>
            </DialogTrigger>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}
