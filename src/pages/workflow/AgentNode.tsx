import { Handle, Position, type NodeProps } from "@xyflow/react";
import { useTranslation } from "react-i18next";
import { Body1, Caption1 } from "@fluentui/react-components";
import { useStyles } from "./styles";
import { ICON, isTrigger, type ANode } from "./types";

export function AgentNodeView({ data, selected }: NodeProps<ANode>) {
  const { t } = useTranslation();
  const styles = useStyles();
  const human = data.agent.kind !== "agent";
  const trigger = isTrigger(data.agent.kind);
  const cls = [styles.node];
  if (human) cls.push(trigger ? styles.trig : styles.human);
  if (data.problem) cls.push(styles.warn);
  if (selected) cls.push(styles.selected);
  return (
    <div className={cls.join(" ")}>
      {!trigger && <Handle type="target" position={Position.Left} />}
      {!human && <Handle type="target" id="order-in" position={Position.Top} style={{ width: 8, height: 8, background: "#fff", border: "1.5px solid #888" }} />}
      <Body1 block>
        <b>{human ? `${ICON[data.agent.kind]} ${data.label || t(trigger ? `wf.kind.${data.agent.kind}` : "wf.kind.you")}` : data.label || data.profileName || data.agent.profile}</b>
      </Body1>
      {((human && !(trigger && !data.label)) || (data.label && data.label !== (data.profileName || data.agent.profile))) && (
        <Caption1 block>{human ? (trigger && data.agent.kind === "cron" && data.agent.trigger.cron ? data.agent.trigger.cron : t(`wf.kind.${data.agent.kind}`)) : data.profileName || data.agent.profile}</Caption1>
      )}
      {data.root && <Caption1 block>★ {t("wf.root")}</Caption1>}
      {data.agent.kind !== "ask" && <Handle type="source" position={Position.Right} />}
      {!human && <Handle type="source" id="order-out" position={Position.Bottom} style={{ width: 8, height: 8, background: "#fff", border: "1.5px solid #888" }} />}
    </div>
  );
}

export const nodeTypes = { agent: AgentNodeView };

