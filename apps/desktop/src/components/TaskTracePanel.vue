<template>
  <aside class="task-trace" :class="{ collapsed: !visible }" aria-label="任务执行追踪">
    <header class="trace-head">
      <span class="trace-title">任务执行</span>
      <span class="muted trace-count">{{ traceToolCount }} 个工具</span>
      <span class="spacer" />
      <button
        type="button"
        class="trace-toggle"
        :title="visible ? '收起右侧面板' : '展开右侧面板'"
        @click="$emit('update:visible', !visible)"
      >
        {{ visible ? "▸" : "◂" }}
      </button>
    </header>
    <div v-show="visible" ref="scroller" class="trace-list">
      <p v-if="!toolMessages.length" class="trace-empty">
        还没有工具调用。任务开始后这里会显示每一步的输入和输出。
      </p>
      <article
        v-for="msg in toolMessages"
        :key="msg.id"
        class="trace-item"
        :class="{ open: expanded[msg.id], [statusKind(msg.status)]: true }"
      >
        <button type="button" class="trace-row" @click="toggle(msg.id)">
          <span class="dot" :class="statusKind(msg.status)" />
          <span class="trace-name">{{ toolName(msg) }}</span>
          <span v-if="msg.status" class="trace-status muted">{{ msg.status }}</span>
          <span class="trace-size muted">{{ size(msg.text) }}</span>
          <span class="trace-chev">{{ expanded[msg.id] ? "▾" : "▸" }}</span>
        </button>
        <pre v-if="expanded[msg.id]" class="trace-body">{{ msg.text }}</pre>
      </article>
    </div>
  </aside>
</template>

<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import type { ChatMessage } from "../lib/types";

interface TodoRow {
  id: string;
  content: string;
  status: string;
}

const props = defineProps<{
  visible: boolean;
  messages: ChatMessage[];
}>();

defineEmits<{
  (e: "update:visible", value: boolean): void;
}>();

const scroller = ref<HTMLElement | null>(null);
const expanded = ref<Record<string, boolean>>({});
let stick = true;

// 只显示"思考过程"——工具调用。todo-list 卡片不算（已经在主聊天区展示了）
const toolMessages = computed(() => {
  return props.messages.filter((m) => {
    if (m.role !== "tool") return false;
    const text = m.text || "";
    if (text.indexOf("{") < 0) return true;
    try {
      const start = text.indexOf("{");
      const parsed = JSON.parse(text.slice(start)) as { todos?: TodoRow[] };
      if (Array.isArray(parsed.todos)) return false;
    } catch {
      /* not todo */
    }
    return true;
  });
});

const traceToolCount = computed(() => toolMessages.value.length);

function toggle(id: string) {
  expanded.value = { ...expanded.value, [id]: !expanded.value[id] };
}

function toolName(msg: ChatMessage): string {
  const t = msg.text || "";
  const i = t.indexOf("\n");
  return (i >= 0 ? t.slice(0, i) : t).trim() || "tool";
}

function statusKind(s?: string): string {
  const v = (s || "").toLowerCase();
  if (v.includes("done") || v === "completed" || v === "success" || v === "ok") return "ok";
  if (v.includes("error") || v.includes("fail")) return "err";
  if (v.includes("running") || v === "in_progress") return "run";
  return "idle";
}

function size(text?: string): string {
  const bytes = (text || "").length;
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}K`;
  return `${(bytes / (1024 * 1024)).toFixed(1)}M`;
}

function onScroll() {
  const el = scroller.value;
  if (!el) return;
  stick = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
}

// 新增 / 更新 / 状态变化时自动滚到底（仅当用户已经贴底）
watch(
  () =>
    toolMessages.value
      .map((m) => `${m.id}:${m.text.length}:${m.status ?? ""}`)
      .join("\0"),
  async () => {
    if (!stick) return;
    await nextTick();
    const el = scroller.value;
    if (el) el.scrollTop = el.scrollHeight;
  },
);
</script>
