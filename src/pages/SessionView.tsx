import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useParams } from "react-router";
import { useTranslation } from "react-i18next";
import { Background, Controls, Handle, MarkerType, Position, ReactFlow, ReactFlowProvider, useNodesState, type Edge, type Node, type NodeProps } from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { Badge, Body1, Button, Caption1, Field, Input, MessageBar, MessageBarBody, Select, Spinner, Subtitle2, Tab, TabList, Textarea, Title2, makeStyles, mergeClasses, tokens } from "@fluentui/react-components";
import { Edit20Regular } from "@fluentui/react-icons";
import { inTauri } from "../prefs";
import { useNotify } from "../notifications";
import { activeEdges, clip, digest, nodeStates, nodeVms, type CallInfo, type Digest, type Entry, type NodeState, type Run, type Traffic, type VmState, type VmStats } from "../sessionLog";

interface Pending {
  id: string;
  node: string;
  label: string;
  kind: "approve" | "questions";
  prompt: string;
  questions: string[];
  human: string | null;
}

interface View {
  session: { name: string; task: string; status: string; workflow: string; autonomy: "ask" | "never"; source: "chat" | "cron" | "webhook" };
  messages: string[];
  pending: Pending | null;
  live: boolean;
  tokens: number;
  result: string | null;
}

interface WNode {
  id: string;
  label: string;
  x: number;
  y: number;
  kind: "agent" | "ask" | "approve";
  profile: string;
}
interface WEdge {
  id: string;
  from: string;
  to: string;
  kind: "delegate" | "before";
}

type Tab_ = "chat" | "timeline" | "agent";

