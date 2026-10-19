<template>
  <div class="setup">
    <h2>连接模型服务</h2>
    <p class="lead">
      填写模型服务的地址和模型名称。服务可以运行在本机、其他电脑或云端；也可以进入“模型”设置，导入由蒲公英启动的 GGUF 或 MLX 模型。
    </p>
    <n-form label-placement="top">
      <n-form-item label="服务地址">
        <n-input v-model:value="baseUrl" placeholder="https://api.example.com/v1" />
      </n-form-item>
      <n-form-item label="模型名称">
        <n-input v-model:value="model" placeholder="服务提供的模型 ID" />
      </n-form-item>
      <n-form-item label="API 密钥（选填）">
        <n-input
          v-model:value="apiKey"
          type="password"
          show-password-on="click"
          :placeholder="session.settings?.apiKeySet ? '已保存，留空则不改' : '可选'"
        />
      </n-form-item>
      <n-button type="primary" :loading="saving" :disabled="!baseUrl.trim() || !model.trim()" @click="onSave">
        保存并继续
      </n-button>
      <n-button :loading="probing" :disabled="!baseUrl.trim()" @click="onProbe">测试连接</n-button>
      <p v-if="probeMsg" :class="probeOk ? 'ok' : 'err'">{{ probeMsg }}</p>
      <p v-if="fail" class="err">{{ fail }}</p>
    </n-form>
  </div>
</template>

<script setup lang="ts">
import { ref } from "vue";
import { NButton, NForm, NFormItem, NInput } from "naive-ui";
import { useSessionStore } from "../stores/session";

const session = useSessionStore();
const baseUrl = ref(String(session.settings?.values["dandelion.baseUrl"] ?? ""));
const model = ref(String(session.settings?.values["dandelion.defaultModel"] ?? ""));
const apiKey = ref("");
const saving = ref(false);
const probing = ref(false);
const fail = ref("");
const probeMsg = ref("");
const probeOk = ref(false);

async function onProbe() {
  fail.value = "";
  probeMsg.value = "";
  probing.value = true;
  try {
    const r = await session.probeProvider(baseUrl.value, apiKey.value);
    probeOk.value = r.ok;
    probeMsg.value = r.ok ? `HTTP ${r.status} ${r.message}` : `失败 HTTP ${r.status} ${r.message}`;
  } catch (err) {
    probeOk.value = false;
    probeMsg.value = err instanceof Error ? err.message : String(err);
  } finally {
    probing.value = false;
  }
}

async function onSave() {
  fail.value = "";
  saving.value = true;
  try {
    await session.saveSetup(baseUrl.value, model.value, apiKey.value);
  } catch (err) {
    fail.value = err instanceof Error ? err.message : String(err);
  } finally {
    saving.value = false;
  }
}
</script>
