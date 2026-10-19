<template>
  <section class="agent-chooser" aria-label="选择 Agent 能力">
    <div class="agent-chooser-intro">
      <div><span>当前对话使用</span><strong>{{ currentName }}</strong></div>
      <small>切换 Agent 会用于当前对话；运行中的任务请先停止。</small>
    </div>

    <p v-if="error" class="agent-error" role="alert">{{ error }}</p>
    <div class="agent-section-title"><h2>可用 Agent</h2><span>按需安装，也可连接本机已有程序</span></div>
    <div class="agent-list">
      <article v-for="agent in agents" :key="agent.id" class="agent-card" :class="{ selected: isCurrent(agent.id) }">
        <div class="agent-card-body">
          <span class="agent-icon" aria-hidden="true">{{ initials(agent.id) }}</span>
          <div class="agent-copy">
            <div class="agent-title"><h3>{{ agent.name }}</h3><span v-if="isCurrent(agent.id)" class="agent-current">当前使用</span></div>
            <p>{{ description(agent.id) }}</p>
            <span class="agent-status" :class="{ ready: agent.ready }">{{ stateLabel(agent) }}</span>
          </div>
        </div>

        <div v-if="busy === agent.id && progress[agent.id]" class="agent-progress" role="status" aria-live="polite">
          <div class="agent-progress-line"><strong>{{ phaseLabel(progress[agent.id].phase) }}</strong><span>{{ elapsed(agent.id) }}</span></div>
          <div class="agent-progress-track"><span /></div>
          <p>{{ progress[agent.id].detail }}</p>
          <button type="button" class="agent-cancel" :disabled="cancelling === agent.id" @click="cancelInstall(agent.id)">{{ cancelling === agent.id ? '正在停止…' : '取消安装' }}</button>
        </div>

        <div class="agent-card-actions">
          <button v-if="isCurrent(agent.id) && (store.running || store.queued)" class="agent-stop" type="button" :disabled="!!busy" @click="store.stop()">停止任务</button>
          <button v-else-if="agent.ready" class="agent-primary" :class="{ secondary: isCurrent(agent.id) }" type="button" :disabled="!!busy || store.running || store.queued" @click="choose(agent.id)">{{ isCurrent(agent.id) ? '返回对话' : '使用此 Agent' }}</button>
          <button v-else-if="agent.executable && !agent.managed" class="agent-primary" type="button" :disabled="!!busy || store.running || store.queued" @click="run(agent.id, () => store.connectAgentPlugin(agent.id))">{{ busy === agent.id ? '启用中…' : '启用 Agent' }}</button>
          <button v-else class="agent-primary" type="button" :disabled="!!busy || store.running || store.queued" @click="installAndChoose(agent.id)">{{ busy === agent.id ? '安装中…' : '安装并使用' }}</button>
          <button v-if="agent.id !== 'native'" class="agent-settings" type="button" :aria-expanded="expanded === agent.id" @click="toggleDetails(agent.id)">{{ expanded === agent.id ? '收起设置' : '设置与管理' }}<span aria-hidden="true">{{ expanded === agent.id ? '⌃' : '⌄' }}</span></button>
        </div>

        <div v-if="expanded === agent.id && agent.id !== 'native'" class="agent-details">
          <div class="agent-detail-group">
            <strong>安装来源</strong><span>{{ sourceLabel(agent) }}</span>
            <p>官方 npm 包：<code>{{ agent.package }}</code></p>
            <p v-if="agent.sourceUrl" class="agent-path">{{ agent.sourceUrl }}</p>
            <p v-if="agent.id === 'claude_code'">自动安装会同时下载 CC Switch CLI。运行时使用蒲公英当前模型，并自动启动命令行代理。</p>
            <p v-if="agent.ccSwitchExecutable" class="agent-path" :title="agent.ccSwitchExecutable">CC Switch CLI：{{ agent.ccSwitchExecutable }}</p>
            <p v-else-if="agent.id === 'claude_code' && agent.managed">CC Switch CLI 尚未安装；点击“安装并使用”可补齐。</p>
            <p>蒲公英安装目录</p>
            <p v-if="agent.installDirectory" class="agent-path" :title="agent.installDirectory">{{ agent.installDirectory }}</p>
          </div>
          <div class="agent-detail-group">
            <strong>本机程序位置</strong>
            <p v-if="agent.executable" class="agent-path" :title="agent.executable">{{ agent.executable }}</p>
            <p v-else>未找到程序。可自动安装，或指定本机已有程序。</p>
            <div class="agent-editor">
              <input v-model="paths[agent.id]" :aria-label="agent.name + ' 程序完整路径'" placeholder="粘贴程序的完整路径" />
              <button type="button" :disabled="!!busy" @click="pickProgram(agent.id)">浏览…</button>
              <button type="button" :disabled="!!busy || !paths[agent.id]?.trim()" @click="run(agent.id, () => store.connectAgentPlugin(agent.id, paths[agent.id].trim()))">保存位置</button>
            </div>
            <button v-if="!agent.ready && !agent.executable" type="button" class="agent-text-action" :disabled="!!busy" @click="run(agent.id, () => store.connectAgentPlugin(agent.id))">重新查找本机程序</button>
          </div>
          <div v-if="agent.ready" class="agent-detail-group">
            <label :for="'agent-model-' + agent.id">{{ agent.id === 'claude_code' ? 'Claude Code 模型别名（可选）' : '专用模型 ID（可选）' }}</label>
            <p>{{ modelHint(agent.id) }}</p>
            <div class="agent-editor">
              <input :id="'agent-model-' + agent.id" v-model="models[agent.id]" :placeholder="agent.id === 'claude_code' ? '留空，由 Claude Code / CC Switch 决定' : defaultModel || '留空，尝试使用蒲公英模型'" />
              <button type="button" :disabled="!!busy || models[agent.id] === (agent.model || '')" @click="run(agent.id, () => store.setAgentPluginModel(agent.id, models[agent.id].trim() || undefined))">保存模型</button>
            </div>
          </div>
          <div class="agent-maintenance">
            <button v-if="agent.managed" type="button" :disabled="!!busy || store.running || store.queued" @click="reinstall(agent.id)">重新安装</button>
            <button v-if="agent.ready" type="button" :disabled="!!busy || store.running || store.queued" @click="run(agent.id, () => store.stopAgentPlugin(agent.id))">停止后台会话</button>
            <button v-if="agent.ready || agent.executable || agent.managed" type="button" class="danger" :disabled="!!busy || store.running || store.queued" @click="removeAgent(agent)">{{ agent.managed ? '移除安装' : '断开连接' }}</button>
            <span>{{ agent.managed ? '移除后会删除蒲公英安装的程序。' : '不会删除本机原有程序。' }}</span>
          </div>
        </div>
      </article>
    </div>
    <p class="agent-footnote">“已找到本机程序”只表示可以启动；模型连接会在实际运行时验证。</p>
  </section>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useSessionStore } from "../stores/session";