const useStyles = makeStyles({
  head: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalXS, marginBottom: tokens.spacingVerticalM },
  autonomy: { gridTemplateColumns: "auto minmax(0, 1fr)", columnGap: tokens.spacingHorizontalM, "& label": { whiteSpace: "nowrap" } },
  bar: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "center", flexWrap: "wrap", marginBottom: tokens.spacingVerticalM },
  title: { display: "flex", alignItems: "center", gap: tokens.spacingHorizontalS },
  grow: { flexGrow: 1 },
  body: { display: "flex", gap: tokens.spacingHorizontalM, height: "min(720px, calc(100vh - 250px))", minHeight: "460px" },
  canvas: { flex: 1, minWidth: 0, border: `1px solid ${tokens.colorNeutralStroke2}`, borderRadius: tokens.borderRadiusMedium },
  side: { width: "440px", display: "flex", flexDirection: "column", border: `1px solid ${tokens.colorNeutralStroke2}`, borderRadius: tokens.borderRadiusMedium, overflow: "hidden" },
  scroll: { flex: 1, minHeight: 0, overflow: "auto", display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM, padding: tokens.spacingHorizontalM, "& > *": { flexShrink: 0 } },
  tabs: { padding: `${tokens.spacingVerticalXS} ${tokens.spacingHorizontalS}`, borderBottom: `1px solid ${tokens.colorNeutralStroke2}` },
  box: { whiteSpace: "pre-wrap", overflowWrap: "anywhere", padding: tokens.spacingVerticalS, borderRadius: tokens.borderRadiusMedium, backgroundColor: tokens.colorNeutralBackground2, fontSize: tokens.fontSizeBase200 },
  user: { whiteSpace: "pre-wrap", overflowWrap: "anywhere", padding: tokens.spacingVerticalS, borderRadius: tokens.borderRadiusMedium, backgroundColor: tokens.colorBrandBackground2, alignSelf: "flex-end", maxWidth: "88%" },
  mono: { fontFamily: tokens.fontFamilyMonospace, color: tokens.colorNeutralForeground3 },
  muted: { color: tokens.colorNeutralForeground3 },
  ask: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalS, padding: tokens.spacingHorizontalM, border: `1px solid ${tokens.colorBrandStroke1}`, borderRadius: tokens.borderRadiusMedium },
  actions: { display: "flex", gap: tokens.spacingHorizontalS },
  composer: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "stretch", padding: tokens.spacingHorizontalM, borderTop: `1px solid ${tokens.colorNeutralStroke2}` },
  section: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalS, paddingBottom: tokens.spacingVerticalL, marginBottom: tokens.spacingVerticalL, borderBottom: `1px solid ${tokens.colorNeutralStroke2}`, ":last-of-type": { borderBottom: "none", marginBottom: 0 } },
  tl: { display: "flex", flexDirection: "column" },
  tlItem: { display: "grid", gridTemplateColumns: "14px minmax(0, 1fr)", columnGap: tokens.spacingHorizontalM, cursor: "pointer", paddingBottom: tokens.spacingVerticalL, ":hover": { backgroundColor: tokens.colorSubtleBackgroundHover } },
  rail: { display: "flex", flexDirection: "column", alignItems: "center" },
  dot: { width: "12px", height: "12px", borderRadius: "50%", flex: "none", marginTop: "3px", backgroundColor: tokens.colorNeutralForeground3 },
  dot_done: { backgroundColor: tokens.colorPaletteGreenBackground3 },
  dot_failed: { backgroundColor: tokens.colorPaletteRedBackground3 },
  dot_wait: { backgroundColor: tokens.colorPaletteMarigoldBackground3 },
  dot_brand: { backgroundColor: tokens.colorBrandBackground },
  line: { flex: 1, width: "2px", marginTop: tokens.spacingVerticalXXS, backgroundColor: tokens.colorNeutralStroke2 },
  tlHead: { display: "flex", alignItems: "baseline", gap: tokens.spacingHorizontalS, minWidth: 0 },
  tlWho: { fontWeight: tokens.fontWeightSemibold, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", minWidth: 0 },
  tlTime: { marginLeft: "auto", flex: "none", color: tokens.colorNeutralForeground3 },
  clamp: { display: "-webkit-box", WebkitBoxOrient: "vertical", WebkitLineClamp: "3", overflow: "hidden", whiteSpace: "pre-wrap", overflowWrap: "anywhere" },
  open: { WebkitLineClamp: "unset", display: "block" },
  card: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalS, padding: tokens.spacingHorizontalM, border: `1px solid ${tokens.colorNeutralStroke2}`, borderRadius: tokens.borderRadiusLarge, minWidth: 0, "& > *": { flexShrink: 0 } },
  cardHead: { display: "flex", alignItems: "center", gap: tokens.spacingHorizontalS, flexWrap: "wrap" },
  quote: { padding: `${tokens.spacingVerticalXS} ${tokens.spacingHorizontalM}`, borderLeft: `3px solid ${tokens.colorNeutralStroke1}`, color: tokens.colorNeutralForeground2 },
  outDone: { padding: tokens.spacingHorizontalS, borderRadius: tokens.borderRadiusMedium, backgroundColor: tokens.colorPaletteGreenBackground1 },
  outFail: { padding: tokens.spacingHorizontalS, borderRadius: tokens.borderRadiusMedium, backgroundColor: tokens.colorPaletteRedBackground1 },
  outAsk: { padding: tokens.spacingHorizontalS, borderRadius: tokens.borderRadiusMedium, backgroundColor: tokens.colorPaletteMarigoldBackground1 },
  handoff: { display: "grid", gridTemplateColumns: "auto minmax(0, 1fr)", gap: tokens.spacingHorizontalS, alignItems: "start" },
  chip: { padding: `1px ${tokens.spacingHorizontalS}`, borderRadius: tokens.borderRadiusCircular, backgroundColor: tokens.colorBrandBackground2, color: tokens.colorBrandForeground2, fontSize: tokens.fontSizeBase200, whiteSpace: "nowrap", maxWidth: "160px", overflow: "hidden", textOverflow: "ellipsis" },
  steps: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalXXS, marginTop: tokens.spacingVerticalXS },
  step: { fontSize: tokens.fontSizeBase200, color: tokens.colorNeutralForeground2, overflowWrap: "anywhere", whiteSpace: "pre-wrap" },
  summaryRow: { cursor: "pointer", color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  link: { cursor: "pointer", color: tokens.colorBrandForeground1, fontSize: tokens.fontSizeBase200, background: "none", border: "none", padding: 0, textAlign: "left" },
  node: {
    minWidth: "150px",
    maxWidth: "210px",
    overflowWrap: "anywhere",
    padding: `${tokens.spacingVerticalS} ${tokens.spacingHorizontalM}`,
    borderRadius: tokens.borderRadiusLarge,
    border: `2px solid ${tokens.colorNeutralStroke1}`,
    backgroundColor: tokens.colorNeutralBackground1,
    color: tokens.colorNeutralForeground1,
    boxShadow: tokens.shadow4,
  },
  nodeTop: { display: "flex", alignItems: "center", justifyContent: "space-between", gap: tokens.spacingHorizontalS },
  picked: { outline: `3px solid ${tokens.colorBrandStroke1}`, outlineOffset: "2px" },
  s_running: { border: `2px solid ${tokens.colorBrandStroke1}`, boxShadow: `0 0 0 4px ${tokens.colorBrandBackground2}` },
  s_busy: { border: `2px solid ${tokens.colorBrandStroke2}` },
  s_waiting: { border: `2px solid ${tokens.colorPaletteMarigoldBorderActive}`, boxShadow: `0 0 0 4px ${tokens.colorPaletteMarigoldBackground1}` },
  s_done: { border: `2px solid ${tokens.colorPaletteGreenBorderActive}` },
  s_failed: { border: `2px solid ${tokens.colorPaletteRedBorderActive}` },
  s_idle: { opacity: 0.7 },
  vm: { display: "inline-flex", alignItems: "center", gap: tokens.spacingHorizontalXS, padding: `1px ${tokens.spacingHorizontalS}`, borderRadius: tokens.borderRadiusCircular, backgroundColor: tokens.colorNeutralBackground3, fontSize: tokens.fontSizeBase100, lineHeight: tokens.lineHeightBase100, whiteSpace: "nowrap" },
  vmRow: { display: "flex", alignItems: "center", justifyContent: "space-between", gap: tokens.spacingHorizontalS, marginTop: tokens.spacingVerticalXXS },
  usage: { fontSize: tokens.fontSizeBase100, color: tokens.colorNeutralForeground3, whiteSpace: "nowrap" },
  bars: { display: "grid", gridTemplateColumns: "1fr 1fr", columnGap: tokens.spacingHorizontalXS, marginTop: "3px" },
  meters: { display: "flex", flexDirection: "column", gap: "3px", marginTop: tokens.spacingVerticalXS },
  meter: { display: "grid", gridTemplateColumns: "26px 1fr auto", alignItems: "center", columnGap: tokens.spacingHorizontalXS, fontSize: tokens.fontSizeBase100, color: tokens.colorNeutralForeground3 },
  track: { height: "4px", borderRadius: tokens.borderRadiusCircular, backgroundColor: tokens.colorNeutralBackground5, overflow: "hidden" },
  fill: { height: "100%", borderRadius: tokens.borderRadiusCircular, backgroundColor: tokens.colorBrandBackground, transitionProperty: "width", transitionDuration: "400ms" },
  fillHot: { backgroundColor: tokens.colorPaletteRedBackground3 },
  spark: { width: "100%", height: "36px", color: tokens.colorBrandForeground1 },
  vmDot: { width: "8px", height: "8px", borderRadius: "50%", flex: "none" },
  vmPulse: { animationName: { "0%, 100%": { opacity: 1 }, "50%": { opacity: 0.3 } }, animationDuration: "1.2s", animationIterationCount: "infinite" },
  vm_queued: { backgroundColor: tokens.colorNeutralForeground4 },
  vm_preparing: { backgroundColor: tokens.colorPaletteMarigoldBackground3 },
  vm_starting: { backgroundColor: tokens.colorPaletteMarigoldBackground3 },
  vm_booting: { backgroundColor: tokens.colorPaletteMarigoldBackground3 },
  vm_running: { backgroundColor: tokens.colorPaletteGreenBackground3 },
  a_idle: { backgroundColor: tokens.colorNeutralForeground4 },
  a_running: { backgroundColor: tokens.colorBrandBackground },
  a_busy: { backgroundColor: tokens.colorBrandStroke2 },
  a_waiting: { backgroundColor: tokens.colorPaletteMarigoldBackground3 },
  a_done: { backgroundColor: tokens.colorPaletteGreenBackground3 },
  a_failed: { backgroundColor: tokens.colorPaletteRedBackground3 },
  vm_off: { backgroundColor: tokens.colorNeutralForeground4 },
  human: { borderTopStyle: "dashed", borderRightStyle: "dashed", borderBottomStyle: "dashed", borderLeftStyle: "dashed" },
});

