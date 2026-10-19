import type { ChatMessage } from "./types";

/** 判断一条 tool 消息是不是 todo 列表卡片（进度标记，不走思考过程分组） */
export function isTodoCard(msg: ChatMessage): boolean {
  if (msg.role !== "tool") return false;
  const text = msg.text || "";
  const start = text.indexOf("{");
  if (start < 0) return false;
  try {
    const parsed = JSON.parse(text.slice(start)) as { todos?: unknown[] };
    return Array.isArray(parsed.todos);
  } catch {
    return false;
  }
}

export type GroupItem =
  | { type: "msg"; msg: ChatMessage }
  /** 中间旁白：同一轮里后面还有工具调用的 assistant 文本。它不是结论，
   *  留在主流程里会呈现"结论下面还有思考过程"的错觉，必须降级展示。 */
  | { type: "narration"; msg: ChatMessage }
  | { type: "thinking"; items: ChatMessage[] };

/**
 * assistant 文本是不是"中间旁白"：在本条 user 消息边界之前，它后面还跟着
 * 别的工具调用或 assistant 轮次 → 真正的结论还没说出口。
 * 后面只有 todo 进度卡片或什么都没有 → 它就是本轮结论，保留气泡。
 */
function isIntermediateAssistant(messages: ChatMessage[], index: number): boolean {
  for (let j = index + 1; j < messages.length; j++) {
    const m = messages[j];
    if (m.role === "user") return false;
    if (m.role === "assistant" && m.text.trim()) return true;
    // External engines historically appended all visible text to one id.
    // A late tool completion must never hide that combined final answer.
    if (m.role === "tool" && !isTodoCard(m) && !messages[index].engineId?.match(/^(claude_code|pi|opencode)$/)) return true;
  }
  return false;
}

/**
 * 把消息流分组：
 * - 连续的普通 tool 调用（非 todo 卡片）合并成一个 thinking 组；
 *   中间的 assistant 旁白一并收进组内（展开时在时间线里显示为旁白行），
 *   这样主流程中每条 assistant 气泡都是本轮最终结论，结论下面不会再出现思考过程。
 * - user / 最终 assistant / todo 卡片作为单条 msg 保留，且会切断工具组；
 *   组内若一条工具都没有（纯旁白），退化为 narration 行，不显示空思考卡片。
 * todo 卡片是进度标记，必须保留在对话流中不断开。
 */
export function groupMessages(messages: ChatMessage[]): GroupItem[] {
  const groups: GroupItem[] = [];
  let current: ChatMessage[] = [];
  const flush = () => {
    if (!current.length) return;
    const hasTool = current.some((m) => m.role === "tool");
    if (hasTool) {
      groups.push({ type: "thinking", items: current });
    } else {
      for (const m of current) groups.push({ type: "narration", msg: m });
    }
    current = [];
  };
  for (let i = 0; i < messages.length; i++) {
    const msg = messages[i];
    if (msg.role === "tool" && !isTodoCard(msg)) {
      current.push(msg);
    } else if (msg.role === "assistant" && isIntermediateAssistant(messages, i)) {
      // 无字的占位 assistant（流式期间先建后填）不进组：
      // 否则渲染成一条只有左边线、没有文字的紫条
      if ((msg.text || "").trim()) current.push(msg);
    } else {
      flush();
      groups.push({ type: "msg", msg });
    }
  }
  flush();
  return groups;
}

/** thinking 组里工具调用的条数（旁白不计入"N 个工具调用"） */
export function thinkingToolCount(items: ChatMessage[]): number {
  return items.filter((m) => m.role === "tool").length;
}

/** 稳定的组 key：msg/narration 用自身 id；thinking 组用第一条成员的 id */
export function groupKey(g: GroupItem): string {
  if (g.type === "msg") return g.msg.id;
  if (g.type === "narration") return `nar_${g.msg.id}`;
  return `grp_${g.items[0].id}`;
}