import type { AgentPluginInfo } from "../lib/types";
import { agentPluginInstallCancel, filePick } from "../lib/ipc";

type InstallEvent = { id: string; phase: string; detail: string };
const store = useSessionStore();
const busy = ref("");
const cancelling = ref("");
const error = ref("");
const expanded = ref<string | null>(null);
const paths = reactive<Record<string, string>>({});
const models = reactive<Record<string, string>>({});
const progress = reactive<Record<string, InstallEvent>>({});
const started = reactive<Record<string, number>>({});
const now = ref(Date.now());
let unlisten: UnlistenFn | undefined;
let clock: ReturnType<typeof setInterval> | undefined;
const agents = computed(() => store.agentPlugins);
const currentId = computed(() => store.active?.prefs.agentEngine || "native");
const currentName = computed(() => agents.value.find((agent) => agent.id === currentId.value)?.name || "蒲公英原生");
const defaultModel = computed(() => String(store.settings?.values["dandelion.defaultModel"] ?? ""));

watch(agents, (items) => {
  for (const item of items) {
    if (!(item.id in models)) models[item.id] = item.model || "";
    if (!(item.id in paths)) paths[item.id] = "";
  }
}, { immediate: true });
onMounted(async () => {
  clock = setInterval(() => { now.value = Date.now(); }, 1000);
  try { unlisten = await listen<InstallEvent>("agent-install-progress", (event) => { progress[event.payload.id] = event.payload; }); }
  catch (err) { error.value = String(err); }
  void store.refreshAgentPlugins().catch((err) => { error.value = String(err); });
});
onUnmounted(() => { unlisten?.(); if (clock) clearInterval(clock); });

