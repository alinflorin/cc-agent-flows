import type { Node } from "@xyflow/react";
import type { NetSpeed, NodeState, VmState, VmStats } from "../../sessionLog";

export interface Pending {
  id: string;
  node: string;
  label: string;
  kind: "approve" | "questions";
  prompt: string;
  questions: string[];
  human: string | null;
}

export interface View {
  session: { name: string; task: string; status: string; workflow: string; autonomy: "ask" | "never"; source: "chat" | "cron" | "webhook" };
  messages: string[];
  pending: Pending | null;
  live: boolean;
  tokens: number;
  result: string | null;
}

export interface WNode {
  id: string;
  label: string;
  x: number;
  y: number;
  kind: "agent" | "ask" | "approve";
  profile: string;
}
export interface WEdge {
  id: string;
  from: string;
  to: string;
  kind: "delegate" | "before";
}

export type Tab_ = "chat" | "timeline" | "agent";

export const fmtTokens = (n: number) => new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 }).format(n);

/** 1536 -> "1.5 KB". */
export const fmtBytes = (n: number) => {
  if (n < 1000) return `${Math.round(n)} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1000;
  let i = 0;
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000;
    i++;
  }
  return `${v >= 100 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
};
export const fmtRate = (n: number) => `${fmtBytes(n)}/s`;

export const statusColor = (s: string) =>
  s === "done" ? "success" : s === "failed" || s === "rejected" ? "danger" : s === "running" ? "brand" : s === "waiting" || s === "needs_input" ? "warning" : "informative";

export type LiveData = {
  label: string;
  sub: string;
  human: boolean;
  state: NodeState;
  calls: number;
  tokens: number;
  picked: boolean;
  vm: { state: VmState; count: number; use: VmStats | null } | null;
  net: { up: number; down: number; speed: NetSpeed; open: number } | null;
};
export type LiveNode = Node<LiveData, "live">;

