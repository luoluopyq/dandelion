<template>
  <aside class="sidebar">
    <header class="side-head">
      <span>可访问目录</span>
    </header>
    <p class="hint">蒲公英可访问工作区及下列目录。写入仍按授权规则处理；未列出的目录会在访问前询问。系统目录与整个用户主目录不可添加。</p>
    <div class="row-btns">
      <button type="button" @click="store.addSandboxRoot()">添加目录</button>
    </div>
    <form class="asset" @submit.prevent="onAddPath">
      <label>
        手动输入目录路径
        <input v-model="manualPath" placeholder="/Users/you/Desktop" />
      </label>
      <button type="submit">添加路径</button>
    </form>
    <p v-if="store.panelError && store.dockPanel === 'sandbox'" class="err">{{ store.panelError }}</p>
    <div class="tree">
      <article v-if="store.sandbox" class="asset">
        <strong>默认工作区</strong>
        <small>{{ store.sandbox.workspace }}</small>
        <button type="button" :disabled="!store.active" @click="store.applySandboxToTask(store.sandbox.workspace)">
          当前任务
        </button>
      </article>
      <article v-for="root in store.sandbox?.extra ?? []" :key="root" class="asset">
        <strong>额外根</strong>
        <small>{{ root }}</small>
        <div class="row-btns">
          <button type="button" :disabled="!store.active" @click="store.applySandboxToTask(root)">当前任务</button>
          <button type="button" class="danger" @click="store.removeSandboxRoot(root)">移除</button>
        </div>
      </article>
    </div>
  </aside>
</template>

<script setup lang="ts">
import { ref } from "vue";
import { useSessionStore } from "../stores/session";

const store = useSessionStore();
const manualPath = ref("");

async function onAddPath() {
  await store.addSandboxPath(manualPath.value);
  if (!store.panelError) manualPath.value = "";
}
</script>
