import type { Edge, Node } from "@xyflow/react";

export type Kind = "agent" | "ask" | "approve" | "chat" | "cron" | "webhook";
export const TRIGGERS: Kind[] = ["chat", "cron", "webhook"];
export const isTrigger = (k: Kind) => TRIGGERS.includes(k);
export const ICON: Partial<Record<Kind, string>> = { ask: "👤", approve: "👤", chat: "💬", cron: "⏰", webhook: "🔗" };
export interface TriggerCfg {
  cron: string;
  token: string;
  autonomy: "" | "ask" | "never";
}
/** What the backend says about a cron or webhook node. */
export interface TriggerInfo {
  workflow: string;
  node: string;
  kind: "cron" | "webhook";
  next: string | null;
  url: string | null;
  last_run: number | null;
  last_session: string | null;
  last_error: string | null;
}
export interface Agent {
  kind: Kind;
  trigger: TriggerCfg;
  profile: string;
  prompt: string;
  max_calls: number;
  timeout_mins: number;
}
export type WNode = { id: string; label: string; x: number; y: number } & Agent;
export type WEdge = { id: string; from: string; to: string; kind: "delegate" | "before" };
export interface Workflow {
  nodes: WNode[];
  edges: WEdge[];
}
export interface Problem {
  code: string;
  node: string | null;
}
export interface View {
  id: string;
  name: string;
  workflow: Workflow;
  problems: Problem[];
  system_prompt: string;
}
export interface Entry {
  id: string;
  name: string;
  agents: number;
}
export interface ProfileInfo {
  id: string;
  name: string;
}

export type Data = { label: string; agent: Agent; problem?: boolean; root?: boolean; profileName?: string };
export type ANode = Node<Data, "agent">;
export type AEdge = Edge<{ kind: "delegate" | "before" }>;
