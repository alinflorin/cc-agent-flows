import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Background, Controls, Handle, MarkerType, Position, ReactFlow, useNodesState, type Edge, type NodeProps } from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { Body1, Caption1, Spinner, mergeClasses, tokens } from "@fluentui/react-components";
import { capitalize } from "../../text";
import { activeEdges, nodeNet, nodeStates, nodeVms, type Digest, type NodeState, type VmState, type VmStats } from "../../sessionLog";
import { useStyles } from "./styles";
import { useProjectId } from "../../project";
import { LiveNode, Pending, WEdge, WNode, fmtBytes, fmtRate, fmtTokens } from "./types";

/** A coloured dot and the VM's state; pulses while the VM is on its way up. */
export function VmPill({ state, count }: { state: VmState | "none"; count?: number }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const s = styles as Record<string, string>;
  const transient = state === "preparing" || state === "starting" || state === "booting";
  return (
    <span className={styles.vm} title={t("session.vm.title")}>
      <span className={mergeClasses(styles.vmDot, s[`vm_${state === "none" ? "off" : state}`], transient && styles.vmPulse)} />
      {t(`session.vm.${state}`)}
      {count && count > 1 ? ` ×${count}` : ""}
    </span>
  );
}

/** What the agent is doing: a coloured dot and the word, nothing around it. */
export function AgentPill({ state }: { state: NodeState }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const s = styles as Record<string, string>;
  return (
    <span className={styles.agentState}>
      <span className={mergeClasses(styles.vmDot, s[`a_${state}`], state === "running" && styles.vmPulse)} />
      {capitalize(t(`session.node.${state}`))}
    </span>
  );
}

export const pct = (v: number, max: number) => Math.max(0, Math.min(100, max > 0 ? (v / max) * 100 : 0));

/** CPU and memory of the VM as two thin bars (CPU: of all its virtual cores; memory: of what it was given). */
export function Meters({ use }: { use: VmStats }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const cpu = pct(use.cpu, use.cpus * 100);
  const mem = pct(use.mem, use.memLimit);
  const row = (label: string, value: number, text: string) => (
    <div className={styles.meter}>
      <span>{label}</span>
      <div className={styles.track}>
        <div className={mergeClasses(styles.fill, value > 90 && styles.fillHot)} style={{ width: `${value}%` }} />
      </div>
      <span>{text}</span>
    </div>
  );
  return (
    <div className={styles.meters} title={t("session.vm.usageTitle")}>
      {row(t("session.vm.cpu"), cpu, `${Math.round(use.cpu)}%`)}
      {row(t("session.vm.mem"), mem, gb(use.mem))}
    </div>
  );
}

/** The last samples of one VM's CPU as a line. */
export function Spark({ samples }: { samples: VmStats[] }) {
  const styles = useStyles();
  if (samples.length < 2) return null;
  const max = Math.max(100 * samples[0].cpus, ...samples.map((s) => s.cpu));
  const pts = samples.map((s, i) => `${(i / (samples.length - 1)) * 100},${34 - (s.cpu / max) * 32}`).join(" ");
  return (
    <svg className={styles.spark} viewBox="0 0 100 36" preserveAspectRatio="none" aria-hidden>
      <polyline points={pts} fill="none" stroke="currentColor" strokeWidth="1.5" vectorEffect="non-scaling-stroke" />
    </svg>
  );
}

