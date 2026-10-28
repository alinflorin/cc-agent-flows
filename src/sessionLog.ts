// Turns a session's log (see engine.rs: call_start, call_end, run_start, run_end, agent, decision,
// wait, reply, error) into what the session page shows.

export interface Entry {
  t: number;
  kind: string;
  body: Record<string, any>;
}

export interface Piece {
  kind: "text" | "tool";
  text: string;
}

/** One line of an agent's network traffic, as the egress proxy logged it. */
export interface Traffic {
  t: number;
  kind: "allowed" | "denied" | "injected" | "other";
  text: string;
}

export interface Run {
  key: string;
  call: string;
  node: string;
  status: "running" | "done" | "needs_input" | "failed";
  cost: number;
  summary: string;
  questions: string[];
  assumptions: string[];
  pieces: Piece[];
  traffic: Traffic[];
  t: number;
}

export interface Decision {
  round: number;
  action: string;
  detail: any;
  notes: string;
  t: number;
}

export interface CallInfo {
  path: string;
  node: string;
  depth: number;
  parent: string | null;
  asked: string;
  status: "open" | "done" | "failed" | "needs_input";
  summary: string;
  questions: string[];
  t: number;
  runs: string[];
  decisions: Decision[];
  refused: string[];
}

export interface TimelineItem {
  t: number;
  kind: "start" | "end" | "decision" | "wait" | "reply" | "error";
  node?: string;
  /** the agent it is about (a name), empty for the user and for errors */
  who: string;
  /** the other side of a handover: who asked */
  from?: string;
  text: string;
  status?: string;
}

export interface Digest {
  calls: Map<string, CallInfo>;
  runs: Map<string, Run>;
  timeline: TimelineItem[];
  errors: string[];
}

export const clip = (s: string, n: number) => (s.length > n ? `${s.slice(0, n)}…` : s);

/** The call a run key belongs to: `r:2:1` and `r~1` both belong to `r`. */
const pathOf = (key: string) => key.split(":")[0].split("~")[0];

