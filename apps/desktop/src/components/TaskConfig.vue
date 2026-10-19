<template>
  <form v-if="store.active" class="task-cfg" @submit.prevent>
    <p class="hint">只改当前任务。空字段继承全局 Settings，下一轮 `prompt` 才生效。</p>
    <div class="task-agent-picker">
      <label>Agent 引擎
        <select :value="store.active.prefs.agentEngine || 'native'" :disabled="store.running || store.queued" @change="onAgent">
          <option value="native">蒲公英原生 · 默认</option>
          <option v-for="plugin in availableAgents" :key="plugin.id" :value="plugin.id" :disabled="!plugin.ready">{{ plugin.name }}{{ plugin.ready ? '' : ' · 需要配置' }}</option>
        </select>
      </label>
      <button type="button" @click="store.openPlugins()">管理 Agent 插件</button>
      <small>手动切换，只影响后续消息；当前执行结束后才可更换。外部 Agent 使用自己的工具和授权规则；下方的蒲公英 Skill、MCP 和知识库绑定不会自动传给它。</small>
    </div>
    <label>
      模型（本任务覆盖）
      <select :value="store.active.prefs.model || globalModel" @change="onModel">
        <option v-for="m in models" :key="m" :value="m">{{ m }}</option>
      </select>
    </label>
    <button type="button" @click="store.openSettings('models')">管理模型</button>
    <label>
      temperature
      <input
        type="number"
        step="0.05"
        min="0"
        max="2"
        :value="store.active.prefs.temperature ?? globalTemp"
        @change="onNum('temperature', ($event.target as HTMLInputElement).value)"
      />
    </label>
    <label>
      maxTokens（本任务覆盖全局）
      <input
        type="number"
        min="1"
        :value="store.active.prefs.maxTokens ?? globalMax"
        @change="onNum('maxTokens', ($event.target as HTMLInputElement).value)"
      />
    </label>
    <label>
      当前任务的授权方式
      <select
        :value="store.active.prefs.permissionMode || globalPerm"
        @change="onPerm"
      >
        <option value="ask">敏感操作先询问</option>
        <option value="auto">自动批准需确认操作</option>
        <option value="deny">拒绝需确认操作</option>
      </select>
    </label>
    <p v-if="['pi', 'opencode'].includes(store.active.prefs.agentEngine || '') && (store.active.prefs.permissionMode || globalPerm) === 'ask'" class="hint">外部 Agent 暂不能显示蒲公英的逐项授权弹窗。当前会安全地按受限模式运行：Pi 不使用工具，OpenCode 仅允许读取。需要编辑或运行命令时，请主动选择“自动批准”。</p>
    <label class="block">
      任务说明（由 core 组装进 system，Vue 不调模型）
      <textarea
        rows="3"
        :value="store.active.prefs.systemPrompt ?? ''"
        @change="onPrompt"
      />
    </label>
    <label class="block">
      任务沙箱路径
      <input
        :value="store.active.prefs.workspace || store.active.workspace"
        @change="onWorkspace"
      />
    </label>
    <button type="button" @click="sandboxPickerOpen = !sandboxPickerOpen">从白名单选</button>
    <div v-if="sandboxPickerOpen" class="task-sandbox-picker">
      <button type="button" @click="chooseSandbox(store.sandbox?.workspace || '')">默认工作区</button>
      <button v-for="path in store.sandbox?.extra ?? []" :key="path" type="button" :title="path" @click="chooseSandbox(path)">{{ path }}</button>
      <button type="button" @click="store.addSandboxRoot()">添加白名单目录…</button>
    </div>

    <p class="badge-row">
      <button
        type="button"
        class="badge"
        :class="{ on: store.contextStatus?.injected }"
        :disabled="!store.contextStatus?.injected"
        :title="store.contextStatus?.injected ? '查看已注入的项目须知' : '当前工作区未发现项目须知'"
        @click="store.openContextBadge()"
      >
        已注入
      </button>
    </p>

    <fieldset>
      <legend>当前任务使用的技能</legend>
      <p class="muted">勾选后才把技能内容交给蒲公英原生 Agent；此选择会保留在当前对话中。</p>
      <p v-if="!enabledSkills.length" class="muted">暂无可用技能。可以到插件页导入或安装。</p>
      <p v-if="store.panelError" class="hint" role="alert">{{ store.panelError }}</p>
      <label v-for="sk in enabledSkills" :key="sk.id" class="chk">
        <input type="checkbox" :checked="store.active.skillIds.includes(sk.id)" :disabled="store.active.prefs.agentEngine !== undefined && store.active.prefs.agentEngine !== 'native'" @change="toggleSkill(sk.id, ($event.target as HTMLInputElement).checked)" />
        <span>{{ sk.id }}</span>
        <small>{{ sk.description }}</small>
      </label>
      <button type="button" @click="store.openPlugins()">管理 Skill</button>
    </fieldset>

    <fieldset>
      <legend>任务资料（勾选后用于当前任务）</legend>
      <p v-if="!readyCorpus.length" class="muted">没有可勾选的资料。添加后仅在当前任务中使用。</p>
      <label v-for="d in readyCorpus" :key="d.id" class="chk">
        <input
          type="checkbox"
          :checked="store.active.corpusIds.includes(d.id)"
          @change="toggleCorpus(d.id, ($event.target as HTMLInputElement).checked)"
        />
        <span>{{ d.name }}</span>
        <small>{{ d.chunks }} 块</small>
      </label>
      <button type="button" @click="store.openCorpus()">管理任务资料</button>
    </fieldset>

    <fieldset>
      <legend>MCP（02 勾选才进 tools[]）</legend>
      <p v-if="!enabledMcp.length" class="muted">没有可勾选的 MCP。到 Dock MCP 添加（全局启用）。</p>
      <label v-for="m in enabledMcp" :key="m.id" class="chk">
        <input
          type="checkbox"
          :checked="store.active.mcpIds.includes(m.id)"
          @change="toggleMcp(m.id, ($event.target as HTMLInputElement).checked)"
        />
        <span>{{ m.id }}</span>
        <small>{{ mcpStatus(m) }}</small>
      </label>
      <button type="button" @click="store.openPlugins()">管理 MCP</button>
    </fieldset>
  </form>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useSessionStore } from "../stores/session";

