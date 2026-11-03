import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Body1, Button, Caption1, Input, MessageBar, MessageBarBody, mergeClasses } from "@fluentui/react-components";
import { ArrowDownload20Regular, ArrowUpload20Regular, Delete20Regular, Document20Regular, Folder20Regular, FolderAdd20Regular, FolderOpen20Regular } from "@fluentui/react-icons";
import { useNotify } from "../../notifications";
import { call } from "../../tauri";
import { useConfirmDelete } from "../../components/Confirm";
import { fmtDateTime } from "../../text";
import { useStyles } from "./styles";
import { fmtBytes } from "./types";

interface FileEntry {
  name: string;
  dir: boolean;
  size: number;
  modified: number;
}

/**
 * The session's shared folder (what the agents see as /home/agent/shared), live: the agents write
 * into it while the session runs, so the list refreshes. What an agent wrote is untrusted, so a
 * file can be saved somewhere, never opened from here.
 */
export function Files({ project, sid }: { project: string | null; sid: string }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const confirm = useConfirmDelete();
  const [path, setPath] = useState("");
  const [items, setItems] = useState<FileEntry[] | null>(null);
  const [folderName, setFolderName] = useState<string | null>(null);
  const args = useCallback((extra: object = {}) => ({ project, id: sid, path, ...extra }), [project, sid, path]);

  const load = useCallback(async () => {
    try {
      setItems(await call<FileEntry[]>("shared_list", args()));
    } catch (e) {
      // the folder went away (a deleted session) or the path did: back to the top
      setItems([]);
      if (path) setPath("");
      else notify(String(e), "error");
    }
  }, [args, path, notify]);

  useEffect(() => {
    void load();
    const timer = setInterval(() => void load(), 3000);
    return () => clearInterval(timer);
  }, [load]);

  const run = async (f: () => Promise<unknown>) => {
    try {
      await f();
    } catch (e) {
      notify(String(e), "error");
    }
    await load();
  };
  const child = (name: string) => (path ? `${path}/${name}` : name);
  const parts = path.split("/").filter(Boolean);

  return (
    <>
      <div className={styles.bar}>
        <Button size="small" icon={<ArrowUpload20Regular />} onClick={() => void run(async () => {
          const n = await call<number>("shared_upload", args());
          if (n > 0) notify(t("session.files.uploaded", { count: n }), "success");
        })}>
          {t("session.files.upload")}
        </Button>
        <Button size="small" icon={<FolderAdd20Regular />} onClick={() => setFolderName("")}>{t("session.files.newFolder")}</Button>
        <Button size="small" icon={<FolderOpen20Regular />} onClick={() => void run(() => call("shared_reveal", { project, id: sid }))}>{t("session.files.reveal")}</Button>
      </div>
      {folderName !== null && (
        <div className={styles.bar}>
          <Input
            size="small"
            autoFocus
            value={folderName}
            placeholder={t("session.files.folderName")}
            onChange={(_, d) => setFolderName(d.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && folderName.trim()) void run(() => call("shared_mkdir", args({ name: folderName.trim() }))).then(() => setFolderName(null));
              if (e.key === "Escape") setFolderName(null);
            }}
          />
          <Button size="small" appearance="primary" disabled={!folderName.trim()} onClick={() => void run(() => call("shared_mkdir", args({ name: folderName.trim() }))).then(() => setFolderName(null))}>
            {t("session.files.create")}
          </Button>
          <Button size="small" onClick={() => setFolderName(null)}>{t("common.cancel")}</Button>
        </div>
      )}
      <Caption1 className={styles.muted}>
        <button type="button" className={styles.link} onClick={() => setPath("")}>{t("session.files.root")}</button>
        {parts.map((p, i) => (
          <span key={i}>
            {" / "}
            <button type="button" className={styles.link} onClick={() => setPath(parts.slice(0, i + 1).join("/"))}>{p}</button>
          </span>
        ))}
      </Caption1>
      <MessageBar intent="info">
        <MessageBarBody>{t("session.files.hint")}</MessageBarBody>
      </MessageBar>
      {items !== null && items.length === 0 && <Body1 className={styles.muted}>{t("session.files.empty")}</Body1>}
      {(items ?? []).map((f) => (
        <div key={f.name} className={mergeClasses(styles.fileRow)}>
          {f.dir ? <Folder20Regular /> : <Document20Regular />}
          {f.dir ? (
            <button type="button" className={mergeClasses(styles.link, styles.fileName)} onClick={() => setPath(child(f.name))}>{f.name}</button>
          ) : (
            <span className={styles.fileName}>{f.name}</span>
          )}
          <Caption1 className={styles.muted}>{f.dir ? "" : fmtBytes(f.size)}</Caption1>
          <Caption1 className={styles.muted}>{fmtDateTime(f.modified)}</Caption1>
          <Button size="small" appearance="subtle" icon={<ArrowDownload20Regular />} title={t("session.files.download")} aria-label={t("session.files.download")} onClick={() => void run(() => call("shared_download", args({ path: child(f.name), dir: f.dir })))} />
          <Button size="small" appearance="subtle" icon={<Delete20Regular />} title={t("common.delete")} aria-label={t("common.delete")} onClick={() => confirm.ask(f.name, () => run(() => call("shared_delete", args({ path: child(f.name) }))))} />
        </div>
      ))}
      {confirm.dialog}
    </>
  );
}
