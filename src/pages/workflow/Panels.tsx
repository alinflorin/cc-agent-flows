import { useState } from "react";
import { useTranslation } from "react-i18next";
import { capitalize, fmtDateTime } from "../../text";
import { Button, Caption1, Field, Input, Select, Subtitle2, Textarea, tokens } from "@fluentui/react-components";
import { Copy20Regular, Delete20Regular, Send20Regular } from "@fluentui/react-icons";
import { useNotify } from "../../notifications";
import { call } from "../../tauri";
import { useProjectId } from "../../project";
import { useStyles } from "./styles";
import type { AEdge, ANode, Agent, Data, ProfileInfo, TriggerInfo } from "./types";

export type PatchData = (id: string, f: (d: Data) => Data) => void;
export type PatchAgent = (id: string, patch: Partial<Agent>) => void;

export function EdgePanel({ edge, nameOf, setEdgeKind }: { edge: AEdge; nameOf: (id: string | null) => string; setEdgeKind: (id: string, kind: "delegate" | "before") => void }) {
  const { t } = useTranslation();
  return (
    <>
      <Subtitle2>
        {nameOf(edge.source)} → {nameOf(edge.target)}
      </Subtitle2>
      <Field label={t("wf.edgeKind")} hint={t(`wf.edgeKindHint.${edge.data?.kind ?? "delegate"}`)}>
        <Select value={edge.data?.kind ?? "delegate"} onChange={(_, d) => setEdgeKind(edge.id, d.value as "delegate" | "before")}>
          <option value="delegate">{t("wf.edgeKinds.delegate")}</option>
          <option value="before">{t("wf.edgeKinds.before")}</option>
        </Select>
      </Field>
    </>
  );
}

export function TriggerPanel(p: {
  node: ANode;
  id: string;
  dirty: boolean;
  infos: TriggerInfo[];
  preview: string[] | string;
  setPreview: (v: string[] | string) => void;
  sample: string;
  setSample: (v: string) => void;
  patchData: PatchData;
  patchAgent: PatchAgent;
  remove: (id: string) => void;
  loadInfos: () => Promise<void>;
}) {
  const { node, id, dirty, infos, preview, setPreview, sample, setSample, patchData, patchAgent, remove, loadInfos } = p;
  const { t } = useTranslation();
  const project = useProjectId();
  const styles = useStyles();
  const notify = useNotify();
  const [firing, setFiring] = useState(false);
  const agent = node.data.agent;
  return (
    <>
      <Subtitle2>{t(`wf.kind.${agent.kind}`)}</Subtitle2>
      <Caption1>{t(`wf.kindHint.${agent.kind}`)}</Caption1>
      <Field label={t("wf.label")}>
        <Input value={node.data.label} maxLength={120} placeholder={t(`wf.kind.${agent.kind}`)} onChange={(_, d) => patchData(node.id, (x) => ({ ...x, label: d.value }))} />
      </Field>
      {agent.kind === "cron" && (
        <Field label={t("wf.cron")} hint={typeof preview === "string" ? undefined : t("wf.cronHint")} validationMessage={typeof preview === "string" ? preview : undefined}>
          <Input
            className={styles.mono}
            value={agent.trigger.cron}
            onChange={(_, d) => {
              patchAgent(node.id, { trigger: { ...agent.trigger, cron: d.value } });
              call<string[]>("cron_preview", { cron: d.value }).then(setPreview, (e) => setPreview(capitalize(String(e))));
            }}
            onFocus={() => call<string[]>("cron_preview", { cron: agent.trigger.cron }).then(setPreview, (e) => setPreview(capitalize(String(e))))}
          />
        </Field>
      )}
      {agent.kind === "cron" && Array.isArray(preview) && preview.length > 0 && (
        <Caption1 block>
          {t("wf.cronNext")}: {preview.join(" · ")}
        </Caption1>
      )}
      {agent.kind !== "chat" && (
        <Field label={agent.kind === "cron" ? t("wf.cronRequest") : t("wf.webhookText")} hint={agent.kind === "cron" ? t("wf.cronRequestHint") : t("wf.webhookTextHint")}>
          <Textarea rows={5} value={agent.prompt} onChange={(_, d) => patchAgent(node.id, { prompt: d.value })} />
        </Field>
      )}
      {agent.kind !== "chat" && (
        <Field label={t("wf.triggerAutonomy")} hint={t("wf.triggerAutonomyHint")}>
          <Select value={agent.trigger.autonomy || "never"} onChange={(_, d) => patchAgent(node.id, { trigger: { ...agent.trigger, autonomy: d.value as "ask" | "never" } })}>
            <option value="never">{t("session.autonomy.never")}</option>
            <option value="ask">{t("session.autonomy.ask")}</option>
          </Select>
        </Field>
      )}
      {(() => {
        const info = infos.find((i) => i.workflow === id && i.node === node.id);
        if (agent.kind === "chat") return null;
        if (!info || dirty) return <Caption1>{t("wf.saveFirst")}</Caption1>;
        return (
          <>
            {info.url && (
              <Field label={t("wf.webhookUrl")} hint={t("wf.webhookUrlHint")}>
                <div className={styles.row}>
                  <span className={styles.mono}>{info.url}</span>
                  <Button appearance="subtle" icon={<Copy20Regular />} aria-label={t("common.copy")} onClick={() => void navigator.clipboard.writeText(info.url ?? "")} />
                </div>
              </Field>
            )}
            {info.kind === "webhook" && (
              <Field label={t("wf.webhookSample")}>
                <Textarea rows={3} value={sample} onChange={(_, d) => setSample(d.value)} />
              </Field>
            )}
            <Button
              icon={<Send20Regular />}
              disabled={firing}
              onClick={async () => {
                setFiring(true);
                try {
                  await call<string>("trigger_fire", { project, workflow: id, node: node.id, payload: info.kind === "webhook" ? sample : null });
                  notify(t("wf.fired"), "success");
                } catch (e) {
                  notify(String(e), "error");
                }
                setFiring(false);
                void loadInfos();
              }}
            >
              {info.kind === "webhook" ? t("wf.webhookTest") : t("wf.cronRunNow")}
            </Button>
            {info.next && <Caption1 block>{t("wf.cronNextRun", { when: info.next })}</Caption1>}
            {info.last_run && <Caption1 block>{t("wf.lastRun", { when: fmtDateTime(info.last_run) })}</Caption1>}
            {info.last_error && <Caption1 block style={{ color: tokens.colorPaletteRedForeground1 }}>{capitalize(info.last_error)}</Caption1>}
          </>
        );
      })()}
      <Button icon={<Delete20Regular />} onClick={() => remove(node.id)}>
        {t("wf.removeNode")}
      </Button>
    </>
  );
}

