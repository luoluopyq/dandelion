<template>
  <div>
    <!-- 缩进只由 button 上那个内联 paddingLeft 决定（子任务每层 +14px）。基准取 14
         而不是 10，是为了让顶级行的标题落在 34px 这条轨上：树内边距 6 + 内边距 14 +
         状态点列 7 + 间隙 7 = 34，正好是分组头里空间名的起点。它是内联样式，会盖掉
         样式表里的 padding-left —— 所以不要在 CSS 里再写一次缩进，两个机制会让子行
         缩两倍。 -->
    <button
      type="button"
      class="task-row"
      :class="{ on: active, kid: depth > 0, compact, picking: store.selectMode }"
      :style="{ paddingLeft: 14 + depth * 14 + 'px' }"
      @click="onClick"
      @contextmenu.prevent="$emit('menu', { ev: $event, node })"
    >
      <!-- 多选模式下复选框顶掉状态点的位置，避免行宽跳动。 -->
      <span
        v-if="store.selectMode"
        class="pick"
        :class="{ on: store.selectedIds.includes(node.id) }"
        aria-hidden="true"
      >{{ store.selectedIds.includes(node.id) ? "✓" : "" }}</span>
      <span v-else class="dot" :class="node.status" />
      <!-- 标题格内部：标题 + 一串跟在文字后面的尾标（所在目录 / 开在标签页里 /
           已分离到独立窗口）。它们必须在**同一格**里，不能让每个尾标各占一列 ——
           网格的 auto 列按内容算宽，行与行的列数一旦不同（有尾标的行 4 列、没有的
           3 列），收尾那一列（时间）就会在两种行之间左右跳 20 多像素，整列时间
           看着是歪的。做成内联后缀之后，每一行都是 3 列，时间对每一行都落在同
           一条右轨上。 -->
      <span class="title-line">
        <span class="title">{{ node.title || "未命名任务" }}</span>
        <span v-if="backgroundCount" class="background-count">后台 {{ backgroundCount }}</span>
        <span v-if="node.folder && !compact" class="folder">{{ node.folder }}</span>
        <span v-if="open" class="tabmark">Tab</span>
        <span v-else-if="detached" class="tabmark">09</span>
      </span>
      <span class="meta">{{ compact ? formatRelative(node.updatedAt) : `${statusLabel(node.status)} · ${formatRelative(node.updatedAt)}` }}</span>
    </button>
    <TaskRow
      v-for="child in node.children"
      :key="child.id"
      :node="child"
      :active="store.activeId === child.id"
      :open="store.openIds.includes(child.id)"
      :detached="store.detachedIds.includes(child.id)"
      :depth="depth + 1"
      :compact="compact"
      @open="store.openSession(child.id)"
      @menu="(p) => $emit('menu', p)"
    />
  </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { useSessionStore } from "../stores/session";
import type { TaskNode } from "../lib/types";
import { formatRelative, statusLabel } from "../lib/task-tree";

defineOptions({ name: "TaskRow" });

const props = defineProps<{
  node: TaskNode;
  active: boolean;
  open: boolean;
  detached?: boolean;
  depth: number;
  compact?: boolean;
}>();

const emit = defineEmits<{
  open: [];
  menu: [payload: { ev: MouseEvent; node: TaskNode }];
}>();

const store = useSessionStore();
const backgroundCount = computed(() => Object.values(store.terminals)
  .filter((term) => term.sessionId === props.node.id && term.background && !term.exited).length);

// 多选模式下点击是勾选，不是打开会话 —— 否则每勾一条都会切换当前对话。
function onClick() {
  if (store.selectMode) {
    store.toggleSelected(props.node.id);
    return;
  }
  emit("open");
}
</script>

<style scoped>
.background-count { flex: 0 0 auto; padding: 1px 5px; border-radius: 4px; background: color-mix(in srgb, var(--ok) 12%, var(--bg-2)); color: var(--ok); font-size: var(--fs-2xs); }
</style>
