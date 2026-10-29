import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Background, Controls, MarkerType, ReactFlow, ReactFlowProvider, useEdgesState, useNodesState, type Connection } from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { Button, Caption1, Field, Input, MessageBar, MessageBarBody, Select, Textarea, tokens } from "@fluentui/react-components";
import { Add20Regular, Chat20Regular, Clock20Regular, Delete20Regular, Flowchart20Regular, Person20Regular, PlugConnected20Regular, Save20Regular, ShieldCheckmark20Regular } from "@fluentui/react-icons";
import { useNotify } from "../../notifications";
import { call } from "../../tauri";
import { useProjectId } from "../../project";
import { useStyles } from "./styles";
import { AEdge, ANode, Agent, Data, Entry, Kind, Problem, ProfileInfo, TriggerCfg, TriggerInfo, View, Workflow, isTrigger } from "./types";
import { nodeTypes } from "./AgentNode";
import { AgentPanel, EdgePanel, HumanPanel, TriggerPanel } from "./Panels";
import { useConfirmDelete } from "../../components/Confirm";

function Editor({ id, onSaved }: { id: string; onSaved: (id: string) => void }) {
  const { t } = useTranslation();
  const project = useProjectId();
  const styles = useStyles();
  const notify = useNotify();
  const [nodes, setNodes, onNodesChange] = useNodesState<ANode>([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState<AEdge>([]);
  const [profiles, setProfiles] = useState<ProfileInfo[]>([]);
  const [problems, setProblems] = useState<Problem[]>([]);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [instructions, setInstructions] = useState("");
  const [name, setName] = useState("");
  const [infos, setInfos] = useState<TriggerInfo[]>([]);
  const [preview, setPreview] = useState<string[] | string>([]);
  const [sample, setSample] = useState('{"event": "example"}');

  const loadInfos = useCallback(async () => {
    try {
      setInfos(await call<TriggerInfo[]>("triggers_status", { project }));
    } catch {
      setInfos([]);
    }
  }, [project]);
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
      markerEnd: { type: MarkerType.ArrowClosed } }),
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
        setProfiles(await call<ProfileInfo[]>("profiles_list", { project }));
        if (id) load(await call<View>("workflow_get", { project, id }), false);
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
    // right of the rightmost node (they are 190 wide), so a new one never lands on another
    const at = nodes.reduce((m, n) => Math.max(m, n.position.x), 0);
    const agent: Agent = { kind, profile: kind === "agent" ? (profiles[0]?.id ?? "") : "", prompt: "", max_calls: 20, timeout_mins: 30, trigger: { cron: kind === "cron" ? "0 9 * * 1-5" : "", token: "", autonomy: "never" } };
    setNodes((ns) => [...ns, { id, type: "agent", position: { x: at + 230, y: 200 }, data: { label: "", agent } }]);
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
    setSaving(true);
    const workflow: Workflow = {
      nodes: nodes.map((n) => ({ id: n.id, label: n.data.label, x: Math.round(n.position.x), y: Math.round(n.position.y), ...n.data.agent })),
      edges: edges.map((e) => ({ id: e.id, from: e.source, to: e.target, kind: e.data?.kind ?? "delegate" })) };
    try {
      const v = await call<View>("workflow_save", { project, id, name, workflow, systemPrompt: instructions });
      setProblems(v.problems);
      setDirty(false);
      notify(t("wf.saved"), "success");
      onSaved(v.id);
      void loadInfos();
    } catch (e) {
      notify(String(e), "error");
    }
    setSaving(false);
  };

  const useStandard = async () => {
    try {
      const v = await call<View>("workflow_standard", { project });
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
        data: { ...n.data, problem: flagged.has(n.id), root: roots.length === 1 && roots[0] === n.id, profileName: profiles.find((p) => p.id === n.data.agent.profile)?.name } })),
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
        <Button appearance="primary" icon={<Save20Regular />} disabled={saving || !dirty || !name.trim()} onClick={save}>
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
            <EdgePanel edge={selectedEdge} nameOf={nameOf} setEdgeKind={setEdgeKind} />
          ) : !selected || !agent ? (
            <Caption1>{t("wf.pick")}</Caption1>
          ) : isTrigger(agent.kind) ? (
            <TriggerPanel node={selected} id={id} dirty={dirty} infos={infos} preview={preview} setPreview={setPreview} sample={sample} setSample={setSample} patchData={patchData} patchAgent={patchAgent} remove={remove} loadInfos={loadInfos} />
          ) : agent.kind !== "agent" ? (
            <HumanPanel node={selected} patchData={patchData} remove={remove} />
          ) : (
            <AgentPanel node={selected} profiles={profiles} handsTo={handsTo} patchData={patchData} patchAgent={patchAgent} remove={remove} />
          )}
        </div>
      </div>
    </div>
  );
}

export function WorkflowEditor() {
  const { t } = useTranslation();
  const project = useProjectId();
  const notify = useNotify();
  const [list, setList] = useState<Entry[] | null>(null);
  const [id, setId] = useState("");
  const confirm = useConfirmDelete();

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

  const remove = async () => {
    try {
      await call("workflow_delete", { project, id });
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
        <Button icon={<Delete20Regular />} disabled={!id} onClick={() => confirm.ask(list.find((w) => w.id === id)?.name ?? "", remove)}>
          {t("wf.delete")}
        </Button>
      </div>
      {confirm.dialog}
      <Editor key={id || "new"} id={id} onSaved={(saved) => void refresh(saved)} />
    </ReactFlowProvider>
  );
}
