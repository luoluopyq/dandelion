<template>
  <aside class="sidebar plugin-panel">
    <header class="side-head">
      <span>插件</span>
      <button type="button" class="new" title="插件设置" @click="store.openSettings('plugins')">⚙ 设置</button>
    </header>
    <p class="hint">统一查看已安装的技能与工具；任务中按需选择后才会生效。</p>
    <div class="plugin-summary">
      <button type="button" @click="store.openDock('skills')"><span>✦</span><strong>技能</strong><small>{{ skills.length }} 个已安装</small><i>›</i></button>
      <button type="button" @click="store.openDock('mcp')"><span>⌬</span><strong>工具</strong><small>{{ store.mcpServers.length }} 个已配置</small><i>›</i></button>
    </div>
    <p class="muted">点击右上角“设置”可在同一页面集中管理插件、技能与工具。</p>
  </aside>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { useSessionStore } from "../stores/session";

const store = useSessionStore();
const skills = computed(() => store.skills.filter((item) => !item.framework));
</script>
