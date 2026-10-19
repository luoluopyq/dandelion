<template>
  <section class="corpus-page">
    <header class="corpus-header"><div><h1>任务资料</h1><p>仅在当前任务配置中勾选后使用；不会进入资料库，也不会影响其他任务。</p></div><div><button type="button" class="button-quiet" :disabled="busy" @click="refresh">刷新</button><button type="button" class="button-primary" :disabled="busy" @click="upload">上传资料</button></div></header>
    <p v-if="store.panelError" class="corpus-error">{{ store.panelError }}</p>
    <section v-if="store.corpusDocs.length" class="corpus-list"><article v-for="doc in store.corpusDocs" :key="doc.id"><span class="corpus-icon">▤</span><div><strong>{{ doc.name }}</strong><small>{{ doc.status === 'ready' ? '已就绪，可用于任务检索' : doc.status }} · {{ doc.chunks }} 个片段 · {{ size(doc.bytes) }}</small></div><button type="button" class="button-danger" :disabled="busy" @click="remove(doc.id)">删除</button></article></section>
    <div v-else class="corpus-empty"><strong>还没有任务资料</strong><p>上传 txt 或 Markdown 文档后，可在任务配置中勾选使用。</p><button type="button" class="button-primary" :disabled="busy" @click="upload">上传第一个资料</button></div>
  </section>
</template>

<script setup lang="ts">
import { ref } from "vue";
import { useSessionStore } from "../stores/session";
const store = useSessionStore(); const busy = ref(false);
async function refresh() { busy.value = true; try { await store.refreshCatalog(); } finally { busy.value = false; } }
async function upload() { busy.value = true; try { await store.importCorpus(); } finally { busy.value = false; } }
async function remove(id: string) { if (!window.confirm("删除这份知识文档？不会影响原始文件。")) return; busy.value = true; try { await store.deleteCorpus(id); } finally { busy.value = false; } }
function size(bytes: number) { return bytes < 1024 ? `${bytes} B` : `${(bytes / 1024).toFixed(1)} KiB`; }
</script>
