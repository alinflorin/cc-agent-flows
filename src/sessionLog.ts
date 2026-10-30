// Turns a session's log (see engine.rs: call_start, call_end, run_start, run_end, agent, decision,
// wait, reply, error) into what the session page shows.

export interface Entry {
  t: number;
  kind: string;
  body: Record<string, any>;
}

interface Piece {
  kind: "text" | "tool";
  text: string;
}

/** One line of an agent's network traffic, as the egress proxy logged it. */
export interface Traffic {
  t: number;
  kind: "allowed" | "denied" | "injected" | "other";
  text: string;
}

/** What a run's VM is doing, as the app reported it (runs.rs `vm_state`). */
export type VmState = "queued" | "preparing" | "starting" | "booting" | "running" | "off";

const STATS_KEPT = 40;

const VM_STATES: VmState[] = ["queued", "preparing", "starting", "booting", "running", "off"];

/** One sample of what a VM takes from the host (QEMU's process): `cpu` in % of one core, memory in MB. */
export interface VmStats {
  cpu: number;
  mem: number;
  memLimit: number;
  cpus: number;
}

/** What a run's traffic has been so far (bytes since the run started, from and to the VM). */
export interface NetTotals {
  up: number;
  down: number;
  open: number;
  connections: number;
}

/** Bytes per second. */
export interface NetSpeed {
  up: number;
  down: number;
}

export interface Run {
  key: string;
  call: string;
  node: string;
  status: "running" | "done" | "needs_input" | "failed";
  tokens: number;
  summary: string;
  questions: string[];
  assumptions: string[];
  pieces: Piece[];
  traffic: Traffic[];
  vm: VmState;
  /** when the VM entered each state */
  vmLog: { state: VmState; t: number }[];
  /** the last samples, oldest first */
  stats: VmStats[];
  net: NetTotals;
  /** the speed between the last reports, and the last ones (oldest first) */
  speed: NetSpeed;
  speeds: NetSpeed[];
  netT: number;
  t: number;
}

interface Decision {
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
  status: "open" | "done" | "failed" | "needs_input" | "unmet";
  summary: string;
  questions: string[];
  t: number;
  runs: string[];
  decisions: Decision[];
  refused: string[];
}

interface TimelineItem {
  t: number;
  kind: "start" | "end" | "decision" | "wait" | "reply" | "error";
  node?: string;
  /** the agent it is about (a name), empty for the user and for errors */
  who: string;
  /** who sent it, when `who` is the receiver (a handover): empty = the user */
  from?: string;
  /** who receives it, when `who` is the sender (a result, questions): empty = the user */
  to?: string;
  text: string;
  status?: string;
}

export interface Digest {
  calls: Map<string, CallInfo>;
  runs: Map<string, Run>;
  timeline: TimelineItem[];
  errors: string[];
}