export const gb = (mb: number) => (mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${Math.round(mb)} MB`);

/** The VM line of a node: its state and, once it is sampled, what it takes, in the little room a node has. */
export function NodeVm({ vm }: { vm: { state: VmState | "none"; count: number; use: VmStats | null } }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const use = vm.state !== "off" && vm.state !== "none" ? vm.use : null;
  const bar = (value: number) => (
    <div className={styles.track}>
      <div className={mergeClasses(styles.fill, value > 90 && styles.fillHot)} style={{ width: `${value}%` }} />
    </div>
  );
  return (
    <div title={use ? t("session.vm.usageTitle") : undefined}>
      <div className={styles.vmRow}>
        <VmPill state={vm.state} count={vm.count} />
        {use && <span className={styles.usage}>{Math.round(use.cpu)}% · {gb(use.mem)}</span>}
      </div>
      {use && (
        <div className={styles.bars}>
          {bar(pct(use.cpu, use.cpus * 100))}
          {bar(pct(use.mem, use.memLimit))}
        </div>
      )}
    </div>
  );
}

export function LiveNodeView({ data }: NodeProps<LiveNode>) {
  const { t } = useTranslation();
  const styles = useStyles();
  const cls = mergeClasses(styles.node, (styles as Record<string, string>)[`s_${data.state}`], data.picked && styles.picked);
  return (
    <div className={cls}>
      <Handle type="target" position={Position.Left} />
      {!data.human && <Handle type="target" id="order-in" position={Position.Top} style={{ opacity: 0 }} />}
      <div className={styles.nodeTop}>
        <Body1><b>{data.human ? "👤 " : ""}{data.label}</b></Body1>
        {data.state === "running" && <Spinner size="extra-tiny" />}
      </div>
      {data.sub && data.sub !== data.label && <Caption1 block className={styles.muted}>{data.sub}</Caption1>}
      <div className={styles.vmRow}>
        <AgentPill state={data.state} />
        {(data.calls > 0 || data.tokens > 0) && (
          <span className={styles.usage}>
            {[data.calls > 0 ? t("session.callsCount", { count: data.calls }) : "", data.tokens > 0 ? t("session.tokens", { n: fmtTokens(data.tokens) }) : ""].filter(Boolean).join(" · ")}
          </span>
        )}
      </div>
      {!data.human && <NodeVm vm={data.vm ?? { state: "none", count: 0, use: null }} />}
      {!data.human && (
        <div className={styles.netRows} title={t("session.net.title")}>
          <span className={styles.netLine}>
            ↓ {fmtBytes(data.net?.down ?? 0)} · ↑ {fmtBytes(data.net?.up ?? 0)}
          </span>
          <span className={styles.netLine}>
            {data.net && data.net.speed.up + data.net.speed.down > 0 ? `↓ ${fmtRate(data.net.speed.down)} · ↑ ${fmtRate(data.net.speed.up)}` : "\u00a0"}
          </span>
        </div>
      )}
      <Handle type="source" position={Position.Right} />
      {!data.human && <Handle type="source" id="order-out" position={Position.Bottom} style={{ opacity: 0 }} />}
    </div>
  );
}

export const nodeTypes = { live: LiveNodeView };
// the live boxes are bigger than the editor's (220x118 against 190x58): spread the positions by the same ratio so nothing overlaps
export const ROW_SPREAD = 2;
export const COL_SPREAD = 1.2;

export function Graph({ wf, d, pending, picked, onPick }: { wf: { nodes: WNode[]; edges: WEdge[] }; d: Digest; pending: Pending | null; picked: string | null; onPick: (id: string) => void }) {
  const { t } = useTranslation();
  const states = useMemo(() => nodeStates(d, pending ? { node: pending.node, human: pending.human } : null), [d, pending]);
  const vms = useMemo(() => nodeVms(d), [d]);
  const nets = useMemo(() => nodeNet(d), [d]);
  const project = useProjectId();
  const [profileNames, setProfileNames] = useState<Record<string, string>>({});
  useEffect(() => {
    void import("@tauri-apps/api/core").then(({ invoke }) =>
      invoke<{ id: string; name: string }[]>("profiles_list", { project }).then((ps) => setProfileNames(Object.fromEntries(ps.map((p) => [p.id, p.name])))),
    ).catch(() => {});
  }, [project]);
  const kinds = useMemo(() => new Map(wf.nodes.map((n) => [n.id, n.kind as string])), [wf]);
  const active = useMemo(() => activeEdges(d, wf.edges, kinds), [d, wf, kinds]);
  const stats = useMemo(() => {
    const out = new Map<string, { calls: number; tokens: number }>();
    for (const c of d.calls.values()) out.set(c.node, { calls: (out.get(c.node)?.calls ?? 0) + 1, tokens: out.get(c.node)?.tokens ?? 0 });
    for (const r of d.runs.values()) out.set(r.node, { calls: out.get(r.node)?.calls ?? 0, tokens: (out.get(r.node)?.tokens ?? 0) + r.tokens });
    return out;
  }, [d]);

  // The nodes live in React Flow's state: it keeps their measured size there. Handing it brand new
  // node objects on every render (the log streams in) makes it forget the sizes and hide the nodes.
  const [nodes, setNodes, onNodesChange] = useNodesState<LiveNode>([]);
  useEffect(() => {
    setNodes((prev) =>
      wf.nodes.map((n): LiveNode => ({
        ...prev.find((p) => p.id === n.id),
        id: n.id,
        type: "live",
        position: { x: n.x * COL_SPREAD, y: n.y * ROW_SPREAD },
        draggable: false,
        connectable: false,
        data: {
          label: n.label || (n.kind === "agent" ? (profileNames[n.profile] ?? n.profile) : t("wf.kind.you")),
          sub: n.kind === "agent" ? (profileNames[n.profile] ?? n.profile) : t(`wf.kind.${n.kind}`),
          human: n.kind !== "agent",
          state: states.get(n.id) ?? "idle",
          calls: stats.get(n.id)?.calls ?? 0,
          tokens: stats.get(n.id)?.tokens ?? 0,
          picked: picked === n.id,
          vm: vms.get(n.id) ?? null,
          net: nets.get(n.id) ?? null } })),
    );
  }, [wf, states, stats, vms, nets, picked, profileNames, t, setNodes]);
  const edges: Edge[] = wf.edges.map((e) => ({
    id: e.id,
    source: e.from,
    target: e.to,
    sourceHandle: e.kind === "before" ? "order-out" : undefined,
    targetHandle: e.kind === "before" ? "order-in" : undefined,
    animated: active.has(e.id),
    label: e.kind === "before" ? t("wf.orderLabel") : undefined,
    // quiet by default: only the arrows work is going through now are coloured
    style: { strokeDasharray: e.kind === "before" ? "6 4" : undefined, strokeWidth: active.has(e.id) ? 2.5 : 1.5, stroke: active.has(e.id) ? tokens.colorBrandStroke1 : tokens.colorNeutralStroke1 },
    markerEnd: { type: MarkerType.ArrowClosed, color: active.has(e.id) ? tokens.colorBrandStroke1 : tokens.colorNeutralStroke1 } }));
  return (
    <ReactFlow
      nodes={nodes}
      edges={edges}
      onNodesChange={onNodesChange}
      nodeTypes={nodeTypes}
      nodesDraggable={false}
      nodesConnectable={false}
      zoomOnDoubleClick={false}
      minZoom={0.2}
      onNodeClick={(_, n) => onPick(n.id)}
      colorMode="system"
      fitView
      proOptions={{ hideAttribution: true }}
    >
      <Background />
      <Controls showInteractive={false} />
    </ReactFlow>
  );
}

/** Long text cut to three lines; a click opens the rest. */
