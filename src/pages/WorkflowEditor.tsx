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
  Menu,
  MenuItem,
  MenuList,
  MenuPopover,
  MenuTrigger,
  MessageBar,
  MessageBarBody,
  Select,
  Subtitle2,
  Textarea,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { Add20Regular, Chat20Regular, Clock20Regular, Copy20Regular, Delete20Regular, Flowchart20Regular, Person20Regular, PlugConnected20Regular, Save20Regular, Send20Regular, ShieldCheckmark20Regular } from "@fluentui/react-icons";
import { useNotify } from "../notifications";

type Kind = "agent" | "ask" | "approve" | "chat" | "cron" | "webhook";
const TRIGGERS: Kind[] = ["chat", "cron", "webhook"];
const isTrigger = (k: Kind) => TRIGGERS.includes(k);
const ICON: Partial<Record<Kind, string>> = { ask: "👤", approve: "👤", chat: "💬", cron: "⏰", webhook: "🔗" };
interface TriggerCfg {
  cron: string;
  token: string;
  autonomy: "" | "ask" | "never";
}
/** What the backend says about a cron or webhook node. */
interface TriggerInfo {
  workflow: string;
  node: string;
  kind: "cron" | "webhook";
  next: string | null;
  url: string | null;
  last_run: number | null;
  last_session: string | null;
  last_error: string | null;
}
interface Agent {
  kind: Kind;
  trigger: TriggerCfg;
  profile: string;
  prompt: string;
  max_calls: number;
  timeout_mins: number;
}
type WNode = { id: string; label: string; x: number; y: number } & Agent;
type WEdge = { id: string; from: string; to: string; kind: "delegate" | "before" };
interface Workflow {
  nodes: WNode[];
  edges: WEdge[];
}
interface Problem {
  code: string;
  node: string | null;
}
interface View {
  id: string;
  name: string;
  workflow: Workflow;
  problems: Problem[];
  system_prompt: string;
}
interface Entry {
  id: string;
  name: string;
  agents: number;
}
interface ProfileInfo {
  id: string;
  name: string;
}

type Data = { label: string; agent: Agent; problem?: boolean; root?: boolean; profileName?: string };
type ANode = Node<Data, "agent">;
type AEdge = Edge<{ kind: "delegate" | "before" }>;

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
    "& > *": { flexShrink: 0 },
  },
  node: {
    overflowWrap: "anywhere",
    minWidth: "150px",
    maxWidth: "220px",
    padding: `${tokens.spacingVerticalS} ${tokens.spacingHorizontalM}`,
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderLeft: `4px solid ${tokens.colorBrandBackground}`,
    backgroundColor: tokens.colorNeutralBackground1,
    color: tokens.colorNeutralForeground1,
  },
  human: { borderLeft: `4px solid ${tokens.colorPaletteMarigoldBorderActive}` },
  trig: { borderLeft: `4px solid ${tokens.colorPaletteGreenBorderActive}` },
  row: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "center" },
  mono: { fontFamily: tokens.fontFamilyMonospace, fontSize: tokens.fontSizeBase200, overflowWrap: "anywhere" },
  selected: { border: `2px solid ${tokens.colorBrandStroke1}`, borderLeft: `4px solid ${tokens.colorBrandBackground}` },
  warn: { border: `1px solid ${tokens.colorPaletteYellowBorder2}`, borderLeft: `4px solid ${tokens.colorBrandBackground}` },
});