function initials(id: string) { return id === "native" ? "蒲" : id === "claude_code" ? "C" : id === "opencode" ? "O" : "π"; }
function isCurrent(id: string) { return currentId.value === id; }
function elapsed(id: string) { return `${Math.max(0, Math.floor((now.value - (started[id] || now.value)) / 1000))} 秒`; }
function phaseLabel(phase: string) { return ({ preparing: "准备安装", downloading: "下载依赖", installing: "安装程序", verifying: "校验程序", completed: "安装完成" } as Record<string, string>)[phase] || "安装中"; }
function description(id: string) {
  if (id === "native") return "蒲公英内置能力，使用你在蒲公英中配置的模型。";
  if (id === "claude_code") return "托管安装会包含 CC Switch CLI，运行时自动连接蒲公英当前模型。";
  if (id === "opencode") return "使用 OpenCode 的 Agent 能力，可尝试使用蒲公英已配置的模型。";
  return "使用 Pi 的 Agent 能力，可尝试使用蒲公英已配置的模型。";
}
function stateLabel(agent: AgentPluginInfo) {
  if (agent.id === "native") return "内置 · 无需安装";
  if (agent.id === "claude_code" && agent.managed && !agent.ccSwitchExecutable) return "需要补装 CC Switch CLI";
  if (agent.ready) return agent.managed ? "蒲公英已安装" : "已找到本机程序";
  return agent.executable ? "已停用" : "可按需安装";
}
function sourceLabel(agent: AgentPluginInfo) {
  if (agent.managed) return "蒲公英安装";
  if (agent.discovered) return "本机自动找到";
  return agent.executable ? "已指定" : "尚未设置";
}
function modelHint(id: string) {
  return id === "claude_code" ? "通常留空；只有使用特定模型别名时才填写。" : "通常留空，蒲公英会尝试复用当前模型。";
}
function toggleDetails(id: string) { expanded.value = expanded.value === id ? null : id; }
async function pickProgram(id: string) {
  try { const selected = await filePick(false); if (selected[0]) paths[id] = selected[0]; }
  catch (err) { error.value = String(err); }
}
async function run(id: string, action: () => Promise<void>) {
  busy.value = id; error.value = "";
  try { await action(); } catch (err) { error.value = String(err); }
  finally { busy.value = ""; cancelling.value = ""; delete progress[id]; delete started[id]; }
}
async function choose(id: AgentPluginInfo["id"]) {
  if (store.running || store.queued) return;
  await run(id, async () => {
    if (!store.active) await store.newTask();
    if (!isCurrent(id)) await store.savePrefs({ agentEngine: id });
    store.returnFromFeature();
  });
}
async function install(id: AgentPluginInfo["id"], chooseAfter: boolean) {
  if (store.running || store.queued) return;
  progress[id] = { id, phase: "preparing", detail: "正在连接安装服务…" };
  started[id] = Date.now();
  await run(id, async () => {
    await store.installAgentPlugin(id);
    if (chooseAfter) {
      if (!store.active) await store.newTask();
      if (!isCurrent(id)) await store.savePrefs({ agentEngine: id });
      store.returnFromFeature();
    }
  });
}
function installAndChoose(id: AgentPluginInfo["id"]) { return install(id, true); }
function reinstall(id: AgentPluginInfo["id"]) { return install(id, false); }
async function cancelInstall(id: string) {
  cancelling.value = id;
  try { await agentPluginInstallCancel(id); }
  catch (err) { cancelling.value = ""; error.value = String(err); }
}
async function removeAgent(agent: AgentPluginInfo) {
  if (store.running || store.queued) return;
  const message = agent.managed
    ? `移除 ${agent.name}？蒲公英安装的程序文件会被删除。`
    : `断开 ${agent.name}？本机原有程序不会被删除。`;
  if (!window.confirm(message)) return;
  await run(agent.id, async () => {
    await store.uninstallAgentPlugin(agent.id);
    if (isCurrent(agent.id) && store.active) await store.savePrefs({ agentEngine: "native" });
  });
}
</script>

