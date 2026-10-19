import type { SessionRow, TaskNode, TaskStatus } from "./types";

export interface RunState {
  running: boolean;
  queued?: boolean;
  stopReason: string | null;
  startedAt?: number;
}

export function deriveStatus(state: RunState | undefined): TaskStatus {
  if (state?.queued) return "queued";
  if (state?.running) return "running";
  if (state?.stopReason === "cancelled") return "stopped";
  if (state?.stopReason === "end_turn" || state?.stopReason === "max_iterations") {
    return "completed";
  }
  return "idle";
}

export function statusLabel(status: TaskStatus): string {
  switch (status) {
    case "running":
      return "运行中";
    case "queued":
      return "排队中";
    case "completed":
      return "已完成";
    case "stopped":
      return "已停止";
    default:
      return "空闲";
  }
}

export function shortId(id: string): string {
  const raw = id.replace(/^tsk_/, "");
  return raw.slice(0, 8);
}

export function formatRelative(iso: string): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "";
  const sec = Math.max(0, Math.round((Date.now() - t) / 1000));
  if (sec < 60) return "刚刚";
  if (sec < 3600) return `${Math.floor(sec / 60)} 分钟前`;
  if (sec < 86400) return `${Math.floor(sec / 3600)} 小时前`;
  return `${Math.floor(sec / 86400)} 天前`;
}

export function buildTaskTree(
  rows: SessionRow[],
  run: Record<string, RunState>,
): TaskNode[] {
  const nodes = new Map<string, TaskNode>();
  for (const row of rows) {
    nodes.set(row.id, {
      ...row,
      status: deriveStatus(run[row.id]),
      children: [],
    });
  }
  const roots: TaskNode[] = [];
  for (const node of nodes.values()) {
    if (node.parentId && nodes.has(node.parentId)) {
      nodes.get(node.parentId)!.children.push(node);
    } else {
      roots.push(node);
    }
  }
  const rank = (n: TaskNode) => Date.parse(n.updatedAt) || 0;
  roots.sort((a, b) => rank(b) - rank(a));
  for (const root of roots) {
    root.children.sort((a, b) => rank(b) - rank(a));
  }
  return roots;
}
