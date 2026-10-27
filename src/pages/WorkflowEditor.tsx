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

export type Step =
  | { type: "start" }
  | { type: "agent"; profile: string; prompt: string; schema: string; max_runs: number; min_instances: number; max_instances: number; timeout_mins: number }
  | { type: "condition"; field: string; op: "eq" | "ne" | "contains"; value: string }
  | { type: "human"; mode: "approve" | "input"; prompt: string }
  | { type: "end"; outcome: "success" | "failure" };

type WNode = { id: string; label: string; x: number; y: number } & Step;
type WEdge = { id: string; from: string; to: string; branch: string | null };
interface Workflow {
  nodes: WNode[];
  edges: WEdge[];
  coordinator: string | null;
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

type Data = { label: string; step: Step; problem?: boolean; coordinator?: boolean };
type SNode = Node<Data, "step">;

function branchesOf(step: Step): string[] {
  if (step.type === "condition") return ["true", "false"];
  if (step.type === "human" && step.mode === "approve") return ["approved", "rejected"];
  return [];
}

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
    backgroundColor: tokens.colorNeutralBackground1,
    color: tokens.colorNeutralForeground1,
    position: "relative",
  },
  selected: { border: `2px solid ${tokens.colorBrandStroke1}` },
  warn: { border: `1px solid ${tokens.colorPaletteYellowBorder2}` },
  agent: { borderLeft: `4px solid ${tokens.colorBrandBackground}` },
  condition: { borderLeft: `4px solid ${tokens.colorPalettePurpleBorderActive}` },
  human: { borderLeft: `4px solid ${tokens.colorPaletteMarigoldBorderActive}` },
  terminal: { borderRadius: "999px", textAlign: "center", minWidth: "80px" },
  branch: { position: "absolute", right: "-8px", fontSize: "10px", transform: "translateX(100%)", color: tokens.colorNeutralForeground3 },
  mono: { fontFamily: tokens.fontFamilyMonospace },
});

function StepNodeView({ data, selected }: NodeProps<SNode>) {
  const { t } = useTranslation();
  const styles = useStyles();
  const { step } = data;
  const branches = branchesOf(step);
  const sub =
    step.type === "agent"
      ? step.max_instances > 1
        ? `${step.profile} ×${step.min_instances === step.max_instances ? step.max_instances : `${step.min_instances}–${step.max_instances}`}`
        : step.profile
      : step.type === "condition"
        ? `${step.field} ${t(`wf.op.${step.op}`)} ${step.value}`
        : step.type === "human"
          ? t(`wf.mode.${step.mode}`)
          : step.type === "end"
            ? t(`wf.outcome.${step.outcome}`)
            : "";
  const cls = [styles.node, styles[step.type === "agent" || step.type === "condition" || step.type === "human" ? step.type : "terminal"]];
  if (step.type === "start" || step.type === "end") cls.push(styles.terminal);
  if (data.problem) cls.push(styles.warn);
  if (selected) cls.push(styles.selected);
  return (
    <div className={cls.join(" ")}>
      {step.type !== "start" && <Handle type="target" position={Position.Left} />}
      <Body1 block>
        <b>{data.label || t(`wf.type.${step.type}`)}</b>
      </Body1>
      {sub && <Caption1 block>{sub}</Caption1>}
      {data.coordinator && <Caption1 block>★ {t("wf.coordinator")}</Caption1>}
      {branches.length === 0 && step.type !== "end" && <Handle type="source" position={Position.Right} />}
      {branches.map((b, i) => (
        <Handle key={b} id={b} type="source" position={Position.Right} style={{ top: i === 0 ? "30%" : "70%" }}>
          <span className={styles.branch} style={{ top: -6 }}>
            {t(`wf.branch.${b}`)}
          </span>
        </Handle>
      ))}
    </div>
  );
}

const nodeTypes = { step: StepNodeView };

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

