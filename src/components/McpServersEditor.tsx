import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Field, Input, Select, Textarea, makeStyles, tokens } from "@fluentui/react-components";
import { Add20Regular, Delete20Regular } from "@fluentui/react-icons";

export interface McpEnvVar {
  name: string;
  value: string;
}

export interface McpServer {
  name: string;
  kind: "stdio" | "http" | "sse";
  command: string;
  args: string[];
  env: McpEnvVar[];
  url: string;
  headers: McpEnvVar[];
}

export const NEW_MCP_SERVER: McpServer = { name: "", kind: "http", command: "", args: [], env: [], url: "", headers: [] };

const useStyles = makeStyles({
  list: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM },
  server: {
    display: "flex",
    flexDirection: "column",
    gap: tokens.spacingVerticalS,
    padding: tokens.spacingHorizontalM,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    borderRadius: tokens.borderRadiusMedium,
  },
  row: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "flex-start" },
  grow: { flex: 1 },
  mono: { fontFamily: tokens.fontFamilyMonospace },
});

/** `NAME=value` per line. */
const parsePairs = (text: string): McpEnvVar[] =>
  text
    .split("\n")
    .filter((l) => l.trim() !== "")
    .map((l) => {
      const i = l.indexOf("=");
      return i < 0 ? { name: l.trim(), value: "" } : { name: l.slice(0, i).trim(), value: l.slice(i + 1) };
    });
const showPairs = (v: McpEnvVar[]) => v.map((p) => `${p.name}=${p.value}`).join("\n");

/** A textarea that keeps what is typed (blank lines included) and reports the parsed lines. */
function Lines<T>({ value, show, parse, onChange, placeholder }: { value: T; show: (v: T) => string; parse: (s: string) => T; onChange: (v: T) => void; placeholder?: string }) {
  const styles = useStyles();
  const [text, setText] = useState(() => show(value));
  return (
    <Textarea
      rows={3}
      textarea={{ className: styles.mono }}
      placeholder={placeholder}
      value={text}
      onChange={(_, d) => {
        setText(d.value);
        onChange(parse(d.value));
      }}
    />
  );
}

export function McpServersEditor({ value, onChange }: { value: McpServer[]; onChange: (v: McpServer[]) => void }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const patch = (i: number, p: Partial<McpServer>) => onChange(value.map((s, j) => (j === i ? { ...s, ...p } : s)));
  return (
    <div className={styles.list}>
      {value.map((s, i) => (
        <div key={i} className={styles.server}>
          <div className={styles.row}>
            <Input className={styles.grow} placeholder={t("profiles.mcpName")} value={s.name} maxLength={64} onChange={(_, d) => patch(i, { name: d.value })} />
            <Select value={s.kind} onChange={(_, d) => patch(i, { kind: d.value as McpServer["kind"] })}>
              <option value="http">{t("profiles.mcpHttp")}</option>
              <option value="sse">{t("profiles.mcpSse")}</option>
              <option value="stdio">{t("profiles.mcpStdio")}</option>
            </Select>
            <Button appearance="subtle" icon={<Delete20Regular />} aria-label={t("profiles.delete")} onClick={() => onChange(value.filter((_, j) => j !== i))} />
          </div>
          {s.kind === "stdio" ? (
            <>
              <Field label={t("profiles.mcpCommand")} hint={t("profiles.mcpCommandHint")}>
                <Input input={{ className: styles.mono }} placeholder="npx" value={s.command} onChange={(_, d) => patch(i, { command: d.value })} />
              </Field>
              <Field label={t("profiles.mcpArgs")} hint={t("profiles.mcpArgsHint")}>
                <Lines value={s.args} show={(v) => v.join("\n")} parse={(x) => x.split("\n").filter((l) => l !== "")} onChange={(args) => patch(i, { args })} placeholder={"-y\n@modelcontextprotocol/server-everything"} />
              </Field>
              <Field label={t("profiles.mcpEnv")} hint={t("profiles.mcpPairsHint")}>
                <Lines value={s.env} show={showPairs} parse={parsePairs} onChange={(env) => patch(i, { env })} placeholder="API_KEY=${secret:API_KEY}" />
              </Field>
            </>
          ) : (
            <>
              <Field label={t("profiles.mcpUrl")} hint={t("profiles.mcpUrlHint")}>
                <Input input={{ className: styles.mono }} placeholder={s.kind === "http" ? "https://mcp.example.com/mcp" : "https://mcp.example.com/sse"} value={s.url} onChange={(_, d) => patch(i, { url: d.value })} />
              </Field>
              <Field label={t("profiles.mcpHeaders")} hint={t("profiles.mcpPairsHint")}>
                <Lines value={s.headers} show={showPairs} parse={parsePairs} onChange={(headers) => patch(i, { headers })} placeholder="Authorization=Bearer ${injection:EXAMPLE}" />
              </Field>
            </>
          )}
        </div>
      ))}
      <div>
        <Button icon={<Add20Regular />} onClick={() => onChange([...value, { ...NEW_MCP_SERVER }])}>
          {t("profiles.mcpAdd")}
        </Button>
      </div>
    </div>
  );
}
