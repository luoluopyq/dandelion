<template>
  <aside class="sidebar">
    <header class="side-head">
      <span>设置</span>
    </header>
    <p class="hint">摘要。完整键在主区 15–17。改完对下一轮 prompt 生效。</p>
    <button type="button" class="asset" @click="store.openSettings('models')">
      <strong>硬件探测</strong>
      <small>{{ hw }}</small>
    </button>
    <button type="button" class="asset" @click="store.openSettings('models')">
      <strong>GGUF / sidecar</strong>
      <small>{{ sidecar }}</small>
    </button>
    <button type="button" class="asset" @click="store.openSettings('providers')">
      <strong>云 API</strong>
      <small>{{ cloud }}</small>
    </button>
    <button type="button" class="asset" @click="store.openSettings('providers')">
      <strong>外部本地</strong>
      <small>{{ backend }}</small>
    </button>
    <button type="button" class="asset" @click="store.openSettings('common')">
      <strong>并发</strong>
      <small>远程不占本地槽 · {{ conc }}</small>
    </button>
    <button type="button" class="asset" @click="store.openSettings('security')">
      <strong>Shell</strong>
      <small>{{ shell }}</small>
    </button>
    <p v-if="store.panelError && store.mainView === 'settings'" class="err">{{ store.panelError }}</p>
  </aside>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { useSessionStore } from "../stores/session";

const store = useSessionStore();
const vals = computed(() => store.settings?.values ?? {});
const cloud = computed(() => {
  const url = String(vals.value["dandelion.baseUrl"] ?? "");
  const model = String(vals.value["dandelion.defaultModel"] ?? "");
  if (!url || !model) return "未配置";
  return store.settings?.apiKeySet ? `${model} · Key 已保存` : model;
});
const backend = computed(() => String(vals.value["dandelion.defaultBackend"] ?? "openai_compat"));
const shell = computed(() => (vals.value["dandelion.allowShell"] === false ? "关" : "开"));
const hw = computed(() => {
  const h = store.hardware;
  if (!h) return "点此探测";
  if (h.gpuName) return `${h.gpuName} · ${h.vramMb ?? "?"} MiB`;
  return h.notes[0] ?? "无 NVIDIA";
});
const sidecar = computed(() => {
  const s = store.sidecar;
  if (!s) return "未刷新";
  if (s.bind) return s.bind;
  return s.kind;
});
const conc = computed(() => {
  const q = store.sidecar?.queue;
  if (!q) return `max ${vals.value["dandelion.maxLocalConcurrency"] ?? 2}`;
  return `${q.running}/${q.max} 排队 ${q.waiting}`;
});
</script>