function Editor({ project }: { project: string }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const [nodes, setNodes, onNodesChange] = useNodesState<SNode>([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>([]);
  const [profiles, setProfiles] = useState<ProfileInfo[]>([]);
  const [problems, setProblems] = useState<Problem[]>([]);
  const [dirty, setDirty] = useState(false);
  const [coordinator, setCoordinator] = useState<string | null>(null);

  const edgeLabel = useCallback((b: string | null) => (b ? t(`wf.branch.${b}`) : undefined), [t]);

  const load = useCallback(
    (v: View, markDirty: boolean) => {
      setNodes(
        v.workflow.nodes.map((n): SNode => {
          const { id, label, x, y, ...step } = n;
          return { id, type: "step", position: { x, y }, data: { label, step: step as Step }, deletable: step.type !== "start" };
        }),
      );
      setEdges(
        v.workflow.edges.map((e) => ({
          id: e.id,
          source: e.from,
          target: e.to,
          sourceHandle: e.branch,
          label: edgeLabel(e.branch),
          markerEnd: { type: MarkerType.ArrowClosed },
        })),
      );
      setCoordinator(v.workflow.coordinator);
      setProblems(v.problems);
      setDirty(markDirty);
    },
    [setNodes, setEdges, edgeLabel],
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
      const branch = c.sourceHandle ?? null;
      setEdges((es) => [
        // a branch has one exit
        ...es.filter((e) => !(branch && e.source === c.source && e.sourceHandle === branch)),
        {
          id: crypto.randomUUID(),
          source: c.source,
          target: c.target,
          sourceHandle: branch,
          label: edgeLabel(branch),
          markerEnd: { type: MarkerType.ArrowClosed },
        },
      ]);
      setDirty(true);
    },
    [setEdges, edgeLabel],
  );

  const add = (step: Step) => {
    const id = crypto.randomUUID().slice(0, 8);
    const at = nodes.reduce((m, n) => Math.max(m, n.position.x), 0);
    setNodes((ns) => [...ns, { id, type: "step", position: { x: at + 40, y: 300 }, data: { label: "", step } }]);
    setDirty(true);
  };

  const selected = nodes.find((n) => n.selected);
  const patchData = (id: string, f: (d: Data) => Data) => {
    setNodes((ns) => ns.map((n) => (n.id === id ? { ...n, data: f(n.data) } : n)));
    setDirty(true);
  };
  const patchStep = (id: string, patch: Partial<Step>) =>
    patchData(id, (d) => ({ ...d, step: { ...d.step, ...patch } as Step }));

  const remove = (id: string) => {
    if (id === coordinator) setCoordinator(null);
    setNodes((ns) => ns.filter((n) => n.id !== id));
    setEdges((es) => es.filter((e) => e.source !== id && e.target !== id));
    setDirty(true);
  };

  const save = async () => {
    const workflow: Workflow = {
      nodes: nodes.map((n) => ({ id: n.id, label: n.data.label, x: Math.round(n.position.x), y: Math.round(n.position.y), ...n.data.step })),
      edges: edges.map((e) => ({ id: e.id, from: e.source, to: e.target, branch: e.sourceHandle ?? null })),
      coordinator,
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
  const shown = useMemo(
    () => nodes.map((n) => ({ ...n, data: { ...n.data, problem: flagged.has(n.id), coordinator: n.id === coordinator } })),
    [nodes, flagged, coordinator],
  );
  const nameOf = (id: string | null) => {
    const n = nodes.find((x) => x.id === id);
    return n ? n.data.label || t(`wf.type.${n.data.step.type}`) : "";
  };

  const step = selected?.data.step;
  return (
    <div className={styles.root}>
      <div className={styles.bar}>
        <Button appearance="primary" icon={<Save16Regular />} disabled={!dirty} onClick={save}>
          {t("common.save")}
        </Button>
        <span className={styles.grow} />
        <Caption1>{t("wf.add")}</Caption1>
        <Button size="small" onClick={() => add({ type: "agent", profile: profiles[0]?.id ?? "", prompt: "", schema: "", max_runs: 3, min_instances: 1, max_instances: 1, timeout_mins: 30 })}>
          {t("wf.type.agent")}
        </Button>
        <Button size="small" onClick={() => add({ type: "condition", field: "status", op: "eq", value: "done" })}>
          {t("wf.type.condition")}
        </Button>
        <Button size="small" onClick={() => add({ type: "human", mode: "approve", prompt: "" })}>
          {t("wf.type.human")}
        </Button>
        <Button size="small" onClick={() => add({ type: "end", outcome: "success" })}>
          {t("wf.type.end")}
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
          {!selected || !step ? (
            <Caption1>{t("wf.pick")}</Caption1>
          ) : (
            <>
              <Subtitle2>{t(`wf.type.${step.type}`)}</Subtitle2>
              {step.type !== "start" && (
                <Field label={t("wf.label")}>
                  <Input value={selected.data.label} maxLength={120} onChange={(_, d) => patchData(selected.id, (x) => ({ ...x, label: d.value }))} />
                </Field>
              )}
              {step.type === "agent" && (
                <>
                  <Field label={t("wf.profile")}>
                    <Select value={step.profile} onChange={(_, d) => patchStep(selected.id, { profile: d.value })}>
                      {!profiles.some((p) => p.id === step.profile) && <option value={step.profile}>{step.profile}</option>}
                      {profiles.map((p) => (
                        <option key={p.id} value={p.id}>
                          {p.name}
                        </option>
                      ))}
                    </Select>
                  </Field>
                  <Field label={t("wf.prompt")} hint={t("wf.agentPromptHint")}>
                    <Textarea rows={6} value={step.prompt} onChange={(_, d) => patchStep(selected.id, { prompt: d.value })} />
                  </Field>
                  <Field label={t("wf.schema")} hint={t("wf.schemaHint")}>
                    <Textarea rows={4} className={styles.mono} value={step.schema} onChange={(_, d) => patchStep(selected.id, { schema: d.value })} />
                  </Field>
                  <Field hint={t("wf.coordinatorHint")}>
                    <Switch
                      label={t("wf.coordinator")}
                      checked={coordinator === selected.id}
                      onChange={(_, d) => {
                        setCoordinator(d.checked ? selected.id : null);
                        setDirty(true);
                      }}
                    />
                  </Field>
                  <Field label={t("wf.instances")} hint={t("wf.instancesHint")}>
                    <div className={styles.bar}>
                      <Input
                        type="number"
                        min={1}
                        max={10}
                        contentBefore={t("wf.min")}
                        value={String(step.min_instances)}
                        onChange={(_, d) => {
                          const min = Math.min(10, Math.max(1, Number(d.value) || 1));
                          patchStep(selected.id, { min_instances: min, max_instances: Math.max(min, step.max_instances) });
                        }}
                      />
                      <Input
                        type="number"
                        min={1}
                        max={10}
                        contentBefore={t("wf.max")}
                        value={String(step.max_instances)}
                        onChange={(_, d) => {
                          const max = Math.min(10, Math.max(1, Number(d.value) || 1));
                          patchStep(selected.id, { max_instances: max, min_instances: Math.min(max, step.min_instances) });
                        }}
                      />
                    </div>
                  </Field>
                  <Field label={t("wf.timeout")} hint={t("wf.timeoutHint")}>
                    <Input
                      type="number"
                      min={1}
                      max={480}
                      value={String(step.timeout_mins)}
                      onChange={(_, d) => patchStep(selected.id, { timeout_mins: Math.min(480, Math.max(1, Math.floor(Number(d.value)) || 1)) })}
                    />
                  </Field>
                  <Field label={t("wf.maxRuns")} hint={t("wf.maxRunsHint")}>
                    <Input
                      type="number"
                      min={1}
                      max={100}
                      value={String(step.max_runs)}
                      onChange={(_, d) => patchStep(selected.id, { max_runs: Math.min(100, Math.max(1, Number(d.value) || 1)) })}
                    />
                  </Field>
                </>
              )}
              {step.type === "condition" && (
                <>
                  <Field label={t("wf.field")} hint={t("wf.fieldHint")}>
                    <Input className={styles.mono} value={step.field} onChange={(_, d) => patchStep(selected.id, { field: d.value })} />
                  </Field>
                  <Field label={t("wf.operator")}>
                    <Select value={step.op} onChange={(_, d) => patchStep(selected.id, { op: d.value as "eq" | "ne" | "contains" })}>
                      {(["eq", "ne", "contains"] as const).map((o) => (
                        <option key={o} value={o}>
                          {t(`wf.op.${o}`)}
                        </option>
                      ))}
                    </Select>
                  </Field>
                  <Field label={t("wf.value")}>
                    <Input value={step.value} onChange={(_, d) => patchStep(selected.id, { value: d.value })} />
                  </Field>
                </>
              )}
              {step.type === "human" && (
                <>
                  <Field label={t("wf.mode.label")} hint={t("wf.mode.hint")}>
                    <Select
                      value={step.mode}
                      onChange={(_, d) => {
                        patchStep(selected.id, { mode: d.value as "approve" | "input" });
                        // the exits change, so the old ones no longer apply
                        setEdges((es) => es.filter((e) => e.source !== selected.id));
                      }}
                    >
                      <option value="approve">{t("wf.mode.approve")}</option>
                      <option value="input">{t("wf.mode.input")}</option>
                    </Select>
                  </Field>
                  <Field label={t("wf.prompt")} hint={t("wf.humanPromptHint")}>
                    <Textarea rows={5} value={step.prompt} onChange={(_, d) => patchStep(selected.id, { prompt: d.value })} />
                  </Field>
                </>
              )}
              {step.type === "end" && (
                <Field label={t("wf.outcome.label")}>
                  <Select value={step.outcome} onChange={(_, d) => patchStep(selected.id, { outcome: d.value as "success" | "failure" })}>
                    <option value="success">{t("wf.outcome.success")}</option>
                    <option value="failure">{t("wf.outcome.failure")}</option>
                  </Select>
                </Field>
              )}
              {step.type !== "start" && (
                <Button icon={<Delete16Regular />} onClick={() => remove(selected.id)}>
                  {t("wf.remove")}
                </Button>
              )}
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
