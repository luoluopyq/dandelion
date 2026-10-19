<template>
  <div class="thinking-group" :class="{ open }">
    <button
      type="button"
      class="thinking-head"
      :title="open ? '点击收起' : '点击展开全部工具调用'"
      @click="toggle"
    >
      <span class="chev">{{ open ? "▾" : "▸" }}</span>
      <span class="label">思考过程</span>
      <span v-if="runningCount > 0" class="run-badge">进行中</span>
      <span class="count">{{ toolCount }} 个工具调用</span>
      <span class="size muted">{{ totalSize }}</span>
      <span class="spacer" />
      <span
        v-for="st in statuses"
        :key="st.raw"
        class="mini-status"
        :class="st.kind"
      >{{ st.label }}</span>
    </button>

    <div v-if="open" ref="bodyEl" class="thinking-body" @scroll.passive="onBodyScroll">
      <template v-for="item in visibleItems" :key="item.id">
        <!-- 中间旁白：模型在工具轮之间说的话，属于过程而不是结论 -->
        <p v-if="item.role === 'assistant'" class="t-narration">{{ item.text }}</p>
        <div v-else class="thinking-item">
          <header class="thinking-item-head">
            <span class="dot" :class="statusKind(item.status)" />
            <span class="name">{{ toolName(item) }}</span>
            <span v-if="item.status" class="t-status muted">{{ item.status }}</span>
            <span class="t-size muted">{{ size(item.text) }}</span>
          </header>
          <!-- 标题已含结果的工具（write_file 等）没有第二行，不再重复渲染一遍标题 -->
          <pre v-if="bodyText(item.text)" class="t-body">{{ bodyText(item.text) }}</pre>
        </div>
      </template>
      <p v-if="!toolCount" class="empty">暂无工具调用</p>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import type { ChatMessage } from "../lib/types";
import { thinkingToolCount } from "../lib/group-messages";

const props = defineProps<{ items: ChatMessage[] }>();

const open = ref(false);
// 组内可能混有中间旁白（assistant），计数与体积只算工具调用
const toolCount = computed(() => thinkingToolCount(props.items));
// 兜底：无字的旁白（历史里遗留的占位消息）不渲染，
// 否则只剩一条左边线紫条，既没信息又挡视线
const visibleItems = computed(() =>
  props.items.filter((m) => m.role !== "assistant" || (m.text || "").trim().length > 0),
);
// thinking-body 的 DOM 引用：用于展开时自动滚到新工具调用
const bodyEl = ref<HTMLElement | null>(null);

function toggle() {
  open.value = !open.value;
  if (open.value) {
    // 首次展开：等 DOM 渲染完成后滚到底
    stick.value = true;
    nextTick(() => {
      const el = bodyEl.value;
      if (el) el.scrollTop = el.scrollHeight;
    });
  }
}

// 是否贴底：用户手动上滑后为 false —— 流式追加时不再把视野拽回底部，
// 这样才滚得动、也点得到吸顶的收起头
const stick = ref(true);
function onBodyScroll() {
  const el = bodyEl.value;
  if (!el) return;
  stick.value = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
}

const runningCount = computed(() =>
  props.items.filter((m) => {
    const s = (m.status || "").toLowerCase();
    return s.includes("running") || s === "in_progress";
  }).length,
);

const totalSize = computed(() => {
  const bytes = props.items
    .filter((m) => m.role === "tool")
    .reduce((n, m) => n + (m.text || "").length, 0);
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}K`;
  return `${(bytes / (1024 * 1024)).toFixed(1)}M`;
});

// 汇总状态标签（去重排序）
const statuses = computed(() => {
  const seen = new Set<string>();
  const out: { label: string; kind: string; raw: string }[] = [];
  for (const m of props.items) {
    const s = (m.status || "").trim();
    if (!s || seen.has(s)) continue;
    seen.add(s);
    const kind = statusKind(s);
    const label = s.includes("running") || s === "in_progress" ? "进行中" : s;
    out.push({ label, kind, raw: s });
  }
  return out;
});

function toolName(msg: ChatMessage): string {
  const t = msg.text || "";
  const i = t.indexOf("\n");
  return (i >= 0 ? t.slice(0, i) : t).trim() || "tool";
}

/** 标题行以外的输出正文；没有第二行时返回空串（标题即全部信息） */
function bodyText(text?: string): string {
  const t = text || "";
  const i = t.indexOf("\n");
  return i >= 0 ? t.slice(i + 1).trim() : "";
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

// "页面中的页面"自动滚动：展开状态下，新工具调用 / 流式追加时
// 把最新的内容滚到视野内。等 DOM 渲染完成后做，避免在 markdown / 代码块
// 布局完成前量 scrollHeight 导致一次滚不到位。
watch(
  () => props.items.length,
  () => {
    if (!open.value || !bodyEl.value) return;
    nextTick(() => {
      requestAnimationFrame(() => {
        const el = bodyEl.value;
        if (el) el.scrollTop = el.scrollHeight;
      });
    });
  },
);

watch(
  () => props.items.map((m) => m.text.length).join("|"),
  () => {
    if (!open.value || !bodyEl.value) return;
    nextTick(() => {
      requestAnimationFrame(() => {
        const el = bodyEl.value;
        if (el) el.scrollTop = el.scrollHeight;
      });
    });
  },
);
</script>