function AgentNodeView({ data, selected }: NodeProps<ANode>) {
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

const nodeTypes = { agent: AgentNodeView };

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

function Editor({ id, project, onSaved }: { id: string; project: string; onSaved: (id: string) => void }) {
  const { t } = useTranslation();
  const styles = useStyles();
  const notify = useNotify();
  const [nodes, setNodes, onNodesChange] = useNodesState<ANode>([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState<AEdge>([]);
  const [profiles, setProfiles] = useState<ProfileInfo[]>([]);
  const [problems, setProblems] = useState<Problem[]>([]);
  const [dirty, setDirty] = useState(false);
  const [instructions, setInstructions] = useState("");
  const [name, setName] = useState("");
  const [infos, setInfos] = useState<TriggerInfo[]>([]);
  const [preview, setPreview] = useState<string[] | string>([]);
  const [sample, setSample] = useState('{"event": "example"}');

  const loadInfos = useCallback(async () => {
    try {
      setInfos(await call<TriggerInfo[]>("triggers_status"));
    } catch {
      setInfos([]);
    }
  }, []);
  useEffect(() => {
    void loadInfos();
  }, [loadInfos, id]);

  const mkEdge = useCallback(
    (id: string, source: string, target: string, kind: "delegate" | "before"): AEdge => ({
      id,
      source,
      target,
      data: { kind },
      // order arrows leave from the bottom and enter at the top, so they do not run over the delegate arrows
      sourceHandle: kind === "before" ? "order-out" : undefined,
      targetHandle: kind === "before" ? "order-in" : undefined,
      label: kind === "before" ? t("wf.orderLabel") : undefined,
      style: kind === "before" ? { strokeDasharray: "6 4" } : undefined,
      markerEnd: { type: MarkerType.ArrowClosed },
    }),
    [t],
  );

  const load = useCallback(
    (v: View, markDirty: boolean) => {
      setNodes(
        v.workflow.nodes.map((n): ANode => {
          const { id, label, x, y, ...rest } = n;
          const base: TriggerCfg = { cron: "", token: "", autonomy: "" };
          const agent: Agent = { ...rest, trigger: { ...base, ...rest.trigger } };
          return { id, type: "agent", position: { x, y }, data: { label, agent } };
        }),
      );
      setEdges(v.workflow.edges.map((e) => mkEdge(e.id, e.from, e.to, e.kind)));
      setProblems(v.problems);
      setInstructions(v.system_prompt);
      if (v.name) setName(v.name);
      setDirty(markDirty);
    },
    [setNodes, setEdges, mkEdge],
  );

  useEffect(() => {
    void (async () => {
      try {
        setProfiles(await call<ProfileInfo[]>("profiles_list"));
        if (id) load(await call<View>("workflow_get", { id }), false);
        else {
          load({ id: "", name: "", workflow: { nodes: [], edges: [] }, problems: [], system_prompt: "" }, false);
          setName(t("wf.newName"));
          setDirty(true);
        }
      } catch (e) {
        notify(String(e), "error");
      }
    })();
  }, [id, load, notify, t]);

  const onConnect = useCallback(
    (c: Connection) => {
      if (!c.source || !c.target || c.source === c.target) return;
      const kindOf = (i: string) => nodes.find((n) => n.id === i)?.data.agent.kind;
      const [from, to] = [kindOf(c.source), kindOf(c.target)];
      if (!from || !to) return;
      if (isTrigger(to)) return notify(t("wf.noIntoTrigger"), "error");
      if (isTrigger(from) && (to !== "agent" || edges.some((e) => e.source === c.source))) return notify(t("wf.triggerOneAgent"), "error");
      setEdges((es) => (es.some((e) => e.source === c.source && e.target === c.target) ? es : [...es, mkEdge(crypto.randomUUID(), c.source, c.target, c.sourceHandle === "order-out" || c.targetHandle === "order-in" ? "before" : "delegate")]));
      setDirty(true);
    },
    [setEdges, mkEdge, nodes, edges, notify, t],
  );

  const add = (kind: Kind = "agent") => {
    const id = crypto.randomUUID().slice(0, 8);
    const at = nodes.reduce((m, n) => Math.max(m, n.position.x), 0);
    const agent: Agent = { kind, profile: kind === "agent" ? (profiles[0]?.id ?? "") : "", prompt: "", max_calls: 20, timeout_mins: 30, trigger: { cron: kind === "cron" ? "0 9 * * 1-5" : "", token: "", autonomy: "never" } };
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

  const setEdgeKind = (id: string, kind: "delegate" | "before") => {
    setEdges((es) => es.map((e) => (e.id === id ? mkEdge(e.id, e.source, e.target, kind) : e)));
    setDirty(true);
  };

  const save = async () => {
    const workflow: Workflow = {
      nodes: nodes.map((n) => ({ id: n.id, label: n.data.label, x: Math.round(n.position.x), y: Math.round(n.position.y), ...n.data.agent })),
      edges: edges.map((e) => ({ id: e.id, from: e.source, to: e.target, kind: e.data?.kind ?? "delegate" })),
    };
    try {
      const v = await call<View>("workflow_save", { id, project, name, workflow, systemPrompt: instructions });
      setProblems(v.problems);
      setDirty(false);
      notify(t("wf.saved"), "success");
      onSaved(v.id);
      void loadInfos();
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const useStandard = async () => {
    try {
      const v = await call<View>("workflow_standard");
      load({ ...v, name: "", system_prompt: instructions }, true);
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const flagged = useMemo(() => new Set(problems.map((p) => p.node)), [problems]);
  const roots = useMemo(() => {
    // an agent is called by an agent that points to it, or by a gate that leads to it (its caller points to the gate)
    const kindOf = new Map(nodes.map((n) => [n.id, n.data.agent.kind]));
    const called = new Set(edges.filter((e) => (e.data?.kind ?? "delegate") === "delegate" && ["agent", "approve"].includes(kindOf.get(e.source) ?? "") && kindOf.get(e.target) === "agent").map((e) => e.target));
    return nodes.filter((n) => n.data.agent.kind === "agent" && !called.has(n.id)).map((n) => n.id);
  }, [nodes, edges]);
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
  // the agents it may hand work to (through approval gates too); order arrows are not handovers
  const handsTo = (() => {
    if (!selected) return 0;
    const out = new Set<string>();
    const walk = (id: string, seen: Set<string>) => {
      for (const e of edges) {
        if (e.source !== id || e.data?.kind === "before") continue;
        const n = nodes.find((x) => x.id === e.target);
        if (!n || seen.has(n.id)) continue;
        if (n.data.agent.kind === "agent") out.add(n.id);
        else if (n.data.agent.kind === "approve") walk(n.id, new Set([...seen, n.id]));
      }
    };
    walk(selected.id, new Set([selected.id]));
    return out.size;
  })();
  return (
    <div className={styles.root}>
      <div className={styles.bar}>
        <Button icon={<Add20Regular />} onClick={() => add("agent")}>
          {t("wf.addAgent")}
        </Button>
        <Button icon={<Person20Regular />} onClick={() => add("ask")}>
          {t("wf.addAsk")}
        </Button>
        <Button icon={<ShieldCheckmark20Regular />} onClick={() => add("approve")}>
          {t("wf.addApprove")}
        </Button>
        <Button icon={<Chat20Regular />} onClick={() => add("chat")}>
          {t("wf.addChat")}
        </Button>
        <Button icon={<Clock20Regular />} onClick={() => add("cron")}>
          {t("wf.addCron")}
        </Button>
        <Button icon={<PlugConnected20Regular />} onClick={() => add("webhook")}>
          {t("wf.addWebhook")}
        </Button>
        <span className={styles.grow} />
        <Button icon={<Flowchart20Regular />} onClick={useStandard}>
          {t("wf.loadStandard")}
        </Button>
        <Button appearance="primary" icon={<Save20Regular />} disabled={!dirty || !name.trim()} onClick={save}>
          {t("common.save")}
        </Button>
      </div>

      <Field label={t("wf.name")}>
        <Input
          value={name}
          maxLength={120}
          onChange={(_, d) => {
            setName(d.value);
            setDirty(true);
          }}
        />
      </Field>

      <Field label={t("wf.instructions")} hint={t("wf.instructionsHint")}>
        <Textarea
          rows={3}
          value={instructions}
          onChange={(_, d) => {
            setInstructions(d.value);
            setDirty(true);
          }}
        />
      </Field>

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
              <Field label={t("wf.edgeKind")} hint={t(`wf.edgeKindHint.${selectedEdge.data?.kind ?? "delegate"}`)}>
                <Select value={selectedEdge.data?.kind ?? "delegate"} onChange={(_, d) => setEdgeKind(selectedEdge.id, d.value as "delegate" | "before")}>
                  <option value="delegate">{t("wf.edgeKinds.delegate")}</option>
                  <option value="before">{t("wf.edgeKinds.before")}</option>
                </Select>
              </Field>
            </>
          ) : !selected || !agent ? (
            <Caption1>{t("wf.pick")}</Caption1>
          ) : (
            isTrigger(agent.kind) ? (
              <>
                <Subtitle2>{t(`wf.kind.${agent.kind}`)}</Subtitle2>
                <Caption1>{t(`wf.kindHint.${agent.kind}`)}</Caption1>
                <Field label={t("wf.label")}>
                  <Input value={selected.data.label} maxLength={120} placeholder={t(`wf.kind.${agent.kind}`)} onChange={(_, d) => patchData(selected.id, (x) => ({ ...x, label: d.value }))} />
                </Field>
                {agent.kind === "cron" && (
                  <Field label={t("wf.cron")} hint={typeof preview === "string" ? undefined : t("wf.cronHint")} validationMessage={typeof preview === "string" ? preview : undefined}>
                    <Input
                      className={styles.mono}
                      value={agent.trigger.cron}
                      onChange={(_, d) => {
                        patchAgent(selected.id, { trigger: { ...agent.trigger, cron: d.value } });
                        call<string[]>("cron_preview", { cron: d.value }).then(setPreview, (e) => setPreview(String(e)));
                      }}
                      onFocus={() => call<string[]>("cron_preview", { cron: agent.trigger.cron }).then(setPreview, (e) => setPreview(String(e)))}
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
                    <Textarea rows={5} value={agent.prompt} onChange={(_, d) => patchAgent(selected.id, { prompt: d.value })} />
                  </Field>
                )}
                {agent.kind !== "chat" && (
                  <Field label={t("wf.triggerAutonomy")} hint={t("wf.triggerAutonomyHint")}>
                    <Select value={agent.trigger.autonomy || "never"} onChange={(_, d) => patchAgent(selected.id, { trigger: { ...agent.trigger, autonomy: d.value as "ask" | "never" } })}>
                      <option value="never">{t("session.autonomy.never")}</option>
                      <option value="ask">{t("session.autonomy.ask")}</option>
                    </Select>
                  </Field>
                )}
                {(() => {
                  const info = infos.find((i) => i.workflow === id && i.node === selected.id);
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
                        onClick={async () => {
                          try {
                            await call<string>("trigger_fire", { workflow: id, node: selected.id, payload: info.kind === "webhook" ? sample : null });
                            notify(t("wf.fired"), "success");
                          } catch (e) {
                            notify(String(e), "error");
                          }
                          void loadInfos();
                        }}
                      >
                        {info.kind === "webhook" ? t("wf.webhookTest") : t("wf.cronRunNow")}
                      </Button>
                      {info.next && <Caption1 block>{t("wf.cronNextRun", { when: info.next })}</Caption1>}
                      {info.last_run && <Caption1 block>{t("wf.lastRun", { when: new Date(info.last_run * 1000).toLocaleString() })}</Caption1>}
                      {info.last_error && <Caption1 block style={{ color: tokens.colorPaletteRedForeground1 }}>{info.last_error}</Caption1>}
                    </>
                  );
                })()}
                <Button icon={<Delete20Regular />} onClick={() => remove(selected.id)}>
                  {t("wf.removeNode")}
                </Button>
              </>
            ) : agent.kind !== "agent" ? (
              <>
                <Subtitle2>{t(`wf.kind.${agent.kind}`)}</Subtitle2>
                <Caption1>{t(`wf.kindHint.${agent.kind}`)}</Caption1>
                <Field label={t("wf.label")}>
                  <Input value={selected.data.label} maxLength={120} placeholder={t("wf.kind.you")} onChange={(_, d) => patchData(selected.id, (x) => ({ ...x, label: d.value }))} />
                </Field>
                <Button icon={<Delete20Regular />} onClick={() => remove(selected.id)}>
                  {t("wf.removeNode")}
                </Button>
              </>
            ) : (
            <>
              <Subtitle2>{t("wf.agent")}</Subtitle2>
              <Caption1>{handsTo > 0 ? t("wf.coordinates", { count: handsTo }) : t("wf.worker")}</Caption1>
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
              <Button icon={<Delete20Regular />} onClick={() => remove(selected.id)}>
                {t("wf.remove")}
              </Button>
            </>
            )
          )}
        </div>
      </div>
    </div>
  );
}

export function WorkflowEditor({ project }: { project: string }) {
  const { t } = useTranslation();
  const notify = useNotify();
  const [list, setList] = useState<Entry[] | null>(null);
  const [id, setId] = useState("");

  const refresh = useCallback(
    async (select?: string) => {
      try {
        const l = await call<Entry[]>("workflows_list", { project });
        setList(l);
        setId((cur) => (select !== undefined ? select : l.some((w) => w.id === cur) ? cur : (l[0]?.id ?? "")));
      } catch (e) {
        notify(String(e), "error");
      }
    },
    [notify, project],
  );
  useEffect(() => {
    void refresh();
  }, [refresh]);

  const [others, setOthers] = useState<{ id: string; name: string }[]>([]);
  useEffect(() => {
    void call<{ id: string; name: string }[]>("projects_list").then((l) => setOthers(l), () => setOthers([]));
  }, [project]);

  const copyTo = async (to: string) => {
    try {
      const copy = await call<string>("workflow_copy", { id, project: to });
      notify(t("wf.copied"), "success");
      if (to === project) await refresh(copy);
    } catch (e) {
      notify(String(e), "error");
    }
  };

  const remove = async () => {
    try {
      await call("workflow_delete", { id });
      await refresh("");
      setId("");
      await refresh();
    } catch (e) {
      notify(String(e), "error");
    }
  };

  if (!list) return null;
  return (
    <ReactFlowProvider>
      <div style={{ display: "flex", gap: tokens.spacingHorizontalS, alignItems: "center", marginBottom: tokens.spacingVerticalM }}>
        <Select aria-label={t("wf.workflow")} style={{ minWidth: 260 }} value={id} onChange={(_, d) => setId(d.value)}>
          {!id && <option value="">{t("wf.newName")}</option>}
          {list.map((w) => (
            <option key={w.id} value={w.id}>
              {w.name}
            </option>
          ))}
        </Select>
        <Button icon={<Add20Regular />} disabled={!id} onClick={() => setId("")}>
          {t("wf.new")}
        </Button>
        <Menu>
          <MenuTrigger disableButtonEnhancement>
            <Button icon={<Copy20Regular />} disabled={!id}>
              {t("wf.copyTo")}
            </Button>
          </MenuTrigger>
          <MenuPopover>
            <MenuList>
              {others.map((p) => (
                <MenuItem key={p.id} onClick={() => void copyTo(p.id)}>
                  {p.name}
                </MenuItem>
              ))}
            </MenuList>
          </MenuPopover>
        </Menu>
        <Button icon={<Delete20Regular />} disabled={!id} onClick={remove}>
          {t("wf.delete")}
        </Button>
      </div>
      <Editor key={`${project}/${id || "new"}`} id={id} project={project} onSaved={(saved) => void refresh(saved)} />
    </ReactFlowProvider>
  );
}
