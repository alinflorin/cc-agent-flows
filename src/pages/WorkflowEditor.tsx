import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Background,
  Controls,
  Handle,
  MarkerType,
  Position,
  ReactFlow,
  ReactFlowProvider,
  useEdgesState,
  useNodesState,
  type Connection,
  type Edge,
  type Node,
  type NodeProps,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import {
  Body1,
  Button,
  Caption1,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Select,
  Subtitle2,
  Switch,
  Textarea,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { Delete16Regular, Save16Regular } from "@fluentui/react-icons";
import { useNotify } from "../notifications";

interface Agent {
  profile: string;
  prompt: string;
  max_calls: number;
  timeout_mins: number;
}
type WNode = { id: string; label: string; x: number; y: number } & Agent;
type WEdge = { id: string; from: string; to: string; approval: boolean };
interface Workflow {
  nodes: WNode[];
  edges: WEdge[];
}
interface Problem {
  code: string;
  node: string | null;
}
interface View {
  workflow: Workflow;
  problems: Problem[];
}
interface ProfileInfo {
  id: string;
  name: string;
}

type Data = { label: string; agent: Agent; problem?: boolean; root?: boolean; profileName?: string };
type ANode = Node<Data, "agent">;
type AEdge = Edge<{ approval: boolean }>;

const useStyles = makeStyles({
  root: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM },
  bar: { display: "flex", gap: tokens.spacingHorizontalS, flexWrap: "wrap", alignItems: "center" },
  grow: { flex: 1 },
  body: { display: "flex", gap: tokens.spacingHorizontalM, height: "min(640px, calc(100vh - 340px))", minHeight: "420px" },
  canvas: { flex: 1, minWidth: 0, border: `1px solid ${tokens.colorNeutralStroke2}`, borderRadius: tokens.borderRadiusMedium },
  side: {
    width: "300px",
    overflow: "auto",
    display: "flex",
    flexDirection: "column",
    gap: tokens.spacingVerticalM,
    padding: tokens.spacingHorizontalM,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    borderRadius: tokens.borderRadiusMedium,
  },
  node: {
    minWidth: "150px",
    maxWidth: "220px",
    padding: `${tokens.spacingVerticalS} ${tokens.spacingHorizontalM}`,
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderLeft: `4px solid ${tokens.colorBrandBackground}`,
    backgroundColor: tokens.colorNeutralBackground1,
    color: tokens.colorNeutralForeground1,
  },
  selected: { border: `2px solid ${tokens.colorBrandStroke1}`, borderLeft: `4px solid ${tokens.colorBrandBackground}` },
  warn: { border: `1px solid ${tokens.colorPaletteYellowBorder2}`, borderLeft: `4px solid ${tokens.colorBrandBackground}` },
});

function AgentNodeView({ data, selected }: NodeProps<ANode>) {
  const { t } = useTranslation();
  const styles = useStyles();
  const cls = [styles.node];
  if (data.problem) cls.push(styles.warn);
  if (selected) cls.push(styles.selected);
  return (
    <div className={cls.join(" ")}>
      <Handle type="target" position={Position.Left} />
      <Body1 block>
        <b>{data.label || data.profileName || data.agent.profile}</b>
      </Body1>
      <Caption1 block>{data.profileName || data.agent.profile}</Caption1>
      {data.root && <Caption1 block>★ {t("wf.root")}</Caption1>}
      <Handle type="source" position={Position.Right} />
    </div>
  );
}

const nodeTypes = { agent: AgentNodeView };

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