export function HumanPanel({ node, patchData, remove }: { node: ANode; patchData: PatchData; remove: (id: string) => void }) {
  const { t } = useTranslation();
  const agent = node.data.agent;
  return (
    <>
      <Subtitle2>{t(`wf.kind.${agent.kind}`)}</Subtitle2>
      <Caption1>{t(`wf.kindHint.${agent.kind}`)}</Caption1>
      <Field label={t("wf.label")}>
        <Input value={node.data.label} maxLength={120} placeholder={t("wf.kind.you")} onChange={(_, d) => patchData(node.id, (x) => ({ ...x, label: d.value }))} />
      </Field>
      <Button icon={<Delete20Regular />} onClick={() => remove(node.id)}>
        {t("wf.removeNode")}
      </Button>
    </>
  );
}

export function AgentPanel(p: { node: ANode; profiles: ProfileInfo[]; handsTo: number; patchData: PatchData; patchAgent: PatchAgent; remove: (id: string) => void }) {
  const { node, profiles, handsTo, patchData, patchAgent, remove } = p;
  const { t } = useTranslation();
  const agent = node.data.agent;
  return (
    <>
      <Subtitle2>{t("wf.agent")}</Subtitle2>
      <Caption1>{handsTo > 0 ? t("wf.coordinates", { count: handsTo }) : t("wf.worker")}</Caption1>
      <Field label={t("wf.label")}>
        <Input value={node.data.label} maxLength={120} onChange={(_, d) => patchData(node.id, (x) => ({ ...x, label: d.value }))} />
      </Field>
      <Field label={t("wf.profile")}>
        <Select value={agent.profile} onChange={(_, d) => patchAgent(node.id, { profile: d.value })}>
          {!profiles.some((p) => p.id === agent.profile) && <option value={agent.profile}>{agent.profile}</option>}
          {profiles.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </Select>
      </Field>
      <Field label={t("wf.prompt")} hint={t("wf.agentPromptHint")}>
        <Textarea rows={8} value={agent.prompt} onChange={(_, d) => patchAgent(node.id, { prompt: d.value })} />
      </Field>
      <Field label={t("wf.maxCalls")} hint={t("wf.maxCallsHint")}>
        <Input
          type="number"
          min={1}
          max={500}
          value={String(agent.max_calls)}
          onChange={(_, d) => patchAgent(node.id, { max_calls: Math.min(500, Math.max(1, Math.floor(Number(d.value)) || 1)) })}
        />
      </Field>
      <Field label={t("wf.timeout")} hint={t("wf.timeoutHint")}>
        <Input
          type="number"
          min={1}
          max={480}
          value={String(agent.timeout_mins)}
          onChange={(_, d) => patchAgent(node.id, { timeout_mins: Math.min(480, Math.max(1, Math.floor(Number(d.value)) || 1)) })}
        />
      </Field>
      <Button icon={<Delete20Regular />} onClick={() => remove(node.id)}>
        {t("wf.remove")}
      </Button>
    </>
  );
}
