import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Body1, Button, Checkbox, Field, Input, MessageBar, MessageBarBody, Subtitle1, makeStyles, tokens } from "@fluentui/react-components";
import { ArrowDownload20Regular, ArrowUpload20Regular } from "@fluentui/react-icons";
import { inTauri } from "../../prefs";
import { useNotify } from "../../notifications";
import { call } from "../../tauri";

const MIN_PASSWORD = 8;

interface Summary {
  written: number;
  skipped: number;
  secrets: number;
  tokens: number;
}

const useStyles = makeStyles({
  page: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalXL, maxWidth: "560px" },
  section: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM },
  muted: { color: tokens.colorNeutralForeground3 },
  divider: { borderTop: `1px solid ${tokens.colorNeutralStroke2}`, paddingTop: tokens.spacingVerticalXL },
});

/** Everything in the app as one encrypted YAML file, and back. The file is chosen and written by the backend; only the password goes through here. */
export function TransferSettings() {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const desktop = inTauri();

  const [exportPw, setExportPw] = useState("");
  const [confirmPw, setConfirmPw] = useState("");
  const [importPw, setImportPw] = useState("");
  const [overwrite, setOverwrite] = useState(false);
  const [busy, setBusy] = useState<"export" | "import" | null>(null);

  const short = exportPw.length > 0 && exportPw.length < MIN_PASSWORD;
  const mismatch = confirmPw.length > 0 && confirmPw !== exportPw;
  const canExport = desktop && !busy && exportPw.length >= MIN_PASSWORD && exportPw === confirmPw;

  const doExport = async () => {
    setBusy("export");
    try {
      const path = await call<string | null>("transfer_export", { password: exportPw });
      if (path) {
        notify(t("settings.transfer.exported", { path }), "success");
        setExportPw("");
        setConfirmPw("");
      }
    } catch (e) {
      notify(String(e), "error");
    }
    setBusy(null);
  };

  const doImport = async () => {
    setBusy("import");
    try {
      const s = await call<Summary | null>("transfer_import", { password: importPw, overwrite });
      if (s) {
        notify(t("settings.transfer.imported", { ...s }), "success");
        setImportPw("");
        window.dispatchEvent(new Event("projects-changed"));
      }
    } catch (e) {
      notify(String(e), "error");
    }
    setBusy(null);
  };

  return (
    <div className={styles.page}>
      {!desktop && <Body1 block>{t("settings.transfer.desktopOnly")}</Body1>}

      <section className={styles.section}>
        <Subtitle1>{t("settings.transfer.exportTitle")}</Subtitle1>
        <Body1 className={styles.muted}>{t("settings.transfer.exportIntro")}</Body1>
        <MessageBar intent="warning">
          <MessageBarBody>{t("settings.transfer.exportWarning")}</MessageBarBody>
        </MessageBar>
        <Field label={t("settings.transfer.password")} hint={t("settings.transfer.passwordHint", { min: MIN_PASSWORD })} validationState={short ? "error" : "none"} validationMessage={short ? t("settings.transfer.tooShort", { min: MIN_PASSWORD }) : undefined}>
          <Input type="password" autoComplete="new-password" disabled={!desktop} value={exportPw} onChange={(_, d) => setExportPw(d.value)} />
        </Field>
        <Field label={t("settings.transfer.confirm")} validationState={mismatch ? "error" : "none"} validationMessage={mismatch ? t("settings.transfer.mismatch") : undefined}>
          <Input
            type="password"
            autoComplete="new-password"
            disabled={!desktop}
            value={confirmPw}
            onChange={(_, d) => setConfirmPw(d.value)}
            onKeyDown={(e) => e.key === "Enter" && canExport && void doExport()}
          />
        </Field>
        <div>
          <Button appearance="primary" icon={<ArrowDownload20Regular />} disabled={!canExport} onClick={() => void doExport()}>
            {t("settings.transfer.export")}
          </Button>
        </div>
      </section>

      <section className={`${styles.section} ${styles.divider}`}>
        <Subtitle1>{t("settings.transfer.importTitle")}</Subtitle1>
        <Body1 className={styles.muted}>{t("settings.transfer.importIntro")}</Body1>
        <Field label={t("settings.transfer.password")}>
          <Input
            type="password"
            autoComplete="off"
            disabled={!desktop}
            value={importPw}
            onChange={(_, d) => setImportPw(d.value)}
            onKeyDown={(e) => e.key === "Enter" && desktop && !busy && importPw && void doImport()}
          />
        </Field>
        <Checkbox checked={overwrite} disabled={!desktop} label={t("settings.transfer.overwrite")} onChange={(_, d) => setOverwrite(!!d.checked)} />
        <div>
          <Button icon={<ArrowUpload20Regular />} disabled={!desktop || !!busy || !importPw} onClick={() => void doImport()}>
            {t("settings.transfer.import")}
          </Button>
        </div>
      </section>
    </div>
  );
}
