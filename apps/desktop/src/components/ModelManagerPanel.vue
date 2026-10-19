<template>
  <div class="model-manager">
    <section class="model-current" aria-label="默认模型">
      <div><span>已生效的新任务默认模型</span><strong>{{ defaultTitle }}</strong><p>{{ defaultDetail }}</p></div>
      <div v-if="localConflict" class="model-current-actions">
        <button type="button" class="button-primary" @click="store.openSettings('providers')">改用模型服务</button>
        <button v-if="activeLocal" type="button" class="button-quiet" @click="syncLocalDefault">使用已选本机模型</button>
      </div>
    </section>
    <nav class="model-source-tabs" aria-label="查看模型配置">
      <button type="button" :class="{ active: tab === 'local' }" :aria-current="tab === 'local' ? 'page' : undefined" @click="store.openSettings('models')"><strong>本机模型配置</strong><span>导入 GGUF 或 MLX，由蒲公英启动</span></button>
      <button type="button" :class="{ active: tab === 'service' }" :aria-current="tab === 'service' ? 'page' : undefined" @click="store.openSettings('providers')"><strong>模型服务配置</strong><span>连接 API、Ollama 或 vLLM 服务</span></button>
    </nav>
    <p v-if="tab !== defaultSource && !localConflict" class="model-source-note">正在查看{{ tab === 'service' ? '模型服务' : '本机模型' }}配置。新任务仍默认使用「{{ defaultTitle }}」；只有点击下方的设为默认操作后才会切换。</p>
    <LocalModelsPanel v-if="tab === 'local'" />
    <ProvidersPanel v-else />
  </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { useSessionStore } from "../stores/session";
import LocalModelsPanel from "./LocalModelsPanel.vue";
import ProvidersPanel from "./ProvidersPanel.vue";

const store = useSessionStore();
const tab = computed(() => store.settingsSection === "providers" ? "service" : "local");
const values = computed(() => store.settings?.values ?? {});
const backend = computed(() => String(values.value["dandelion.defaultBackend"] ?? "openai_compat"));
const modelId = computed(() => String(values.value["dandelion.defaultModel"] ?? "").trim());
const activeLocal = computed(() => store.localModels.find(model => model.active));
const defaultSource = computed(() => backend.value === "sidecar" ? "local" : "service");
const localConflict = computed(() => backend.value === "sidecar" && !!modelId.value && modelId.value !== activeLocal.value?.id);
const defaultTitle = computed(() => {
  if (localConflict.value) return "模型设置不一致";
  return backend.value === "sidecar" ? (activeLocal.value?.name || "尚未选择本机模型") : (modelId.value || "尚未配置模型");
});
const defaultDetail = computed(() => {
  if (backend.value === "sidecar") {
    if (localConflict.value) return `设置为本机运行的模型是“${modelId.value}”，但本机已选${activeLocal.value ? `“${activeLocal.value.name}”` : "模型为空"}。请选择实际要使用的连接方式。`;
    return activeLocal.value ? `本机运行 · ${store.sidecar?.kind === "ready" ? "推理服务运行中" : "推理服务未启动"}` : "先在下方导入并选用模型。";
  }
  const kind = ({ openai_compat: "兼容 OpenAI 的 API", openai_responses: "OpenAI Responses API", ollama: "Ollama", vllm: "vLLM" } as Record<string, string>)[backend.value] || backend.value;
  const url = String(values.value["dandelion.baseUrl"] ?? "").trim();
  return `${kind}${url ? ` · ${url}` : " · 尚未填写服务地址"}`;
});
async function syncLocalDefault() {
  if (!activeLocal.value) return;
  await store.patchSettings({ "dandelion.defaultBackend": "sidecar", "dandelion.defaultModel": activeLocal.value.id });
}
</script>

<style scoped>
.model-manager { display: grid; gap: 18px; width: min(100%, 940px); }
.model-current { display: flex; align-items: center; justify-content: space-between; gap: 20px; padding: 20px 24px; border: 1px solid var(--border); border-radius: var(--radius-xl); background: color-mix(in srgb, var(--accent) 6%, var(--panel)); }
.model-current span { color: var(--muted); font-size: var(--fs-xs); }.model-current strong { display: block; margin: 6px 0; font-size: var(--fs-md); }.model-current p { margin: 0; color: var(--muted); font-size: var(--fs-xs); line-height: 1.5; overflow-wrap: anywhere; }
.model-current button { flex: 0 0 auto; }
.model-current-actions { display: flex; flex: 0 0 auto; gap: 8px; flex-wrap: wrap; }
.model-source-tabs { display: grid; grid-template-columns: 1fr 1fr; gap: 9px; }
.model-source-tabs button { display: grid; gap: 5px; padding: 13px 16px; text-align: left; border: 1px solid var(--border); border-radius: var(--radius-lg); background: var(--panel); color: var(--text); cursor: pointer; }
.model-source-tabs button.active { border-color: var(--accent); background: color-mix(in srgb, var(--accent) 11%, var(--panel)); }
.model-source-tabs strong { font-size: var(--fs-sm); }.model-source-tabs span { color: var(--muted); font-size: var(--fs-xs); }
.model-source-note { margin: -5px 0 0; color: var(--muted); font-size: var(--fs-xs); line-height: 1.5; }
@media (max-width: 700px) { .model-current { align-items: flex-start; flex-direction: column; }.model-source-tabs { grid-template-columns: 1fr; } }
</style>