const fmtTokens = (n: number) => new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 }).format(n);

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

const statusColor = (s: string) =>
  s === "done" ? "success" : s === "failed" || s === "rejected" ? "danger" : s === "running" ? "brand" : s === "waiting" || s === "needs_input" ? "warning" : "informative";

type LiveData = {
  label: string;
  sub: string;
  human: boolean;
  state: NodeState;
  calls: number;
  tokens: number;
  picked: boolean;
  vm: { state: VmState; count: number; use: VmStats | null } | null;
};
type LiveNode = Node<LiveData, "live">;

/** A coloured dot and the VM's state; pulses while the VM is on its way up. */
function VmPill({ state, count }: { state: VmState | "none"; count?: number }) {
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

/** The agent's status, as a pill like the VM's. */
function AgentPill({ state }: { state: NodeState }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const s = styles as Record<string, string>;
  return (
    <span className={styles.vm}>
      <span className={mergeClasses(styles.vmDot, s[`a_${state}`], state === "running" && styles.vmPulse)} />
      {t("session.agentState")} {t(`session.node.${state}`)}
    </span>
  );
}

const pct = (v: number, max: number) => Math.max(0, Math.min(100, max > 0 ? (v / max) * 100 : 0));

/** CPU and memory of the VM as two thin bars (CPU: of all its virtual cores; memory: of what it was given). */
function Meters({ use }: { use: VmStats }) {
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
function Spark({ samples }: { samples: VmStats[] }) {
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

const gb = (mb: number) => (mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${Math.round(mb)} MB`);

/** The VM line of a node: its state and, once it is sampled, what it takes, in the little room a node has. */
function NodeVm({ vm }: { vm: { state: VmState | "none"; count: number; use: VmStats | null } }) {
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

function LiveNodeView({ data }: NodeProps<LiveNode>) {
  const { t } = useTranslation();
  const styles = useStyles();
  const cls = mergeClasses(styles.node, (styles as Record<string, string>)[`s_${data.state}`], data.human && styles.human, data.picked && styles.picked);
  return (
    <div className={cls}>
      <Handle type="target" position={Position.Left} />
      {!data.human && <Handle type="target" id="order-in" position={Position.Top} style={{ opacity: 0 }} />}
      <div className={styles.nodeTop}>
        <Body1><b>{data.human ? "👤 " : ""}{data.label}</b></Body1>
        {data.state === "running" && <Spinner size="extra-tiny" />}
      </div>
      {data.sub && data.sub !== data.label && <Caption1 block>{data.sub}</Caption1>}
      <div className={styles.vmRow}>
        <AgentPill state={data.state} />
        {(data.calls > 0 || data.tokens > 0) && (
          <span className={styles.usage}>
            {[data.calls > 0 ? t("session.callsCount", { count: data.calls }) : "", data.tokens > 0 ? t("session.tokens", { n: fmtTokens(data.tokens) }) : ""].filter(Boolean).join(" · ")}
          </span>
        )}
      </div>
      {!data.human && <NodeVm vm={data.vm ?? { state: "none", count: 0, use: null }} />}
      <Handle type="source" position={Position.Right} />
      {!data.human && <Handle type="source" id="order-out" position={Position.Bottom} style={{ opacity: 0 }} />}
    </div>
  );
}

const nodeTypes = { live: LiveNodeView };
const ROW_SPREAD = 1.5;

function Graph({ wf, d, pending, picked, onPick }: { wf: { nodes: WNode[]; edges: WEdge[] }; d: Digest; pending: Pending | null; picked: string | null; onPick: (id: string) => void }) {
  const { t } = useTranslation();
  const states = useMemo(() => nodeStates(d, pending ? { node: pending.node, human: pending.human } : null), [d, pending]);
  const vms = useMemo(() => nodeVms(d), [d]);
  const [profileNames, setProfileNames] = useState<Record<string, string>>({});
  useEffect(() => {
    void import("@tauri-apps/api/core").then(({ invoke }) =>
      invoke<{ id: string; name: string }[]>("profiles_list").then((ps) => setProfileNames(Object.fromEntries(ps.map((p) => [p.id, p.name])))),
    ).catch(() => {});
  }, []);
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
        // the live boxes are taller than the editor's: spread the rows so the order arrows between them show
        position: { x: n.x, y: n.y * ROW_SPREAD },
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
        },
      })),
    );
  }, [wf, states, stats, vms, picked, profileNames, t, setNodes]);
  const edges: Edge[] = wf.edges.map((e) => ({
    id: e.id,
    source: e.from,
    target: e.to,
    sourceHandle: e.kind === "before" ? "order-out" : undefined,
    targetHandle: e.kind === "before" ? "order-in" : undefined,
    animated: active.has(e.id),
    label: e.kind === "before" ? t("wf.orderLabel") : undefined,
    style: { strokeDasharray: e.kind === "before" ? "6 4" : undefined, strokeWidth: active.has(e.id) ? 3 : 1.5, opacity: e.kind === "before" ? 0.75 : 1 },
    markerEnd: { type: MarkerType.ArrowClosed },
  }));
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
function Clamp({ text, className }: { text: string; className?: string }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const [open, setOpen] = useState(false);
  const long = text.length > 180 || text.split("\n").length > 3;
  return (
    <div className={className}>
      <div className={mergeClasses(styles.clamp, open && styles.open)}>{text}</div>
      {long && (
        <button type="button" className={styles.link} onClick={(e) => { e.stopPropagation(); setOpen(!open); }}>
          {open ? t("session.less") : t("session.more")}
        </button>
      )}
    </div>
  );
}

/** The picked agent's network traffic, live: what its VM asked the egress proxy for. */
function TrafficView({ calls, d }: { calls: CallInfo[]; d: Digest }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const runs = calls
    .flatMap((c) => c.runs)
    .map((k) => d.runs.get(k))
    .filter((r): r is Run => !!r)
    .sort((a, b) => a.t - b.t);
  const all = runs.flatMap((r) => r.traffic);
  if (runs.length === 0) return <Caption1 className={styles.muted}>{t("session.noCalls")}</Caption1>;
  const count = (k: Traffic["kind"]) => all.filter((x) => x.kind === k).length;
  return (
    <>
      <div className={styles.cardHead}>
        <Badge appearance="tint" color="success">{t("session.traffic.allowed", { count: count("allowed") })}</Badge>
        <Badge appearance="tint" color="danger">{t("session.traffic.denied", { count: count("denied") })}</Badge>
        <Badge appearance="tint" color="brand">{t("session.traffic.injected", { count: count("injected") })}</Badge>
      </div>
      {all.length === 0 && <Caption1 className={styles.muted}>{t("session.traffic.empty")}</Caption1>}
      {runs.map((r, n) =>
        r.traffic.length === 0 ? null : (
          <div key={r.key} className={styles.steps}>
            <Caption1 className={styles.muted}>
              {t("session.run")} {n + 1} · {new Date(r.t * 1000).toLocaleTimeString()}
            </Caption1>
            {r.traffic.map((x, i) => (
              <div key={i} className={mergeClasses(styles.step, styles.mono)}>
                <Caption1 className={styles.muted}>{new Date(x.t * 1000).toLocaleTimeString()} </Caption1>
                <Badge size="extra-small" appearance="filled" color={x.kind === "denied" ? "danger" : x.kind === "injected" ? "brand" : x.kind === "allowed" ? "success" : "informative"}>
                  {t(`session.traffic.kind.${x.kind}`)}
                </Badge>{" "}
                {clip(x.text, 300)}
              </div>
            ))}
          </div>
        ),
      )}
    </>
  );
}

function CallCard({ c, d, names }: { c: CallInfo; d: Digest; names: Record<string, string> }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const open = c.status === "open";
  return (
    <div className={styles.card}>
      <div className={styles.cardHead}>
        <Badge appearance="filled" color={statusColor(open ? "running" : c.status)}>{open ? t("session.node.busy") : t(`runs.status.${c.status}`, c.status)}</Badge>
        <Caption1 className={styles.muted}>{c.parent ? t("session.askedBy", { name: names[c.parent] ?? c.parent }) : t("session.thisIsTheRequest")}</Caption1>
        <Caption1 className={mergeClasses(styles.muted, styles.grow)} style={{ textAlign: "right" }}>{new Date(c.t * 1000).toLocaleTimeString()}</Caption1>
      </div>
      {c.asked && <Clamp className={styles.quote} text={c.asked} />}

      {c.runs.map((k, n) => {
        const r = d.runs.get(k);
        if (!r) return null;
        return (
          <div key={k}>
            <div className={styles.cardHead}>
              {r.status === "running" ? <Spinner size="extra-tiny" /> : <Badge size="small" appearance="tint" color={statusColor(r.status)}>{t(`runs.status.${r.status}`, r.status)}</Badge>}
              <Caption1>{t("session.run")} {n + 1}</Caption1>
              <VmPill state={r.vm} />
              {r.tokens > 0 && <Caption1 className={styles.muted}>{t("session.tokens", { n: fmtTokens(r.tokens) })}</Caption1>}
            </div>
            {r.vm !== "off" && r.stats.length > 0 && (
              <>
                <Meters use={r.stats[r.stats.length - 1]} />
                <Spark samples={r.stats} />
              </>
            )}
            {r.vmLog.length > 1 && (
              <Caption1 block className={styles.muted}>
                {r.vmLog.map((x, i) => `${t(`session.vm.${x.state}`)}${i > 0 ? ` +${Math.max(0, x.t - r.vmLog[i - 1].t)}s` : ""}`).join(" → ")}
              </Caption1>
            )}
            {r.pieces.length > 0 && (
              <details open={r.status === "running" || undefined}>
                <summary className={styles.summaryRow}>{t("session.activity", { count: r.pieces.length })}</summary>
                <div className={styles.steps}>
                  {r.pieces.slice(-30).map((p, i) => (
                    <div key={i} className={mergeClasses(styles.step, p.kind === "tool" && styles.mono)}>{clip(p.text, 600)}</div>
                  ))}
                </div>
              </details>
            )}
            {r.assumptions.length > 0 && (
              <Clamp className={styles.muted} text={`${t("session.assumed")}: ${r.assumptions.join("; ")}`} />
            )}
          </div>
        );
      })}

      {c.decisions.map((x, i) => (
        <div key={i} className={styles.steps}>
          {x.action === "delegate" &&
            (x.detail.calls ?? []).map((k: any, j: number) => (
              <div key={j} className={styles.handoff}>
                <span className={styles.chip} title={k.label}>→ {k.label}</span>
                <Clamp text={k.instructions} />
              </div>
            ))}
          {x.action === "finish" && (
            <div className={x.detail.success === false ? styles.outFail : styles.outDone}>
              <Clamp text={x.detail.result ?? ""} />
            </div>
          )}
          {x.action === "ask" && (
            <div className={styles.outAsk}>
              <Clamp text={(x.detail.questions ?? []).map((q: string) => `? ${q}`).join("\n")} />
            </div>
          )}
          {x.notes && <Clamp className={styles.muted} text={`${t("session.notes")}: ${x.notes}`} />}
        </div>
      ))}
      {c.refused.map((r, i) => <Clamp key={i} className={styles.muted} text={t("session.refused", { reason: r })} />)}
      {!open && c.decisions.length === 0 && c.summary && (
        <div className={c.status === "failed" ? styles.outFail : styles.outDone}><Clamp text={c.summary} /></div>
      )}
    </div>
  );
}

function Timeline({ items, onPick }: { items: Digest["timeline"]; onPick: (node: string) => void }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const dotOf = (x: Digest["timeline"][number]) =>
    x.status === "done" ? styles.dot_done : x.status === "failed" || x.kind === "error" ? styles.dot_failed : x.status === "needs_input" || x.kind === "wait" ? styles.dot_wait : styles.dot_brand;
  const verb = (x: Digest["timeline"][number]) =>
    x.kind === "start" ? t("session.ev.start", { from: x.from || t("session.ev.you") }) : t(`session.ev.${x.kind}${x.kind === "end" ? `_${x.status}` : ""}`, "");
  return (
    <div className={styles.tl}>
      {items.map((x, i) => (
        <div key={i} className={styles.tlItem} onClick={() => x.node && onPick(x.node)}>
          <div className={styles.rail}>
            <span className={mergeClasses(styles.dot, dotOf(x))} />
            {i < items.length - 1 && <span className={styles.line} />}
          </div>
          <div>
            <div className={styles.tlHead}>
              {x.who && <span className={styles.tlWho}>{x.who}</span>}
              <Caption1 className={styles.muted}>{verb(x)}</Caption1>
              <Caption1 className={styles.tlTime}>{new Date(x.t * 1000).toLocaleTimeString()}</Caption1>
            </div>
            {x.text && <Clamp text={x.text} />}
          </div>
        </div>
      ))}
    </div>
  );
}

export function SessionView() {
  const { pid = "", sid = "" } = useParams();
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const [view, setView] = useState<View | null>(null);
  const [wf, setWf] = useState<{ nodes: WNode[]; edges: WEdge[] } | null>(null);
  const [workflows, setWorkflows] = useState<{ id: string; name: string }[]>([]);
  const [log, setLog] = useState<Entry[]>([]);
  const [answer, setAnswer] = useState("");
  const [message, setMessage] = useState("");
  const [title, setTitle] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab_>("chat");
  const [picked, setPicked] = useState<string | null>(null);
  const end = useRef<HTMLDivElement>(null);

  const refresh = useCallback(async () => {
    try {
      setView(await call<View>("session_get", { id: sid }));
    } catch (e) {
      notify(String(e), "error");
    }
  }, [sid, notify]);

  useEffect(() => {
    if (!inTauri()) return;
    let off: (() => void) | undefined;
    let gone = false;
    void (async () => {
      try {
        setWorkflows(await call<{ id: string; name: string }[]>("workflows_list", { project: pid }));
      } catch (e) {
        notify(String(e), "error");
      }
      setLog(await call<Entry[]>("session_log", { id: sid }));
      await refresh();
      const { listen } = await import("@tauri-apps/api/event");
      const u = await listen<{ session: string; entry: Entry }>("session-log", (e) => {
        if (e.payload.session !== sid) return;
        setLog((l) => [...l, e.payload.entry]);
        if (["status", "wait", "reply", "call_end"].includes(e.payload.entry.kind)) void refresh();
      });
      if (gone) u();
      else off = u;
    })();
    return () => {
      gone = true;
      off?.();
    };
  }, [sid, pid, refresh, notify]);

  const wfId = view?.session.workflow;
  useEffect(() => {
    if (!inTauri() || !wfId) return;
    void (async () => {
      try {
        setWf((await call<{ workflow: { nodes: WNode[]; edges: WEdge[] } }>("workflow_get", { id: wfId })).workflow);
      } catch (e) {
        notify(String(e), "error");
      }
    })();
  }, [wfId, notify]);

  const names = useMemo(() => Object.fromEntries((wf?.nodes ?? []).map((n) => [n.id, n.label || n.profile || t("wf.kind.you")])), [wf, t]);
  const d = useMemo(() => digest(log, names), [log, names]);
  const vmAlive = useMemo(() => [...d.runs.values()].filter((r) => r.vm !== "off").length, [d]);

  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [log.length, tab]);

  const act = async (cmd: string, args: Record<string, unknown> = {}) => {
    try {
      await call(cmd, { id: sid, ...args });
      setAnswer("");
      setMessage("");
      await refresh();
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const rename = async () => {
    if (title === null) return;
    const name = title;
    setTitle(null);
    await act("sessions_rename", { name });
  };

  const pick = (node: string) => {
    setPicked(node);
    setTab("agent");
  };

  if (!inTauri()) return <Body1 block>{t("app.desktopOnly")}</Body1>;
  if (!view) return null;
  const { session, pending, live } = view;
  const canStart = session.status === "new";
  const canContinue = !live && ["interrupted", "failed", "cancelled"].includes(session.status);
  // what the root answered to the request and to each message after it, in order
  const turnResults = log
    .filter((e) => e.kind === "call_end" && typeof e.body.call === "string" && /^(r|t\d+)$/.test(e.body.call as string))
    .map((e) => ({ turn: e.body.call === "r" ? 0 : Number((e.body.call as string).slice(1)), status: e.body.status as string, text: String(e.body.summary ?? "") }));
  const turnResult = (i: number) => {
    const all = turnResults.filter((r) => r.turn === i);
    return all[all.length - 1];
  };
  const sendMessage = () => {
    const text = message.trim();
    if (text) void act("session_message", { text });
  };
  const pickedCalls = [...d.calls.values()].filter((c) => c.node === picked).sort((a, b) => b.t - a.t);

  const reply = (approved: boolean | null) => act("session_reply", { reply: { approved, text: answer } });

  return (
    <>
      <div className={styles.head}>
        <Caption1><Link to={`/projects/${pid}/sessions`}>{t("session.back")}</Link></Caption1>
        {title === null ? (
          <div className={styles.title}>
            <Title2 as="h1" className={session.name || session.task ? undefined : styles.muted}>{session.name || (session.task.length > 60 ? `${session.task.slice(0, 60)}…` : session.task) || t("sessions.untitled")}</Title2>
            <Button appearance="subtle" icon={<Edit20Regular />} aria-label={t("session.rename")} onClick={() => setTitle(session.name)} />
          </div>
        ) : (
          <Input
            autoFocus
            maxLength={120}
            placeholder={t("sessions.untitled")}
            value={title}
            onChange={(_, d) => setTitle(d.value)}
            onBlur={rename}
            onKeyDown={(e) => {
              if (e.key === "Enter") void rename();
              if (e.key === "Escape") setTitle(null);
            }}
          />
        )}
      </div>
      <div className={styles.bar}>
        <Badge appearance="filled" color={statusColor(session.status)}>{t(`sessions.status.${session.status}`, session.status)}</Badge>
        {view.tokens > 0 && <Caption1>{t("session.tokens", { n: fmtTokens(view.tokens) })}</Caption1>}
        {vmAlive > 0 && <Caption1>{t("session.vm.alive", { count: vmAlive })}</Caption1>}
        {canContinue && <Button appearance="primary" onClick={() => act("session_continue")}>{t("session.continue")}</Button>}
        {live && <Button onClick={() => act("session_cancel")}>{t("runs.cancel")}</Button>}
        {session.source !== "chat" && <Badge appearance="tint">{t(`session.source.${session.source}`)}</Badge>}
        <span className={styles.grow} />
        <Field className={styles.autonomy} orientation="horizontal" label={t("session.workflow")}>
          <Select
            value={session.workflow}
            disabled={!canStart}
            onChange={async (_, x) => {
              try {
                await call("sessions_workflow", { id: sid, workflow: x.value });
                await refresh();
              } catch (e) {
                notify(String(e), "error");
              }
            }}
          >
            {workflows.map((w) => (
              <option key={w.id} value={w.id}>
                {w.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field className={styles.autonomy} orientation="horizontal" label={t("session.autonomy.label")} title={t(`session.autonomy.${session.autonomy}Hint`)}>
          <Select value={session.autonomy} onChange={(_, x) => act("sessions_autonomy", { autonomy: x.value })}>
            <option value="ask">{t("session.autonomy.ask")}</option>
            <option value="never">{t("session.autonomy.never")}</option>
          </Select>
        </Field>
      </div>

      <div className={styles.body}>
        <div className={styles.canvas}>
          {wf && wf.nodes.length > 0 ? (
            <ReactFlowProvider>
              <Graph wf={wf} d={d} pending={live ? pending : null} picked={picked} onPick={pick} />
            </ReactFlowProvider>
          ) : (
            <Body1 block>{t("session.noWorkflow")}</Body1>
          )}
        </div>

        <div className={styles.side}>
          <TabList className={styles.tabs} size="small" selectedValue={tab} onTabSelect={(_, x) => setTab(x.value as Tab_)}>
            <Tab value="chat">{t("session.tabs.chat")}{pending && live ? " ●" : ""}</Tab>
            <Tab value="timeline">{t("session.tabs.timeline")}</Tab>
            <Tab value="agent">{picked ? clip(names[picked] ?? "", 18) : t("session.tabs.agent")}</Tab>
          </TabList>

          <div className={styles.scroll}>
            {tab === "chat" && (
              <>
                {session.task ? <div className={styles.user}>{session.task}</div> : <Body1 className={styles.muted}>{t("session.firstMessage")}</Body1>}
                {[session.task, ...view.messages].map((m, i) => (
                  <Fragment key={i}>
                    {i > 0 && <div className={styles.user}>{m}</div>}
                    {turnResult(i) && !(live && i === view.messages.length) && (
                      <div className={turnResult(i)?.status === "done" ? styles.outDone : styles.outFail}>{turnResult(i)?.text}</div>
                    )}
                  </Fragment>
                ))}
                {d.errors.map((e, i) => <MessageBar key={i} intent="error"><MessageBarBody>{e}</MessageBarBody></MessageBar>)}
                {pending && live && (
                  <div className={styles.ask}>
                    <Body1><b>{t(`session.ask.${pending.kind}`, { label: pending.label })}</b></Body1>
                    <Body1 className={styles.box}>{pending.kind === "questions" ? [pending.prompt, ...pending.questions.map((q) => `• ${q}`)].filter(Boolean).join("\n") : pending.prompt}</Body1>
                    <Field label={pending.kind === "approve" ? t("session.comment") : t("session.answer")}>
                      <Textarea rows={3} value={answer} onChange={(_, x) => setAnswer(x.value)} />
                    </Field>
                    <div className={styles.actions}>
                      {pending.kind === "approve" ? (
                        <>
                          <Button appearance="primary" onClick={() => reply(true)}>{t("session.approve")}</Button>
                          <Button onClick={() => reply(false)}>{t("session.reject")}</Button>
                        </>
                      ) : (
                        <Button appearance="primary" disabled={!answer.trim()} onClick={() => reply(null)}>{t("session.send")}</Button>
                      )}
                    </div>
                  </div>
                )}
                {view.result && !live && turnResults.length === 0 && (
                  <>
                    <Subtitle2>{t("session.result")}</Subtitle2>
                    <div className={styles.box}>{view.result}</div>
                  </>
                )}
              </>
            )}

            {tab === "timeline" &&
              (d.timeline.length === 0 ? <Caption1 className={styles.muted}>{t("session.noEvents")}</Caption1> : <Timeline items={d.timeline} onPick={pick} />)}

            {tab === "agent" &&
              (!picked ? (
                <Caption1 className={styles.muted}>{t("session.pickAgent")}</Caption1>
              ) : pickedCalls.length === 0 ? (
                <Caption1 className={styles.muted}>{t("session.noCalls")}</Caption1>
              ) : (
                <>
                  <section className={styles.section}>
                    <Subtitle2>{t("session.logs")}</Subtitle2>
                    {pickedCalls.map((c) => <CallCard key={c.path} c={c} d={d} names={names} />)}
                  </section>
                  <section className={styles.section}>
                    <Subtitle2>{t("session.tabs.traffic")}</Subtitle2>
                    <TrafficView calls={pickedCalls} d={d} />
                  </section>
                </>
              ))}
            <div ref={end} />
          </div>

          {!canStart && (
            <div className={styles.composer}>
              <Textarea
                className={styles.grow}
                rows={2}
                placeholder={live ? t("session.followUpQueued") : t("session.followUp")}
                value={message}
                onChange={(_, x) => setMessage(x.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey && message.trim()) {
                    e.preventDefault();
                    sendMessage();
                  }
                }}
              />
              <Button appearance="primary" disabled={!message.trim()} onClick={sendMessage}>
                {t("session.send")}
              </Button>
            </div>
          )}
          {canStart && (
            <div className={styles.composer}>
              <Textarea
                className={styles.grow}
                rows={3}
                autoFocus
                placeholder={t("session.placeholder")}
                value={message}
                onChange={(_, x) => setMessage(x.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey && message.trim()) {
                    e.preventDefault();
                    void act("session_start", { task: message.trim() });
                  }
                }}
              />
              <Button appearance="primary" disabled={!message.trim()} onClick={() => act("session_start", { task: message.trim() })}>
                {t("session.send")}
              </Button>
            </div>
          )}
        </div>
      </div>
    </>
  );
}
