<template>
  <form class="settings-provider-form" @submit.prevent="onSave(true)">
    <div class="settings-provider-fields">
      <div class="settings-provider-intro"><strong>连接模型服务</strong><p>通过服务地址连接模型。API、Ollama 和 vLLM 服务可以运行在本机，也可以运行在其他电脑或云端。</p></div>
      <label>
        <span>服务类型</span>
        <select v-model="backend">
          <option value="openai_compat">兼容 OpenAI 的 API</option>
          <option value="openai_responses">OpenAI Responses API</option>
          <option value="ollama">Ollama 服务</option>
          <option value="vllm">vLLM 服务</option>
        </select>
      </label>
      <label><span>服务地址</span><input v-model.trim="baseUrl" :placeholder="urlPlaceholder" /><small>填写接口根地址，无需附加 /chat/completions。</small></label>
      <label><span>模型名称</span><input v-model.trim="model" placeholder="服务提供的模型 ID" /></label>
      <div class="settings-provider-key">
        <label for="provider-api-key">API 密钥 <span class="settings-provider-key-optional">选填</span></label>
        <div class="settings-provider-key-control">
          <input v-if="revealedSavedKey !== null" id="provider-api-key" :value="revealedSavedKey" type="text" readonly autocomplete="off" spellcheck="false" aria-label="已保存的 API 密钥" />
          <input v-else id="provider-api-key" v-model="apiKey" :type="showApiKey ? 'text' : 'password'" autocomplete="off" spellcheck="false" :placeholder="store.settings?.apiKeySet ? '已保存；留空保持不变' : '不需要密钥可留空'" />
          <button type="button" class="button-quiet" :disabled="revealingKey" :aria-label="showApiKey ? '隐藏 API 密钥' : '显示 API 密钥'" @click="toggleApiKey">{{ revealingKey ? '读取中…' : showApiKey ? '隐藏' : '显示' }}</button>
        </div>
        <small>{{ revealedSavedKey !== null ? '当前显示已保存的密钥；隐藏后可输入新密钥。' : store.settings?.apiKeySet ? '密钥已保存于此设备。' : '尚未保存密钥。' }}调用模型服务时会发送给上方填写的服务地址；更换地址并留空保存会清除旧密钥。</small>
      </div>
    </div>
    <div class="settings-provider-actions">
      <button type="button" class="button-quiet" :disabled="probing || !baseUrl.trim()" @click="onProbe">测试连接</button>
      <button v-if="localIsDefault" type="button" class="button-quiet" :disabled="!canSave" @click="onSave(false)">仅保存服务配置</button>
      <button type="submit" class="button-primary" :disabled="!canSave">保存并设为新任务默认</button>
    </div>
    <p v-if="probeMsg" :class="probeOk ? 'ok' : 'err'">{{ probeMsg }}</p>
    <p v-if="probeModels.length" class="muted">可用模型：{{ probeModels.slice(0, 8).join(", ") }}</p>
    <button type="button" class="settings-provider-advanced-link" @click="store.openSettings('network')">连接问题排查与重试设置 ↗</button>
  </form>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { settingsApiKeyReveal } from "../lib/ipc";
import { useSessionStore } from "../stores/session";

const store = useSessionStore();
const baseUrl = ref("");
const model = ref("");
const apiKey = ref("");
const showApiKey = ref(false);
const revealedSavedKey = ref<string | null>(null);
const revealingKey = ref(false);
const backend = ref("openai_compat");
const probing = ref(false);
const probeMsg = ref("");
const probeOk = ref(false);
const probeModels = ref<string[]>([]);

function syncFromStore() {
  hideApiKey();
  const v = store.settings?.values ?? {};
  const savedBackend = String(v["dandelion.defaultBackend"] ?? "openai_compat");
  const savedServiceBackend = String(v["dandelion.service.backend"] ?? (savedBackend === "sidecar" ? "openai_compat" : savedBackend));
  const savedModel = String(v["dandelion.service.model"] ?? (savedBackend === "sidecar" ? "" : v["dandelion.defaultModel"] ?? ""));
  const savedUrl = String(v["dandelion.baseUrl"] ?? "");
  backend.value = savedServiceBackend;
  baseUrl.value = savedUrl;
  model.value = savedModel;
}

function hideApiKey() {
  revealedSavedKey.value = null;
  showApiKey.value = false;
}

async function toggleApiKey() {
  if (showApiKey.value) {
    hideApiKey();
    return;
  }
  if (apiKey.value || !store.settings?.apiKeySet) {
    showApiKey.value = true;
    return;
  }
  revealingKey.value = true;
  try {
    const savedKey = await settingsApiKeyReveal();
    if (savedKey) {
      revealedSavedKey.value = savedKey;
      showApiKey.value = true;
    } else {
      probeOk.value = false;
      probeMsg.value = '未找到已保存的密钥，请重新输入并保存。';
    }
  } catch (err) {
    probeOk.value = false;
    probeMsg.value = err instanceof Error ? err.message : String(err);
  } finally {
    revealingKey.value = false;
  }
}

watch(
  () => store.settings,
  () => syncFromStore(),
  { immediate: true },
);

const canSave = computed(() => {
  return !!model.value.trim() && !!baseUrl.value.trim();
});
const localIsDefault = computed(() => store.settings?.values["dandelion.defaultBackend"] === "sidecar");
const addressChanged = computed(() => {
  const saved = store.settings?.values ?? {};
  return String(saved["dandelion.baseUrl"] ?? "").trim() !== baseUrl.value.trim();
});
const urlPlaceholder = computed(() => backend.value === "ollama" ? "http://127.0.0.1:11434/v1" : backend.value === "vllm" ? "http://127.0.0.1:8000/v1" : "https://api.example.com/v1");
async function onProbe() {
  if (addressChanged.value && store.settings?.apiKeySet && !apiKey.value.trim()) {
    probeOk.value = false;
    probeMsg.value = "请先保存新服务地址，再测试连接；保存时会清除旧服务的密钥。";
    return;
  }
  probing.value = true;
  probeMsg.value = "";
  probeModels.value = [];
  try {
    const r = await store.probeProvider(baseUrl.value, apiKey.value);
    probeOk.value = r.ok;
    probeMsg.value = r.ok ? `HTTP ${r.status} ${r.message}` : `失败 HTTP ${r.status} ${r.message}`;
    probeModels.value = r.models;
  } catch (err) {
    probeOk.value = false;
    probeMsg.value = err instanceof Error ? err.message : String(err);
  } finally {
    probing.value = false;
  }
}

async function onSave(activate: boolean) {
  if (!activate && !localIsDefault.value) return;
  probeMsg.value = "";
  const patch: Record<string, unknown> = {
    "dandelion.baseUrl": baseUrl.value.trim(),
    "dandelion.service.model": model.value.trim(),
    "dandelion.service.backend": backend.value,
  };
  if (activate) {
    patch["dandelion.defaultModel"] = model.value.trim();
    patch["dandelion.defaultBackend"] = backend.value;
  }
  if (apiKey.value.trim()) patch["dandelion.apiKey"] = apiKey.value.trim();
  else if (addressChanged.value) patch["dandelion.apiKey"] = null;
  await store.patchSettings(patch);
  if (!store.panelError) {
    hideApiKey();
    apiKey.value = "";
    probeOk.value = true;
    probeMsg.value = activate ? "已保存，并设为新任务默认模型。" : "服务配置已保存；当前默认模型未更改。";
  }
}
</script>
