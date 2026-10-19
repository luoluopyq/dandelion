<template>
  <div class="security-panel">
    <div class="security-scope" role="status">
      <span class="security-scope-icon">◎</span>
      <div>
        <strong>{{ taskMode ? "当前任务有单独的授权方式" : store.active ? "当前任务继承全局设置" : "新任务使用全局设置" }}</strong>
        <p>{{ taskMode ? `当前任务：${modeName(taskMode)}。下面修改的是全局默认值。` : "此处的更改从下一轮任务执行生效。" }}</p>
      </div>
      <button v-if="taskMode" type="button" @click="openTaskSettings">查看任务设置 ↗</button>
    </div>

    <section class="security-section" aria-labelledby="security-mode-title">
      <div class="security-section-heading">
        <div><h2 id="security-mode-title">默认授权方式</h2><p>只决定需要授权的操作如何处理；工作区限制和系统级禁止始终生效。</p></div>
        <span class="security-section-tag">全局默认</span>
      </div>
      <div class="security-mode-grid" role="group" aria-label="默认授权方式">
        <button v-for="item in modes" :key="item.value" type="button" class="security-mode"
          :class="{ selected: mode === item.value, 'security-mode-risk': item.value === 'auto' }"
          :aria-pressed="mode === item.value" :disabled="busy" @click="selectMode(item.value)">
          <span class="security-mode-top"><span class="security-radio" /><strong>{{ item.title }}</strong><span v-if="item.value === 'ask'" class="security-recommended">推荐</span></span>
          <small>{{ item.description }}</small>
        </button>
      </div>
      <div v-if="confirmAuto" class="security-confirm" role="alert">
        <p><strong>启用自动批准？</strong>写文件、删除类命令和外部 MCP 工具调用将不再逐次询问。工作区限制与系统级禁止仍然生效。</p>
        <div><button type="button" class="button-quiet" @click="confirmAuto = false">取消</button><button type="button" class="button-primary" :disabled="busy" @click="saveMode('auto')">确认启用</button></div>
      </div>
      <p v-if="mode === 'deny' && rules.allow.length" class="security-mode-warning" role="status">已有 {{ rules.allow.length }} 条免询问规则。这些规则命中时仍会放行对应操作；可在下方高级规则中移除。</p>
    </section>

    <section class="security-section" aria-labelledby="security-access-title">
      <div class="security-section-heading"><div><h2 id="security-access-title">本机访问</h2><p>工作区与已添加目录可直接进入；访问其他目录时会单独询问。</p></div></div>
      <div class="security-access-row">
        <div><strong>终端与进程命令</strong><small>关闭后，代理无法调用终端和进程工具；文件和外部工具不受此开关影响。</small></div>
        <label class="security-switch"><input type="checkbox" :checked="shellEnabled" :disabled="busy" aria-label="允许代理使用终端与进程命令" @change="toggleShell" /><span>{{ shellEnabled ? "已开启" : "已关闭" }}</span></label>
      </div>
      <div class="security-access-row">
        <div><strong>可访问目录</strong><small class="security-path" :title="workspacePath">默认工作区：{{ workspacePath }}</small><small v-if="taskWorkspace && taskWorkspace !== workspacePath" class="security-path" :title="taskWorkspace">当前任务：{{ taskWorkspace }}</small><small>额外允许目录：{{ store.sandbox?.extra.length ?? 0 }} 个</small><small v-for="root in store.sandbox?.extra ?? []" :key="root" class="security-path" :title="root">{{ root }}</small></div>
        <button type="button" class="security-text-action" @click="store.openDock('sandbox')">管理目录 ↗</button>
      </div>
    </section>

    <section class="security-section" aria-labelledby="security-effect-title">
      <div class="security-section-heading"><div><h2 id="security-effect-title">这些操作会怎样处理？</h2><p>按当前全局设置展示常见操作；{{ hasRules ? "自定义规则可能改变下列结果。" : "目前没有自定义规则。" }}</p></div></div>
      <div class="security-outcomes">
        <div v-for="row in outcomes" :key="row.name" class="security-outcome">
          <span>{{ row.name }}</span><strong :class="`security-result-${row.kind}`">{{ row.label }}</strong>
        </div>
      </div>
      <p class="security-footnote">“拒绝需确认操作”仍允许读取文件和普通终端命令。目录规则约束文件工具与终端工作目录；终端命令本身仍可访问操作系统允许的其他路径，请按需关闭终端或添加命令规则。</p>
    </section>

    <section class="security-section security-advanced">
      <button type="button" class="security-advanced-toggle" :aria-expanded="showAdvanced" @click="showAdvanced = !showAdvanced">
        <span><strong>高级规则</strong><small>禁止 {{ rules.deny.length }} 条 · 需询问 {{ rules.ask.length }} 条 · 免询问 {{ rules.allow.length }} 条</small></span><span>{{ showAdvanced ? "收起 ⌃" : "展开 ⌄" }}</span>
      </button>
      <div v-if="showAdvanced" class="security-advanced-body">
        <p class="security-rule-order">判断顺序：系统级禁止与自定义禁止 → 目录授权 → 需询问 → 免询问 → 默认授权方式。明确的禁止不会被覆盖；需询问可在弹窗中选择本轮记住。</p>
        <div v-for="side in ruleSides" :key="side.id" class="security-rule-group">
          <div><strong>{{ side.title }}</strong><small>{{ side.description }}</small></div>
          <ul v-if="rules[side.id].length" class="security-rule-list">
            <li v-for="rule in rules[side.id]" :key="rule"><code>{{ rule }}</code><button type="button" :disabled="busy" :aria-label="`移除${side.title}规则 ${rule}`" @click="removeRule(side.id, rule)">移除</button></li>
          </ul>
          <p v-else class="security-rule-empty">暂无规则</p>
          <form class="security-rule-add" @submit.prevent="addRule(side.id)">
            <input v-model="drafts[side.id]" :aria-label="`新增${side.title}规则`" placeholder="如 write_file、write_file:/Users/me/Desktop/*.html、terminal:git push *" />
            <button type="submit" :disabled="busy || !drafts[side.id].trim()">添加</button>
          </form>
        </div>
        <p v-if="ruleError" class="err" role="alert">{{ ruleError }}</p>
        <p class="security-rule-order">工具名支持 *、? 通配；<code>write_file:路径模式</code> 等可限定文件路径；<code>terminal:命令模式</code> 可限定终端命令。规则全局生效，请用完整路径缩小范围。</p>
        <label class="security-timeout"><span><strong>授权等待时间</strong><small>超过时间没有回答时，本次操作会被拒绝。</small></span><span><input type="number" min="1" :value="timeoutSeconds" :disabled="busy" @change="saveTimeout" /> 秒</span></label>
      </div>
    </section>
    <p v-if="store.panelError" class="err" role="alert">{{ store.panelError }}</p>
  </div>
