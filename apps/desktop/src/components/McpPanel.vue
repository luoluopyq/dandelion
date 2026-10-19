<template>
  <aside class="sidebar">
    <header class="side-head">
      <span>工具管理</span>
    </header>
    <p class="hint">外部工具进同一 registry。启动不等于当前对话在用；还须 02 勾选。停 MCP 不会停对话。</p>
    <form class="asset" @submit.prevent="onAdd">
      <label>
        id
        <input v-model="form.id" required />
      </label>
      <label>
        传输
        <select v-model="form.kind">
          <option value="stdio">stdio</option>
          <option value="http">HTTP</option>
        </select>
      </label>
      <label v-if="form.kind === 'stdio'">
        command
        <input v-model="form.command" placeholder="npx" />
      </label>
      <label v-if="form.kind === 'stdio'">
        args（空格分隔）
        <input v-model="form.args" placeholder="-y some-mcp-server" />
      </label>
      <label v-if="form.kind === 'http'">
        url
        <input v-model="form.url" placeholder="http://127.0.0.1:8080/mcp" />
      </label>
      <label v-if="form.kind === 'http'">
        bearerTokenEnvVar
        <input v-model="form.bearer" />
      </label>
      <label v-if="form.kind === 'stdio'">
        cwd（可选）
        <input v-model="form.cwd" />
      </label>
      <label v-if="form.kind === 'stdio'">
        env（每行 KEY=VALUE）
        <textarea v-model="form.env" rows="2" />
      </label>
      <button type="submit">添加 MCP</button>
    </form>
    <p v-if="store.panelError && store.dockPanel === 'mcp'" class="err">{{ store.panelError }}</p>
    <div class="tree">
      <article v-for="m in store.mcpServers" :key="m.id" class="asset">
        <strong>{{ m.id }}</strong>
        <small>{{ status(m) }}</small>
        <small v-if="m.error" class="err">{{ m.error }}</small>
        <div class="row-btns">
          <button type="button" :disabled="m.ready" @click="store.startMcp(m.id)">启动</button>
          <button type="button" @click="store.stopMcp(m.id)">停止</button>
          <button type="button" class="danger" @click="onDelete(m.id)">删除</button>
        </div>
      </article>
      <p v-if="!store.mcpServers.length" class="muted">还没有工具。</p>
    </div>
  </aside>
</template>

<script setup lang="ts">
import { reactive } from "vue";
import { useSessionStore } from "../stores/session";
import type { McpListItem, McpServerConfig } from "../lib/types";

const store = useSessionStore();
const form = reactive({
  id: "",
  kind: "stdio" as "stdio" | "http",
  command: "",
  args: "",
  cwd: "",
  env: "",
  url: "",
  bearer: "",
});

function status(m: McpListItem) {
  if (!m.enabled) return "disabled";
  if (m.ready) return "运行中";
  if (m.error) return "失败";
  return "已停止";
}

function parseEnv(raw: string): Record<string, string> | undefined {
  const env: Record<string, string> = {};
  for (const line of raw.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const eq = trimmed.indexOf("=");
    if (eq <= 0) continue;
    env[trimmed.slice(0, eq)] = trimmed.slice(eq + 1);
  }
  return Object.keys(env).length ? env : undefined;
}

async function onAdd() {
  const id = form.id.trim();
  if (!id) return;
  const server: McpServerConfig =
    form.kind === "http"
      ? {
          id,
          enabled: true,
          url: form.url.trim(),
          bearerTokenEnvVar: form.bearer.trim() || null,
          transport: "http",
        }
      : {
          id,
          enabled: true,
          command: form.command.trim(),
          args: form.args.trim() ? form.args.trim().split(/\s+/) : [],
          cwd: form.cwd.trim() || null,
          env: parseEnv(form.env),
        };
  await store.upsertMcp(server);
  form.id = "";
  form.command = "";
  form.args = "";
  form.cwd = "";
  form.env = "";
  form.url = "";
  form.bearer = "";
}

async function onDelete(id: string) {
  if (!window.confirm(`删除 MCP ${id}？运行中会先停止。不会停当前对话。`)) return;
  await store.removeMcp(id);
}
</script>
