<template>
  <aside class="sidebar">
    <header class="side-head">
      <span>知识库管理</span>
    </header>
    <p class="hint">
      用户文档 corpus。上传后还须在 02 勾选才会检索。检索片段走短 user 段，不进 system。不是 wiki，不是 GGUF。
    </p>
    <div class="row-btns">
      <button type="button" :disabled="busy" @click="onUpload">上传文档</button>
    </div>
    <p class="muted">项目理解（wiki）在设置 19；此处只跳转，不在 04 编译。</p>
    <button type="button" @click="store.openSettings('wiki')">项目理解 → 19</button>
    <p v-if="store.panelError && store.dockPanel === 'corpus'" class="err">{{ store.panelError }}</p>
    <div class="tree">
      <article v-for="d in store.corpusDocs" :key="d.id" class="asset">
        <strong>{{ d.name }}</strong>
        <small>
          {{ d.status }} · {{ d.chunks }} 块 · {{ size(d.bytes) }}
        </small>
        <small class="muted">{{ d.id }}</small>
        <div class="row-btns">
          <button type="button" class="danger" :disabled="busy" @click="store.deleteCorpus(d.id)">
            删除
          </button>
        </div>
      </article>
      <p v-if="!store.corpusDocs.length" class="muted">还没有文档。上传 txt/md 后才能在 02 勾选。</p>
    </div>
  </aside>
</template>

<script setup lang="ts">
import { ref } from "vue";
import { useSessionStore } from "../stores/session";

const store = useSessionStore();
const busy = ref(false);

async function onUpload() {
  busy.value = true;
  try {
    await store.importCorpus();
  } finally {
    busy.value = false;
  }
}

function size(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  return `${(bytes / 1024).toFixed(1)} KiB`;
}
</script>