</template>

<script setup lang="ts">
import { computed, reactive, ref } from "vue";
import { useSessionStore } from "../stores/session";

type Mode = "ask" | "auto" | "deny";
type RuleSide = "deny" | "ask" | "allow";
const store = useSessionStore();
const busy = ref(false);
const confirmAuto = ref(false);
const showAdvanced = ref(false);
const ruleError = ref("");
const drafts = reactive<Record<RuleSide, string>>({ deny: "", ask: "", allow: "" });
const vals = computed(() => store.settings?.values ?? {});
const mode = computed<Mode>(() => {
  const raw = vals.value["dandelion.permissionMode"];
  return raw === "auto" || raw === "deny" ? raw : "ask";
});
const taskMode = computed(() => store.active?.prefs.permissionMode ?? null);
const shellEnabled = computed(() => vals.value["dandelion.allowShell"] !== false);
const timeoutSeconds = computed(() => Number(vals.value["dandelion.permissionTimeoutSec"] ?? 120));
const workspacePath = computed(() => store.sandbox?.workspace || String(vals.value["dandelion.workspace"] ?? "尚未读取工作区"));
const taskWorkspace = computed(() => store.active?.prefs.workspace || store.active?.workspace || "");
const rules = computed<Record<RuleSide, string[]>>(() => {
  const value = vals.value["dandelion.permission.rules"] as Partial<Record<RuleSide, string[]>> | undefined;
  return { deny: value?.deny ?? [], ask: value?.ask ?? [], allow: value?.allow ?? [] };
});
const hasRules = computed(() => rules.value.deny.length + rules.value.ask.length + rules.value.allow.length > 0);

