<template>
  <div class="g5e-panel">
    <p class="hint">
      G6：`dln serve` / `dln acp` 是可选进程外宿主。桌面仍用进程内 Runtime，不走 HTTP 后端。
      出厂只绑 127.0.0.1；非回环必须 CLI 显式 `--bind`。
    </p>
    <label class="block">
      serve.bind（settings）
      <input :value="bind" @change="onBind" />
      <small class="muted">dandelion.serve.bind · 非回环地址保存后仍须 CLI `--bind` 才能监听</small>
    </label>
    <p class="muted">CLI：`dln serve` · `dln acp`（stdio JSON-RPC）</p>
    <p v-if="!isLoopback" class="err">当前 bind 非回环：桌面不会启动监听；请用 `dln serve --bind …`。</p>
  </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { useSessionStore } from "../stores/session";

const store = useSessionStore();
const bind = computed(() => String(store.settings?.values?.["dandelion.serve.bind"] ?? "127.0.0.1:3927"));

const isLoopback = computed(() => {
  const host = bind.value.split(":").slice(0, -1).join(":") || bind.value;
  return ["127.0.0.1", "localhost", "::1"].includes(host.replace(/[\[\]]/g, ""));
});

function onBind(ev: Event) {
  const v = (ev.target as HTMLInputElement).value.trim();
  void store.patchSettings({ "dandelion.serve.bind": v });
}
</script>