<style scoped>
.agent-chooser { color: var(--text); }
.agent-chooser-intro { display: flex; align-items: center; justify-content: space-between; gap: 20px; margin: 0 0 20px; padding: 15px 18px; border-left: 2px solid var(--accent); background: color-mix(in srgb, var(--accent) 5%, var(--panel)); border-radius: 0 7px 7px 0; }
.agent-chooser-intro > div { display: flex; align-items: baseline; flex-wrap: wrap; gap: 12px; }.agent-chooser-intro span, .agent-chooser-intro small { color: var(--muted); font-size: var(--fs-xs); line-height: 1.7; }.agent-chooser-intro strong { font-size: var(--fs-sm); font-weight: 600; }
.agent-section-title { display: flex; align-items: baseline; gap: 16px; margin-bottom: 12px; }.agent-section-title h2 { margin: 0; font-size: 17px; font-weight: 600; }.agent-section-title span { color: var(--muted); font-size: var(--fs-xs); }
.agent-list { border: 1px solid var(--border); border-radius: 10px; overflow: hidden; background: var(--panel); }
.agent-card { display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: center; gap: 0 24px; min-width: 0; border-top: 1px solid var(--border); border-left: 2px solid transparent; }.agent-card:first-child { border-top: 0; }.agent-card.selected { border-left-color: var(--accent); background: color-mix(in srgb, var(--accent) 4%, var(--panel)); }
.agent-card-body { display: flex; align-items: flex-start; gap: 16px; padding: 16px 20px; min-width: 0; }.agent-icon { display: grid; flex: none; place-items: center; width: 40px; height: 40px; border-radius: 10px; background: var(--bg-2); color: var(--accent); font-size: 20px; font-weight: 650; }.agent-copy { min-width: 0; }.agent-title { display: flex; align-items: center; flex-wrap: wrap; gap: 10px; }.agent-title h3 { margin: 0; font-size: 15px; line-height: 1.5; font-weight: 600; }.agent-current { padding: 2px 6px; border-radius: 4px; color: var(--accent); background: color-mix(in srgb, var(--accent) 9%, var(--panel)); font-size: 11px; }.agent-copy p { max-width: 70ch; margin: 5px 0 7px; color: var(--muted); font-size: var(--fs-xs); line-height: 1.7; }.agent-status { display: inline-flex; align-items: center; gap: 6px; color: var(--muted); font-size: 11px; }.agent-status::before { content: ''; width: 5px; height: 5px; border-radius: 50%; background: currentColor; }.agent-status.ready { color: var(--ok); }
.agent-card-actions { display: flex; align-items: center; flex-wrap: wrap; justify-content: flex-end; gap: 8px; padding: 20px 22px 20px 0; }.agent-card-actions button { cursor: pointer; font: inherit; font-size: var(--fs-xs); white-space: nowrap; }
.agent-primary, .agent-stop { min-height: 34px; padding: 8px 12px; border: 1px solid var(--accent); border-radius: 7px; background: var(--accent); color: #fff; font-weight: 550 !important; }.agent-primary:hover { filter: brightness(.94); }.agent-primary.secondary { border-color: var(--border); background: var(--panel); color: var(--text); }.agent-stop { border-color: var(--danger); background: var(--danger); }.agent-settings { display: inline-flex; align-items: center; gap: 7px; min-height: 34px; padding: 7px 10px; border: 1px solid var(--border); border-radius: 7px; background: transparent; color: var(--text); }.agent-settings:hover, .agent-primary.secondary:hover { background: var(--bg-2); }.agent-card button:disabled { opacity: .5; cursor: default; }
.agent-progress { grid-column: 1 / -1; grid-row: 2; margin: 0 22px 18px 76px; padding: 14px; border: 1px solid var(--border); border-radius: 7px; background: var(--bg-2); }.agent-progress-line { display: flex; justify-content: space-between; gap: 12px; font-size: var(--fs-xs); }.agent-progress-line span, .agent-progress p { color: var(--muted); }.agent-progress-track { overflow: hidden; height: 4px; margin: 10px 0; border-radius: 4px; background: var(--bg-3); }.agent-progress-track span { display: block; width: 35%; height: 100%; border-radius: inherit; background: var(--accent); animation: agent-install 1.4s ease-in-out infinite alternate; }@keyframes agent-install { from { transform: translateX(0); } to { transform: translateX(190%); } }.agent-progress p { margin: 0; font-size: var(--fs-xs); line-height: 1.7; overflow-wrap: anywhere; }.agent-cancel { margin-top: 9px; padding: 0; border: 0; background: transparent; color: var(--danger); cursor: pointer; font-size: var(--fs-xs); }
.agent-details { grid-column: 1 / -1; display: grid; grid-template-columns: 1fr 1fr; gap: 24px; padding: 24px; border-top: 1px solid var(--border); background: var(--bg); }.agent-detail-group { display: grid; align-content: start; gap: 7px; min-width: 0; }.agent-detail-group strong, .agent-detail-group label { font-size: var(--fs-xs); font-weight: 600; }.agent-detail-group > span, .agent-detail-group > p { margin: 0; color: var(--muted); font-size: var(--fs-xs); line-height: 1.75; }.agent-detail-group .agent-path { overflow-wrap: anywhere; color: var(--text); }.agent-detail-group code { overflow-wrap: anywhere; }.agent-editor { display: flex; align-items: flex-start; flex-wrap: wrap; gap: 7px; margin-top: 4px; }.agent-editor input { width: 100%; box-sizing: border-box; min-width: 0; padding: 9px 10px; border: 1px solid var(--border); border-radius: 7px; background: var(--panel); color: var(--text); font: inherit; font-size: var(--fs-xs); }.agent-editor button, .agent-maintenance button { padding: 7px 10px; border: 1px solid var(--border); border-radius: 7px; background: var(--panel); color: var(--text); cursor: pointer; font: inherit; font-size: var(--fs-xs); white-space: nowrap; }.agent-text-action { justify-self: start; padding: 4px 0; border: 0; background: transparent; color: var(--accent); cursor: pointer; font-size: var(--fs-xs); }.agent-maintenance { grid-column: 1 / -1; display: flex; align-items: center; flex-wrap: wrap; gap: 9px; padding-top: 16px; border-top: 1px solid var(--border); }.agent-maintenance .danger { border-color: color-mix(in srgb, var(--danger) 35%, var(--border)); color: var(--danger); }.agent-maintenance span { color: var(--muted); font-size: var(--fs-xs); line-height: 1.7; }
.agent-error { padding: 10px 12px; border: 1px solid color-mix(in srgb, var(--danger) 30%, var(--border)); border-radius: 8px; color: var(--danger); font-size: var(--fs-xs); line-height: 1.8; overflow-wrap: anywhere; }.agent-footnote { margin: 16px 0 0; color: var(--muted); font-size: var(--fs-xs); line-height: 1.8; }
.agent-chooser :is(button, input):focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
@media (max-width: 1050px) { .agent-card { grid-template-columns: 1fr; gap: 0; }.agent-card-body { padding-bottom: 12px; }.agent-card-actions { padding: 0 20px 20px 76px; justify-content: flex-start; }.agent-progress { grid-row: auto; }.agent-chooser-intro { align-items: flex-start; flex-direction: column; gap: 5px; } }
@media (max-width: 650px) { .agent-details { grid-template-columns: 1fr; padding: 20px; gap: 20px; }.agent-section-title { flex-direction: column; gap: 7px; }.agent-card-body { padding: 18px 16px 12px; gap: 12px; }.agent-card-actions { padding: 0 16px 18px 68px; }.agent-progress { margin: 0 16px 16px; }.agent-chooser-intro { padding: 14px; } }
@media (max-width: 400px) { .agent-card-actions { padding-left: 16px; } }
@media (prefers-reduced-motion: reduce) { .agent-progress-track span { animation: none; width: 100%; } }
</style>