const store = useSessionStore();
onMounted(() => { void store.refreshAgentPlugins(); });
const sandboxPickerOpen = ref(false);
const enabledSkills = computed(() => store.skills.filter((s) => !s.framework && s.enabled));
const enabledMcp = computed(() => store.mcpServers.filter((m) => m.enabled));
const readyCorpus = computed(() => store.corpusDocs.filter((d) => d.status === "ready"));
const availableAgents = computed(() => store.agentPlugins.filter((p) => p.id !== "native" && p.installed));

function onAgent(ev: Event) {
  const agentEngine = (ev.target as HTMLSelectElement).value as "native" | "claude_code" | "opencode" | "pi";
  void store.savePrefs({ agentEngine });
}

const globalModel = computed(() => String(store.settings?.values["dandelion.defaultModel"] ?? ""));
const globalTemp = computed(() => Number(store.settings?.values["dandelion.temperature"] ?? 0.2));
const globalMax = computed(() => Number(store.settings?.values["dandelion.maxTokens"] ?? 8192));
const globalPerm = computed(() => String(store.settings?.values["dandelion.permissionMode"] ?? "ask"));

const models = computed(() => {
  const set = new Set<string>();
  if (globalModel.value) set.add(globalModel.value);
  if (store.active?.prefs.model) set.add(store.active.prefs.model);
  return [...set];
});

function onModel(ev: Event) {
  const v = (ev.target as HTMLSelectElement).value;
  void store.savePrefs({ model: v });
}

function onNum(key: "temperature" | "maxTokens", raw: string) {
  const n = Number(raw);
  if (!Number.isFinite(n)) return;
  void store.savePrefs({ [key]: n });
}

function onPerm(ev: Event) {
  const v = (ev.target as HTMLSelectElement).value as "ask" | "auto" | "deny";
  void store.savePrefs({ permissionMode: v });
}

function onPrompt(ev: Event) {
  void store.savePrefs({ systemPrompt: (ev.target as HTMLTextAreaElement).value });
}

function onWorkspace(ev: Event) {
  void store.savePrefs({ workspace: (ev.target as HTMLInputElement).value });
}

function chooseSandbox(path: string) {
  if (!path) return;
  void store.applySandboxToTask(path);
  sandboxPickerOpen.value = false;
}

function toggleMcp(id: string, on: boolean) {
  const cur = store.active?.mcpIds ?? [];
  const next = on ? [...new Set([...cur, id])] : cur.filter((x) => x !== id);
  void store.setMcpIds(next);
}

function toggleSkill(id: string, on: boolean) {
  const cur = store.active?.skillIds ?? [];
  const next = on ? [...new Set([...cur, id])] : cur.filter((x) => x !== id);
  void store.setSkillIds(next);
}

function toggleCorpus(id: string, on: boolean) {
  const cur = store.active?.corpusIds ?? [];
  const next = on ? [...new Set([...cur, id])] : cur.filter((x) => x !== id);
  void store.setCorpusIds(next);
}

function mcpStatus(m: { ready: boolean; enabled: boolean; error?: string | null }) {
  if (!m.enabled) return "disabled";
  if (m.ready) return "ready";
  if (m.error) return "failed";
  return "stopped";
}
</script>

<style scoped>
.task-agent-picker { display:grid; gap:9px; padding:15px; border:1px solid #e8e1f1; border-radius:13px; background:#faf8ff; }
.task-agent-picker label { font-weight:650; color:#332744; }
.task-agent-picker select { margin-top:7px; }
.task-agent-picker button { justify-self:start; }
.task-agent-picker small { color:#746d7c; line-height:1.5; }
</style>
