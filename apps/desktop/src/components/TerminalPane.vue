<template>
  <section v-if="term" class="live-term" aria-label="活终端">
    <header class="live-term-bar">
      <span class="live-term-title" :title="term.command">{{ title }}</span>
      <span class="muted">{{ term.exited ? exitLabel : "运行中" }}</span>
      <span class="spacer" />
      <button type="button" :disabled="term.exited" @click="store.killLiveTerminal(term.terminalId)">
        杀进程树
      </button>
      <button type="button" title="关闭窗不会杀进程" @click="store.closeLivePane()">关闭窗</button>
    </header>
    <pre class="live-term-out">{{ term.text || "(no output)" }}</pre>
  </section>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { useSessionStore } from "../stores/session";

const store = useSessionStore();

const term = computed(() =>
  store.liveOpenId ? store.terminals[store.liveOpenId] ?? null : null,
);

const title = computed(() => {
  if (!term.value) return "";
  return term.value.command || term.value.terminalId;
});

const exitLabel = computed(() => {
  if (!term.value) return "";
  if (term.value.exitCode == null) return "已退出";
  return `已退出 ${term.value.exitCode}`;
});
</script>
