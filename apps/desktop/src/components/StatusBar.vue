<template>
  <footer class="statusbar">
    <span v-if="session.info?.loopReady" class="ok">已配置</span>
    <span v-else class="warn">未配置</span>
    <span v-if="session.sidecar?.bind" class="ok">sidecar {{ session.sidecar.bind }}</span>
    <span v-else-if="backend === 'sidecar'" class="hint">sidecar 已停</span>
    <span class="hint">远程不占本地槽</span>
    <span v-if="session.sidecar">
      本地 {{ session.sidecar.queue.running }}/{{ session.sidecar.queue.max }}
      · 排队 {{ session.sidecar.queue.waiting }}
    </span>
    <span class="grow" />
    <span class="mono" :title="session.info?.dataDir">
      {{ session.info?.version }} · {{ session.info?.host }}
    </span>
  </footer>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted } from "vue";
import { useSessionStore } from "../stores/session";

const session = useSessionStore();
const backend = computed(
  () => String(session.settings?.values["dandelion.defaultBackend"] ?? "openai_compat"),
);
let timer: ReturnType<typeof setInterval> | undefined;
onMounted(() => {
  timer = setInterval(() => {
    void session.refreshSidecar();
  }, 4000);
});
onUnmounted(() => {
  if (timer) clearInterval(timer);
});
</script>