function Editor({ project }: { project: string }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const [nodes, setNodes, onNodesChange] = useNodesState<ANode>([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState<AEdge>([]);
  const [profiles, setProfiles] = useState<ProfileInfo[]>([]);
  const [problems, setProblems] = useState<Problem[]>([]);
  const [dirty, setDirty] = useState(false);

  const mkEdge = useCallback(
    (id: string, source: string, target: string, approval: boolean): AEdge => ({
      id,
      source,
      target,
      data: { approval },
      label: approval ? t("wf.approvalLabel") : undefined,
      markerEnd: { type: MarkerType.ArrowClosed },
    }),
    [t],
  );

  const load = useCallback(
    (v: View, markDirty: boolean) => {
      setNodes(
        v.workflow.nodes.map((n): ANode => {
          const { id, label, x, y, ...agent } = n;
          return { id, type: "agent", position: { x, y }, data: { label, agent } };
        }),
      );
      setEdges(v.workflow.edges.map((e) => mkEdge(e.id, e.from, e.to, e.approval)));
      setProblems(v.problems);
      setDirty(markDirty);
    },
    [setNodes, setEdges, mkEdge],
  );

  useEffect(() => {
    void (async () => {
      try {
        setProfiles(await call<ProfileInfo[]>("profiles_list"));
        load(await call<View>("workflow_get", { project }), false);
      } catch (e) {
        notify(String(e), "error");
      }
    })();
  }, [project, load, notify]);

  const onConnect = useCallback(
    (c: Connection) => {
      if (!c.source || !c.target || c.source === c.target) return;
      setEdges((es) => (es.some((e) => e.source === c.source && e.target === c.target) ? es : [...es, mkEdge(crypto.randomUUID(), c.source, c.target, false)]));
      setDirty(true);
    },
    [setEdges, mkEdge],
  );

  const add = () => {
    const id = crypto.randomUUID().slice(0, 8);
    const at = nodes.reduce((m, n) => Math.max(m, n.position.x), 0);
    const agent: Agent = { profile: profiles[0]?.id ?? "", prompt: "", max_calls: 20, timeout_mins: 30 };
    setNodes((ns) => [...ns, { id, type: "agent", position: { x: at + 40, y: 200 }, data: { label: "", agent } }]);
    setDirty(true);
  };

  const selected = nodes.find((n) => n.selected);
  const selectedEdge = !selected ? edges.find((e) => e.selected) : undefined;
  const patchData = (id: string, f: (d: Data) => Data) => {
    setNodes((ns) => ns.map((n) => (n.id === id ? { ...n, data: f(n.data) } : n)));
    setDirty(true);
  };
  const patchAgent = (id: string, patch: Partial<Agent>) => patchData(id, (d) => ({ ...d, agent: { ...d.agent, ...patch } }));

  const remove = (id: string) => {
    setNodes((ns) => ns.filter((n) => n.id !== id));
    setEdges((es) => es.filter((e) => e.source !== id && e.target !== id));
    setDirty(true);
  };

  const setApproval = (id: string, approval: boolean) => {
    setEdges((es) => es.map((e) => (e.id === id ? { ...e, data: { approval }, label: approval ? t("wf.approvalLabel") : undefined } : e)));
    setDirty(true);
  };

  const save = async () => {
    const workflow: Workflow = {
      nodes: nodes.map((n) => ({ id: n.id, label: n.data.label, x: Math.round(n.position.x), y: Math.round(n.position.y), ...n.data.agent })),
      edges: edges.map((e) => ({ id: e.id, from: e.source, to: e.target, approval: e.data?.approval ?? false })),
    };
    try {
      const v = await call<View>("workflow_save", { project, workflow });
      setProblems(v.problems);
      setDirty(false);
      notify(t("wf.saved"), "success");
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const useStandard = async () => {
    try {
      load(await call<View>("workflow_standard"), true);
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const flagged = useMemo(() => new Set(problems.map((p) => p.node)), [problems]);
  const roots = useMemo(() => nodes.filter((n) => !edges.some((e) => e.target === n.id)).map((n) => n.id), [nodes, edges]);
  const shown = useMemo(
    () =>
      nodes.map((n) => ({
        ...n,
        data: { ...n.data, problem: flagged.has(n.id), root: roots.length === 1 && roots[0] === n.id, profileName: profiles.find((p) => p.id === n.data.agent.profile)?.name },
      })),
    [nodes, flagged, roots, profiles],
  );
  const nameOf = (id: string | null) => {
    const n = nodes.find((x) => x.id === id);
    return n ? n.data.label || profiles.find((p) => p.id === n.data.agent.profile)?.name || n.data.agent.profile : "";
  };

  const agent = selected?.data.agent;
  const outgoing = selected ? edges.filter((e) => e.source === selected.id).length : 0;
  return (
    <div className={styles.root}>
      <div className={styles.bar}>
        <Button appearance="primary" icon={<Save16Regular />} disabled={!dirty} onClick={save}>
          {t("common.save")}
        </Button>
        <span className={styles.grow} />
        <Button size="small" onClick={add}>
          {t("wf.addAgent")}
        </Button>
        <Button size="small" appearance="subtle" onClick={useStandard}>
          {t("wf.loadStandard")}
        </Button>
      </div>

      {problems.length > 0 && !dirty && (
        <MessageBar intent="warning">
          <MessageBarBody>
            {problems.map((p, i) => (
              <div key={i}>{t(`wf.problem.${p.code}`, { node: nameOf(p.node) })}</div>
            ))}
          </MessageBarBody>
        </MessageBar>
      )}

      <div className={styles.body}>
        <div className={styles.canvas}>
          <ReactFlow
            nodes={shown}
            edges={edges}
            nodeTypes={nodeTypes}
            onNodesChange={(c) => {
              onNodesChange(c);
              if (c.some((x) => x.type !== "select" && x.type !== "dimensions")) setDirty(true);
            }}
            onEdgesChange={(c) => {
              onEdgesChange(c);
              if (c.some((x) => x.type !== "select")) setDirty(true);
            }}
            onConnect={onConnect}
            colorMode="system"
            fitView
            proOptions={{ hideAttribution: true }}
          >
            <Background />
            <Controls showInteractive={false} />
          </ReactFlow>
        </div>

        <div className={styles.side}>
          {selectedEdge ? (
            <>
              <Subtitle2>
                {nameOf(selectedEdge.source)} → {nameOf(selectedEdge.target)}
              </Subtitle2>
              <Field hint={t("wf.approvalHint")}>
                <Switch label={t("wf.approval")} checked={selectedEdge.data?.approval ?? false} onChange={(_, d) => setApproval(selectedEdge.id, d.checked)} />
              </Field>
            </>
          ) : !selected || !agent ? (
            <Caption1>{t("wf.pick")}</Caption1>
          ) : (
            <>
              <Subtitle2>{t("wf.agent")}</Subtitle2>
              <Caption1>{outgoing > 0 ? t("wf.coordinates", { count: outgoing }) : t("wf.worker")}</Caption1>
              <Field label={t("wf.label")}>
                <Input value={selected.data.label} maxLength={120} onChange={(_, d) => patchData(selected.id, (x) => ({ ...x, label: d.value }))} />
              </Field>
              <Field label={t("wf.profile")}>
                <Select value={agent.profile} onChange={(_, d) => patchAgent(selected.id, { profile: d.value })}>
                  {!profiles.some((p) => p.id === agent.profile) && <option value={agent.profile}>{agent.profile}</option>}
                  {profiles.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.name}
                    </option>
                  ))}
                </Select>
              </Field>
              <Field label={t("wf.prompt")} hint={t("wf.agentPromptHint")}>
                <Textarea rows={8} value={agent.prompt} onChange={(_, d) => patchAgent(selected.id, { prompt: d.value })} />
              </Field>
              <Field label={t("wf.maxCalls")} hint={t("wf.maxCallsHint")}>
                <Input
                  type="number"
                  min={1}
                  max={500}
                  value={String(agent.max_calls)}
                  onChange={(_, d) => patchAgent(selected.id, { max_calls: Math.min(500, Math.max(1, Math.floor(Number(d.value)) || 1)) })}
                />
              </Field>
              <Field label={t("wf.timeout")} hint={t("wf.timeoutHint")}>
                <Input
                  type="number"
                  min={1}
                  max={480}
                  value={String(agent.timeout_mins)}
                  onChange={(_, d) => patchAgent(selected.id, { timeout_mins: Math.min(480, Math.max(1, Math.floor(Number(d.value)) || 1)) })}
                />
              </Field>
              <Button icon={<Delete16Regular />} onClick={() => remove(selected.id)}>
                {t("wf.remove")}
              </Button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

export function WorkflowEditor({ project }: { project: string }) {
  return (
    <ReactFlowProvider>
      <Editor project={project} />
    </ReactFlowProvider>
  );
}