export function digest(log: Entry[], names: Record<string, string>): Digest {
  const calls = new Map<string, CallInfo>();
  const runs = new Map<string, Run>();
  const timeline: TimelineItem[] = [];
  const errors: string[] = [];
  const name = (id: string | null | undefined) => (id ? (names[id] ?? id) : "");
  const callOf = (path: string, node: string, t: number): CallInfo => {
    let c = calls.get(path);
    if (!c) {
      c = { path, node, depth: path.split("/").length - 1, parent: null, asked: "", status: "open", summary: "", questions: [], t, runs: [], decisions: [], refused: [] };
      calls.set(path, c);
    }
    return c;
  };

  for (const e of log) {
    const b = e.body;
    switch (e.kind) {
      case "call_start": {
        // a call started again (a continued session) starts over
        const c = callOf(b.call, b.node, e.t);
        Object.assign(c, { node: b.node, depth: b.depth, parent: b.parent ?? null, asked: b.asked ?? "", status: "open", summary: "", questions: [], t: e.t });
        timeline.push({ t: e.t, kind: "start", node: b.node, who: name(b.node), from: name(b.parent), text: clip(b.asked ?? "", 240) });
        break;
      }
      case "call_end": {
        const c = callOf(b.call, b.node, e.t);
        Object.assign(c, { status: b.status, summary: b.summary ?? "", questions: b.questions ?? [] });
        timeline.push({ t: e.t, kind: "end", node: b.node, status: b.status, who: name(b.node), text: clip(b.summary ?? "", 240) });
        break;
      }
      case "run_start": {
        const path = pathOf(b.call);
        const c = callOf(path, b.node, e.t);
        if (!c.runs.includes(b.call)) c.runs.push(b.call);
        runs.set(b.call, { key: b.call, call: path, node: b.node, status: "running", cost: 0, summary: "", questions: [], assumptions: [], pieces: [], traffic: [], t: e.t });
        break;
      }
      case "run_end": {
        const r = runs.get(b.call);
        if (r) Object.assign(r, { status: b.status, cost: b.cost_usd ?? 0, summary: b.summary ?? "", questions: b.questions ?? [], assumptions: b.assumptions ?? [] });
        break;
      }
      case "agent": {
        const r = runs.get(b.call);
        if (r && b.event?.type === "egress") {
          const text = String(b.event.text ?? "").replace(/^egress: /, "");
          const kind = /denied|refused|cannot|failed/.test(text) ? "denied" : / gets /.test(text) ? "injected" : /allowed$/.test(text) ? "allowed" : "other";
          r.traffic.push({ t: e.t, kind, text });
          break;
        }
        if (!r || b.event?.type !== "assistant") break;
        for (const c of b.event.message?.content ?? []) {
          if (c.type === "text" && c.text) r.pieces.push({ kind: "text", text: c.text });
          else if (c.type === "tool_use") r.pieces.push({ kind: "tool", text: `${c.name} ${clip(JSON.stringify(c.input), 160)}` });
        }
        break;
      }
      case "decision": {
        const c = callOf(b.call, b.node, e.t);
        c.decisions.push({ round: b.round, action: b.action, detail: b.detail ?? {}, notes: b.notes ?? "", t: e.t });
        break;
      }
      case "decision_rejected": {
        const c = [...calls.values()].filter((x) => x.node === b.node).pop();
        c?.refused.push(b.reason);
        break;
      }
      case "wait":
        timeline.push({ t: e.t, kind: "wait", node: b.node, who: name(b.node), text: clip(b.kind === "approve" ? b.prompt : (b.questions ?? []).join("\n"), 240) });
        break;
      case "reply":
        timeline.push({ t: e.t, kind: "reply", status: b.approved === true ? "done" : b.approved === false ? "failed" : undefined, who: "", text: clip(b.text ?? "", 240) });
        break;
      case "error":
        errors.push(b.reason);
        timeline.push({ t: e.t, kind: "error", who: "", text: clip(b.reason, 240) });
        break;
    }
  }
  return { calls, runs, timeline, errors };
}

export type NodeState = "idle" | "running" | "busy" | "waiting" | "done" | "failed";

/** What each node of the workflow is doing now. `busy`: called, its own run not going on (it waits for the agents it called). */
export function nodeStates(d: Digest, pending: { node: string; human: string | null } | null): Map<string, NodeState> {
  const out = new Map<string, NodeState>();
  const byNode = new Map<string, CallInfo[]>();
  for (const c of d.calls.values()) byNode.set(c.node, [...(byNode.get(c.node) ?? []), c]);
  for (const [id, calls] of byNode) {
    const running = calls.some((c) => c.runs.some((k) => d.runs.get(k)?.status === "running"));
    const open = calls.some((c) => c.status === "open");
    const latest = [...calls].sort((a, b) => b.t - a.t).find((c) => c.status !== "open");
    out.set(id, running ? "running" : open ? "busy" : latest?.status === "failed" ? "failed" : "done");
  }
  if (pending) {
    out.set(pending.node, "waiting");
    if (pending.human) out.set(pending.human, "waiting");
  }
  return out;
}

/** The workflow edges the work is passing through now. */
export function activeEdges(d: Digest, edges: { id: string; from: string; to: string; kind: string }[], nodeKinds: Map<string, string>): Set<string> {
  const out = new Set<string>();
  for (const c of d.calls.values()) {
    if (c.status !== "open" || !c.parent) continue;
    for (const e of edges.filter((x) => x.kind === "delegate" && x.from === c.parent)) {
      if (e.to === c.node) out.add(e.id);
      else if (nodeKinds.get(e.to) === "approve") {
        const second = edges.find((x) => x.from === e.to && x.to === c.node);
        if (second) {
          out.add(e.id);
          out.add(second.id);
        }
      }
    }
  }
  return out;
}
