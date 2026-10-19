<template>
  <aside class="sidebar">
    <header class="side-head">
      <span>技能管理</span>
    </header>
    <p class="hint">技能是给 Agent 的操作指南。全局启用表示可选；在对话中选用后才加载正文。</p>
    <div class="row-btns">
      <button type="button" @click="store.importSkill(true)">导入文件夹</button>
      <button type="button" @click="store.importSkill(false)">导入 SKILL.md</button>
      <button type="button" @click="remoteOpen = !remoteOpen">远程安装</button>
    </div>
    <form v-if="remoteOpen" class="asset" @submit.prevent="onInstall">
      <label>
        URL（SKILL.md 直链，或 git 仓库）
        <input v-model="remote.url" placeholder="https://github.com/owner/repo" required />
      </label>
      <label>
        subdir（仓库内子目录，可选）
        <input v-model="remote.subdir" placeholder="skills" />
      </label>
      <p class="hint">远程安装后默认停用。先检查内容，再启用并在需要的对话中选用。</p>
      <button type="submit">安装</button>
    </form>
    <p v-if="store.panelError && store.dockPanel === 'skills'" class="err">{{ store.panelError }}</p>
    <ul v-if="store.installNotes.length" class="hint">
      <li v-for="note in store.installNotes" :key="note">{{ note }}</li>
    </ul>
    <div class="tree">
      <article v-for="sk in items" :key="sk.id" class="asset">
        <strong>{{ sk.id }}</strong>
        <small>{{ sk.description }}</small>
        <label class="chk">
          <input
            type="checkbox"
            :checked="sk.enabled"
            :disabled="sk.framework"
            @change="store.setSkillEnabled(sk.id, ($event.target as HTMLInputElement).checked)"
          />
          可用（全局）
        </label>
        <div class="row-btns">
          <button type="button" @click="store.openSkill(sk.id)">打开文件</button>
          <button type="button" @click="store.exportSkill(sk.id)">导出</button>
          <button type="button" class="danger" @click="onRemove(sk.id)">删除</button>
        </div>
      </article>
      <p v-if="!items.length" class="muted">还没有技能。导入 SKILL.md 或技能目录。</p>
    </div>
  </aside>
</template>

<script setup lang="ts">
import { computed, reactive, ref } from "vue";
import { useSessionStore } from "../stores/session";

const store = useSessionStore();
const items = computed(() => store.skills.filter((s) => !s.framework));
const remoteOpen = ref(false);
const remote = reactive({ url: "", subdir: "" });

async function onInstall() {
  const url = remote.url.trim();
  if (!url) return;
  // A bare .md is fetched directly; anything else is cloned.
  if (url.toLowerCase().endsWith(".md") && !remote.subdir.trim()) {
    await store.installSkillFromUrl(url);
  } else {
    await store.installSkillFromGit(url, remote.subdir.trim() || null);
  }
  remote.url = "";
  remote.subdir = "";
}

async function onRemove(id: string) {
  if (!window.confirm(`删除技能 ${id}？会删掉 data_dir/skills 下的目录，不影响已有任务绑定。`)) return;
  await store.removeSkill(id);
}
</script>
