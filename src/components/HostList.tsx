import { Button, Checkbox, Input, makeStyles, tokens } from "@fluentui/react-components";
import { Add20Regular, Delete20Regular } from "@fluentui/react-icons";
import { useTranslation } from "react-i18next";

const HTTP = "http://";

const useStyles = makeStyles({
  list: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalS },
  row: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "center" },
  grow: { flex: 1, fontFamily: tokens.fontFamilyMonospace },
});

/**
 * Allowed hosts, one row each. An entry that also allows plain HTTP is stored with an `http://`
 * prefix, which the row shows as a ticked box (the backend reads the prefix).
 */
export function HostList({ value, onChange, disabled, placeholder }: { value: string[]; onChange: (hosts: string[]) => void; disabled?: boolean; placeholder?: string }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const set = (i: number, host: string, http: boolean) => onChange(value.map((x, j) => (j === i ? (http && host.trim() !== "" ? HTTP + host : host) : x)));
  return (
    <div className={styles.list}>
      {value.map((entry, i) => {
        const http = entry.toLowerCase().startsWith(HTTP);
        const host = http ? entry.slice(HTTP.length) : entry;
        return (
          <div key={i} className={styles.row}>
            <Input className={styles.grow} disabled={disabled} placeholder={placeholder} value={host} onChange={(_, d) => set(i, d.value, http)} />
            <Checkbox disabled={disabled || host.trim() === ""} checked={http} label={t("hosts.http")} title={t("hosts.httpHint")} onChange={(_, d) => set(i, host, d.checked === true)} />
            <Button appearance="subtle" disabled={disabled} icon={<Delete20Regular />} aria-label={t("common.delete")} onClick={() => onChange(value.filter((_, j) => j !== i))} />
          </div>
        );
      })}
      <div>
        <Button icon={<Add20Regular />} disabled={disabled} onClick={() => onChange([...value, ""])}>
          {t("hosts.add")}
        </Button>
      </div>
    </div>
  );
}