const modes: { value: Mode; title: string; description: string }[] = [
  { value: "ask", title: "敏感操作先询问", description: "写入文件、删除类命令等在执行前询问。" },
  { value: "auto", title: "自动批准需确认操作", description: "这些操作不再逐次询问，适合可信任务。" },
  { value: "deny", title: "拒绝需确认操作", description: "阻止写入等操作；读取与普通命令仍可运行。" },
];
const ruleSides: { id: RuleSide; title: string; description: string }[] = [
  { id: "deny", title: "始终禁止", description: "命中时直接拒绝，优先于免询问规则。" },
  { id: "ask", title: "先询问", description: "即使全局设为自动批准，命中时仍向你确认；可在弹窗中选择本轮记住。" },
  { id: "allow", title: "免询问", description: "命中时跳过询问，包括“拒绝需确认操作”模式。" },
];
function modeName(value: Mode) { return modes.find((item) => item.value === value)?.title ?? value; }
function gatedResult(): { label: string; kind: string } {
  if (mode.value === "auto") return { label: "直接执行", kind: "allow" };
  if (mode.value === "deny") return { label: "拒绝", kind: "deny" };
  return { label: "先询问", kind: "ask" };
}
const outcomes = computed(() => {
  const direct = { label: "直接执行", kind: "allow" };
  const blocked = { label: "拒绝", kind: "deny" };
  return [
    { name: "读取与搜索项目文件", ...direct },
    { name: "写入与修改文件", ...gatedResult() },
    { name: "普通终端命令", ...(shellEnabled.value ? direct : blocked) },
    { name: "删除类终端命令", ...(shellEnabled.value ? gatedResult() : blocked) },
    { name: "结束后台进程", ...(shellEnabled.value ? gatedResult() : blocked) },
    { name: "外部 MCP 工具", ...gatedResult() },
    { name: "系统路径与系统级禁止", ...blocked },
  ];
});

async function patch(patchValue: Record<string, unknown>) {
  busy.value = true;
  try { await store.patchSettings(patchValue); }
  finally { busy.value = false; }
}
function selectMode(value: Mode) {
  if (mode.value === value) { confirmAuto.value = false; return; }
  if (value === "auto") { confirmAuto.value = true; return; }
  confirmAuto.value = false;
  void saveMode(value);
}
async function saveMode(value: Mode) {
  await patch({ "dandelion.permissionMode": value });
  confirmAuto.value = false;
}
function toggleShell(ev: Event) {
  void patch({ "dandelion.allowShell": (ev.target as HTMLInputElement).checked });
}
function openTaskSettings() {
  store.closeSettings();
  store.configOpen = true;
}
function validRule(value: string): boolean {
  if (!value || /\r|\n/.test(value)) return false;
  if (value.startsWith("terminal:")) return value.slice("terminal:".length).trim().length > 0;
  if (/^(read_file|write_file|patch|search_files):/.test(value)) return value.slice(value.indexOf(":") + 1).trim().length > 0;
  return /^[A-Za-z0-9_*?-]+$/.test(value);
}
async function addRule(side: RuleSide) {
  const value = drafts[side].trim();
  ruleError.value = "";
  if (!validRule(value)) { ruleError.value = "规则仅支持工具名、文件工具:路径模式或 terminal:命令模式。"; return; }
  if (rules.value[side].includes(value)) { ruleError.value = "这条规则已经存在。"; return; }
  await patch({ "dandelion.permission.rules": { ...rules.value, [side]: [...rules.value[side], value] } });
  if (!store.panelError) drafts[side] = "";
}
async function removeRule(side: RuleSide, value: string) {
  ruleError.value = "";
  await patch({ "dandelion.permission.rules": { ...rules.value, [side]: rules.value[side].filter((rule) => rule !== value) } });
}
function saveTimeout(ev: Event) {
  const value = Number((ev.target as HTMLInputElement).value);
  if (!Number.isInteger(value) || value < 1) { ruleError.value = "授权等待时间必须是大于 0 的整数秒。"; return; }
  ruleError.value = "";
  void patch({ "dandelion.permissionTimeoutSec": value });
}
</script>