/** The assistant's text without the JSON answer `agentd` asks it to end with (the final shows it). */
export function withoutAnswer(text: string): string {
  const end = text.lastIndexOf("}");
  if (end < 0) return text;
  for (let i = text.lastIndexOf("{", end); i >= 0; i = text.lastIndexOf("{", i - 1)) {
    try {
      const o = JSON.parse(text.slice(i, end + 1));
      if (o && typeof o === "object" && "status" in o) return (text.slice(0, i) + text.slice(end + 1)).replace(/```(?:json)?\s*```/g, "").trim();
    } catch { /* not the answer */ }
  }
  return text;
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
        timeline.push({ t: e.t, kind: "end", node: b.node, status: b.status, who: name(b.node), to: name(c.parent), text: clip(b.summary ?? "", 240) });
        break;
      }
      case "run_start": {
        const path = pathOf(b.call);
        const c = callOf(path, b.node, e.t);
        if (!c.runs.includes(b.call)) c.runs.push(b.call);
        runs.set(b.call, { key: b.call, call: path, node: b.node, status: "running", tokens: 0, summary: "", questions: [], assumptions: [], pieces: [], traffic: [], vm: "queued", vmLog: [{ state: "queued", t: e.t }], stats: [], net: { up: 0, down: 0, open: 0, connections: 0 }, speed: { up: 0, down: 0 }, speeds: [], netT: e.t, t: e.t });
        break;
      }
      case "run_end": {
        const r = runs.get(b.call);
        if (r && r.vm !== "off") {
          r.vm = "off";
          r.vmLog.push({ state: "off", t: e.t });
        }
        if (r) r.speed = { up: 0, down: 0 };
        if (r) Object.assign(r, { status: b.status, tokens: b.tokens ?? 0, summary: b.summary ?? "", questions: b.questions ?? [], assumptions: b.assumptions ?? [] });
        break;
      }
      case "agent": {
        const r = runs.get(b.call);
        if (r && b.event?.type === "vm") {
          const state = b.event.state as VmState;
          if (VM_STATES.includes(state) && r.vm !== state) {
            r.vm = state;
            r.vmLog.push({ state, t: e.t });
          }
          break;
        }
        if (r && b.event?.type === "vm_stats") {
          r.stats.push({ cpu: Number(b.event.cpu) || 0, mem: Number(b.event.mem_mb) || 0, memLimit: Number(b.event.mem_limit_mb) || 0, cpus: Number(b.event.cpus) || 1 });
          if (r.stats.length > STATS_KEPT) r.stats.shift();
          break;
        }
        if (r && b.event?.type === "net") {
          const up = Number(b.event.up) || 0;
          const down = Number(b.event.down) || 0;
          const secs = Math.max(1, e.t - r.netT);
          r.speed = { up: Math.max(0, (up - r.net.up) / secs), down: Math.max(0, (down - r.net.down) / secs) };
          r.speeds.push(r.speed);
          if (r.speeds.length > STATS_KEPT) r.speeds.shift();
          r.net = { up, down, open: Number(b.event.open) || 0, connections: Number(b.event.connections) || 0 };
          r.netT = e.t;
          break;
        }
        if (r && b.event?.type === "egress") {
          const text = String(b.event.text ?? "").replace(/^egress: /, "");
          const kind = /denied|refused|cannot|failed|budget/.test(text) ? "denied" : / gets /.test(text) ? "injected" : /allowed$/.test(text) ? "allowed" : "other";
          r.traffic.push({ t: e.t, kind, text });
          break;
        }
        if (!r || b.event?.type !== "assistant") break;
        for (const c of b.event.message?.content ?? []) {
          if (c.type === "text" && withoutAnswer(c.text ?? "").trim()) r.pieces.push({ kind: "text", text: withoutAnswer(c.text) });
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

/** The VM state shown on each agent's node: of its runs that are going on, the one furthest along (several calls can run in parallel); else the last run's. `count`: VMs alive now. */
export function nodeVms(d: Digest): Map<string, { state: VmState; count: number; use: VmStats | null }> {
  const out = new Map<string, { state: VmState; count: number; use: VmStats | null }>();
  const byNode = new Map<string, Run[]>();
  for (const r of d.runs.values()) byNode.set(r.node, [...(byNode.get(r.node) ?? []), r]);
  for (const [id, runs] of byNode) {
    const alive = runs.filter((r) => r.vm !== "off");
    if (alive.length) {
      const best = alive.reduce((a, b) => (VM_STATES.indexOf(b.vm) > VM_STATES.indexOf(a.vm) ? b : a));
      // the VMs alive now, added up (the last sample of each)
      const last = alive.map((r) => r.stats[r.stats.length - 1]).filter(Boolean);
      const use = last.length ? last.reduce((a, b) => ({ cpu: a.cpu + b.cpu, mem: a.mem + b.mem, memLimit: a.memLimit + b.memLimit, cpus: a.cpus + b.cpus })) : null;
      out.set(id, { state: best.vm, count: alive.length, use });
    } else out.set(id, { state: "off", count: 0, use: null });
  }
  return out;
}

/** What each agent's traffic has been: bytes over all its runs, the speed of the VMs alive now added up, and the connections open. */
export function nodeNet(d: Digest): Map<string, { up: number; down: number; speed: NetSpeed; open: number }> {
  const out = new Map<string, { up: number; down: number; speed: NetSpeed; open: number }>();
  for (const r of d.runs.values()) {
    const n = out.get(r.node) ?? { up: 0, down: 0, speed: { up: 0, down: 0 }, open: 0 };
    n.up += r.net.up;
    n.down += r.net.down;
    if (r.vm !== "off") {
      n.speed = { up: n.speed.up + r.speed.up, down: n.speed.down + r.speed.down };
      n.open += r.net.open;
    }
    out.set(r.node, n);
  }
  return out;
}
