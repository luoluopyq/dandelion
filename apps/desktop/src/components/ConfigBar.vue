<template>
  <section v-if="store.active" class="config">
    <button type="button" class="strip" @click="store.configOpen = !store.configOpen">
      <span class="summary" :title="summary">{{ summary }}</span>
      <span class="chev">{{ store.configOpen ? "▾" : "▸" }} 配置</span>
    </button>
    <TaskConfig v-if="store.configOpen" />
  </section>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { useSessionStore } from "../stores/session";
import TaskConfig from "./TaskConfig.vue";

const store = useSessionStore();

const summary = computed(() => {
  const rec = store.active;
  if (!rec) return "";
  const g = store.settings?.values ?? {};
  const engine = rec.prefs.agentEngine || "native";
  const agentName = ({ native: "蒲公英原生", claude_code: "Claude Code", opencode: "OpenCode", pi: "Pi" } as Record<string, string>)[engine] || engine;
  const plugin = store.agentPlugins.find((item) => item.id === engine);
  const nativeModel = rec.prefs.model || String(g["dandelion.defaultModel"] ?? "模型未设");
  const model = engine === "native" ? nativeModel
    : plugin?.model || (engine === "claude_code" ? "模型由 Claude Code / CC Switch 决定" : `模型 ${nativeModel}（尝试复用）`);
  const temp = rec.prefs.temperature ?? Number(g["dandelion.temperature"] ?? 0.2);
  const tokens = rec.prefs.maxTokens ?? Number(g["dandelion.maxTokens"] ?? 0);
  // 这一段只报**这条对话自己的**目录，不拿 dandelion.workspace 这个全局设置兜底：
  // 兜底会让一条没选目录的新对话在这儿显示一个用户没选过的路径（和输入框那个
  // 目录 chip 自相矛盾）。没有目录就不显示这一段，连分隔符一起省掉 —— 拼出来的
  // 字符串以 " · " 收尾看着像丢了一个值。
  const parts = [
    `Agent ${agentName}`,
    model,
    `t=${temp}`,
    `${tokens} tok`,
    `Skill ${engine === "native" ? rec.skillIds.filter((id) => store.skills.some((item) => item.id === id && item.enabled)).length : 0}`,
    `MCP ${rec.mcpIds.length}`,
  ];
  const ws = rec.prefs.workspace || rec.workspace;
  if (ws) parts.push(ws);
  return parts.join(" · ");
});
</script>
