<template>
  <div class="g5e-panel">
    <p class="hint">这里管理旧版<strong>全局记忆文件</strong>，作用于所有项目，并在<strong>新对话开始时</strong>读取。日常使用建议从左侧「记忆」进入项目记忆页。</p>
    <div class="row-btns">
      <button type="button" @click="store.openMemoryDir()">打开文件目录</button>
      <button type="button" @click="store.refreshMemory()">重新读取文件</button>
    </div>
    <div class="block">
      <label for="global-memory-soul"><strong>助理角色</strong></label><small>只在需要自定义助理身份时填写；留空即可使用默认身份。</small>
      <textarea id="global-memory-soul" v-model="soul" rows="4" />
      <button type="button" @click="save('soul', soul)">保存助理角色</button>
    </div>
    <div class="block">
      <label for="global-memory-user"><strong>个人偏好</strong></label><small>例如常用语言或回答格式，会对所有项目的新对话生效。</small>
      <textarea id="global-memory-user" v-model="user" rows="4" />
      <button type="button" @click="save('user', user)">保存个人偏好</button>
    </div>
    <div class="block">
      <label for="global-memory-notes"><strong>通用笔记</strong></label><small>只放不属于某个项目的长期信息；项目约定请用项目记忆。</small>
      <textarea id="global-memory-notes" v-model="notes" rows="4" />
      <button type="button" @click="save('memory', notes)">保存通用笔记</button>
    </div>
    <details><summary>查看文件位置</summary><small>{{ store.memoryFiles?.soulPath }}<br />{{ store.memoryFiles?.userPath }}<br />{{ store.memoryFiles?.memoryPath }}</small></details>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
import { useSessionStore } from "../stores/session";

const store = useSessionStore();
const soul = ref("");
const user = ref("");
const notes = ref("");

onMounted(() => { void store.refreshMemory(); });
watch(() => store.memoryFiles, (files) => {
  if (!files) return;
  soul.value = files.soul;
  user.value = files.user;
  notes.value = files.memory;
}, { immediate: true });
function save(kind: "soul" | "user" | "memory", body: string) { void store.saveMemory(kind, body); }
</script>
