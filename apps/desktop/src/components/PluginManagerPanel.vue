<template>
  <section class="extensions-page">
    <div class="extensions-content">
      <header class="extensions-header">
        <div><h1>插件</h1><p>为当前对话选择 Agent，管理可用技能与工具。</p></div>
        <div class="extensions-header-actions">
          <button type="button" class="button-quiet" @click="refresh">{{ tab === 'agents' ? '重新扫描' : '刷新' }}</button>
          <button v-if="tab !== 'agents'" type="button" class="button-primary" :aria-expanded="importMenu" @click="importMenu = !importMenu">添加 <span aria-hidden="true">⌄</span></button>
          <div v-if="importMenu && tab !== 'agents'" class="extensions-add-menu" aria-label="添加扩展">
            <button type="button" @click="store.importSkill(true); importMenu = false">导入技能文件夹</button>
            <button type="button" @click="store.importSkill(false); importMenu = false">导入 SKILL.md</button>
            <button type="button" @click="remoteSkillOpen = true; importMenu = false; tab = 'skills'">从链接安装技能</button>
            <button type="button" @click="toolFormOpen = true; importMenu = false">配置工具</button>
          </div>
        </div>
      </header>
      <nav class="extensions-tabs" aria-label="扩展分类">
        <button type="button" :class="{ on: tab === 'agents' }" :aria-current="tab === 'agents' ? 'page' : undefined" @click="tab = 'agents'">Agent <span>{{ store.agentPlugins.filter(p => p.ready).length }}</span></button>
        <button type="button" :class="{ on: tab === 'skills' }" :aria-current="tab === 'skills' ? 'page' : undefined" @click="tab = 'skills'">技能 <span>{{ skills.length }}</span></button>
        <button type="button" :class="{ on: tab === 'tools' }" :aria-current="tab === 'tools' ? 'page' : undefined" @click="tab = 'tools'">工具 <span>{{ tools.length }}</span></button>
        <button type="button" :class="{ on: tab === 'discover' }" :aria-current="tab === 'discover' ? 'page' : undefined" @click="tab = 'discover'">发现</button>
      </nav>
      <p v-if="store.panelError" class="extensions-error" role="alert">{{ store.panelError }}</p>
      <ul v-if="store.installNotes.length" class="extensions-notes" aria-label="安装说明"><li v-for="note in store.installNotes" :key="note">{{ note }}</li></ul>

      <form v-if="remoteSkillOpen" class="extension-form" @submit.prevent="installRemoteSkill">
        <div class="extension-form-heading"><h2>从链接安装技能</h2><p>支持 SKILL.md 直链或 Git 仓库。安装后先停用，检查内容后再启用。</p></div>
        <label>来源地址<input v-model="remoteSkill.url" type="url" placeholder="https://github.com/owner/repo" required /></label>
        <label>仓库内目录（可选）<input v-model="remoteSkill.subdir" placeholder="skills/skill-name" /></label>
        <div class="extension-form-actions"><button type="button" class="button-quiet" :disabled="remoteSkillBusy" @click="remoteSkillOpen = false">取消</button><button type="submit" class="button-primary" :disabled="remoteSkillBusy">{{ remoteSkillBusy ? '安装中…' : '安装技能' }}</button></div>
      </form>
      <form v-if="toolFormOpen" class="extension-form" @submit.prevent="saveTool">
        <div class="extension-form-heading"><h2>配置 MCP 工具</h2><p>填写工具名称与连接方式。</p></div>
        <label>工具名称<input v-model="toolForm.id" placeholder="例如 filesystem" required /></label>
        <label>连接方式<select v-model="toolForm.kind" aria-label="连接方式"><option value="stdio">命令行（stdio）</option><option value="http">HTTP</option></select></label>
        <template v-if="toolForm.kind === 'stdio'"><label>启动命令<input v-model="toolForm.command" placeholder="例如 npx" /></label><label>命令参数<input v-model="toolForm.args" placeholder="例如 -y some-mcp-server" /></label></template>
        <label v-else class="extension-form-wide">服务地址<input v-model="toolForm.url" placeholder="MCP 服务地址" /></label>
        <div class="extension-form-actions"><button type="button" class="button-quiet" @click="toolFormOpen = false">取消</button><button type="submit" class="button-primary">保存工具</button></div>
      </form>

      <AgentPluginsPanel v-show="tab === 'agents'" />
      <section v-if="tab !== 'agents'" class="extensions-section" :aria-label="tab === 'skills' ? '技能管理' : tab === 'tools' ? '工具管理' : '发现工具'">
        <div class="extensions-section-heading">
          <div><h2>{{ tab === 'discover' ? '发现工具' : tab === 'skills' ? '已安装技能' : '已配置工具' }}</h2><p>{{ tab === 'skills' ? '管理可用技能，在对话中选择后用于该任务。' : tab === 'tools' ? '查看连接状态，启动或停止已配置的 MCP 工具。' : '在官方 MCP 注册表中查找适合任务的工具。' }}</p></div>
          <label v-if="tab === 'skills' || tab === 'tools'" class="extensions-search"><svg width="16" height="16" viewBox="0 0 20 20" fill="none" aria-hidden="true"><circle cx="8.5" cy="8.5" r="5.5" stroke="currentColor" stroke-width="1.5"/><path d="m13 13 4 4" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/></svg><input v-model="query" type="search" :aria-label="tab === 'skills' ? '搜索技能' : '搜索工具'" :placeholder="tab === 'skills' ? '搜索技能' : '搜索工具'" /></label>
        </div>

        <template v-if="tab === 'skills'">
          <div v-if="filteredSkills.length" class="extensions-items">
            <div class="extensions-columns" aria-hidden="true"><span>技能</span><span>状态与操作</span></div>
            <article v-for="skill in filteredSkills" :key="`skill:${skill.id}`" class="extension-row">
              <span class="extension-icon skill" aria-hidden="true">✦</span>
              <div class="extension-copy"><h3>{{ skill.id }}</h3><p>{{ skill.description || '本地技能' }}</p><p v-if="exceedsSkillLimit(skill)" class="extension-warning">文件超出单个技能容量；调整后才能使用。</p></div>
              <div class="extension-actions"><span class="extension-state" :class="{ enabled: skill.enabled && !exceedsSkillLimit(skill), warning: exceedsSkillLimit(skill) }">{{ exceedsSkillLimit(skill) ? '容量不足' : skill.enabled ? '可用' : '已停用' }}</span><button v-if="exceedsSkillLimit(skill)" type="button" class="button-quiet" @click="store.openSettings('skills')">调整容量</button><label class="extension-toggle"><input type="checkbox" :aria-label="'启用技能 ' + skill.id" :checked="skill.enabled" :disabled="skill.framework || exceedsSkillLimit(skill)" @change="store.setSkillEnabled(skill.id, ($event.target as HTMLInputElement).checked)" /><i aria-hidden="true"></i></label><button type="button" class="extension-text-action" :aria-label="'打开 ' + skill.id + ' 的 SKILL.md'" title="用系统默认应用打开 SKILL.md" @click="store.openSkill(skill.id)">打开文件</button><button type="button" class="extension-text-action remove" :aria-label="'移除技能 ' + skill.id" @click="removeSkill(skill.id)">移除</button></div>
            </article>
          </div>
          <div v-else class="extensions-empty"><span class="extensions-empty-symbol" aria-hidden="true">✦</span><h3>{{ query.trim() ? '没有匹配的技能' : '添加第一个技能' }}</h3><p>{{ query.trim() ? '换一个名称或关键词试试。' : '通过右上角“添加”导入本地技能，或从链接安装。' }}</p></div>
          <p class="extensions-footnote">技能内容在对话中选用后加载；大型技能可能延长本地模型的响应时间。</p>
        </template>
        <template v-if="tab === 'tools'">
          <div v-if="filteredTools.length" class="extensions-items">
            <div class="extensions-columns" aria-hidden="true"><span>工具</span><span>连接状态与操作</span></div>
            <article v-for="tool in filteredTools" :key="`tool:${tool.id}`" class="extension-row">
              <span class="extension-icon tool" aria-hidden="true">⌬</span><div class="extension-copy"><h3>{{ tool.id }}</h3><p>{{ tool.ready ? '已连接，可在任务中选择' : tool.enabled ? '已配置，尚未连接' : '已停用' }}</p></div>
              <div class="extension-actions"><span class="extension-state" :class="{ enabled: tool.ready }">{{ tool.ready ? '已就绪' : tool.enabled ? '待启动' : '已停用' }}</span><button v-if="!tool.ready" type="button" class="button-quiet" @click="store.startMcp(tool.id)">启动</button><button v-else type="button" class="button-quiet" @click="store.stopMcp(tool.id)">停止</button><button type="button" class="extension-text-action remove" :aria-label="'移除工具 ' + tool.id" @click="removeTool(tool.id)">移除</button></div>
            </article>
          </div>
          <div v-else class="extensions-empty"><span class="extensions-empty-symbol" aria-hidden="true">⌬</span><h3>{{ query.trim() ? '没有匹配的工具' : '还没有配置工具' }}</h3><p>{{ query.trim() ? '换一个工具名称试试。' : '通过“添加”配置已有工具，或前往“发现”搜索官方目录。' }}</p></div>
        </template>
        <template v-if="tab === 'discover'">
          <div class="extensions-registry">
            <div class="extensions-registry-intro"><span class="extension-icon tool" aria-hidden="true">⌬</span><div><h3>官方 MCP 注册表</h3><p>搜索名称或用途，找到需要的工具。</p></div></div>
            <form class="extensions-registry-search" @submit.prevent="runSearch"><label><span class="sr-only">搜索官方 MCP 注册表</span><input v-model="registryQuery" placeholder="例如 filesystem、数据库、浏览器" /></label><button type="submit" class="button-primary" :disabled="store.registryBusy">{{ store.registryBusy ? '搜索中…' : '搜索工具' }}</button></form>
            <p class="extensions-registry-note">安装只保存配置，不会自动启动；缺少密钥的工具会保持停用。</p>
          </div>
          <div v-if="store.registryResults.length" class="extensions-items extensions-results"><div class="extensions-columns" aria-hidden="true"><span>搜索结果</span><span>安装</span></div>
            <article v-for="entry in store.registryResults" :key="`reg:${entry.name}:${entry.version}`" class="extension-row"><span class="extension-icon tool" aria-hidden="true">⌬</span><div class="extension-copy"><h3>{{ entry.title || entry.name }}</h3><p>{{ entry.description }}</p><div class="extension-registry-meta"><span>{{ entry.name }}</span><span>v{{ entry.version }}</span><span>{{ formsOf(entry) }}</span></div></div><div class="extension-actions"><button type="button" class="button-quiet" :disabled="store.registryBusy || !installable(entry)" @click="store.installFromRegistry(entry.name, entry.version)">{{ installable(entry) ? '安装' : '不支持' }}</button></div></article>
          </div>
          <div v-if="store.registryCursor" class="extensions-load-more"><button type="button" class="button-quiet" :disabled="store.registryBusy" @click="store.searchRegistry(store.registryQuery, true)">加载更多</button></div>
          <p v-if="!store.registryResults.length && !store.registryBusy" class="extensions-discovery-note">输入关键词开始查找，搜索结果会显示在这里。</p>
          <p v-if="store.registryBusy" class="extensions-discovery-note" role="status">正在查询官方目录…</p>
        </template>
      </section>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed, reactive, ref } from "vue";
import { useSessionStore } from "../stores/session";
import type { McpRegistryEntry, McpServerConfig, SkillListItem } from "../lib/types";
import AgentPluginsPanel from "./AgentPluginsPanel.vue";
const store = useSessionStore();
const tab = ref<"agents" | "skills" | "tools" | "discover">("agents"), query = ref(""), importMenu = ref(false);
const registryQuery = ref("");
const toolFormOpen = ref(false);
const toolForm = reactive({ id: "", kind: "stdio" as "stdio" | "http", command: "", args: "", url: "" });
const remoteSkillOpen = ref(false);
const remoteSkillBusy = ref(false);
const remoteSkill = reactive({ url: "", subdir: "" });
const normalized = computed(() => query.value.trim().toLowerCase());
const skillLimit = computed(() => Number(store.settings?.values["dandelion.skills.maxInjectBytes"] ?? 32768));
function exceedsSkillLimit(skill: SkillListItem) { return skill.sizeBytes + new TextEncoder().encode("已启用技能全文：\n").length > skillLimit.value; }
const skills = computed(() => store.skills.filter((item) => !item.framework));
const tools = computed(() => store.mcpServers);
const filteredSkills = computed(() => skills.value.filter((item) => `${item.id} ${item.description}`.toLowerCase().includes(normalized.value)));
const filteredTools = computed(() => tools.value.filter((item) => item.id.toLowerCase().includes(normalized.value)));
function refresh() { void (tab.value === "agents" ? store.refreshAgentPlugins() : store.refreshCatalog()); }
function formsOf(entry: McpRegistryEntry) { const forms = entry.variants.filter((v) => v.supported).map((v) => v.label); return forms.length ? forms.join(" / ") : "无受支持的安装形式"; }
function installable(entry: McpRegistryEntry) { return entry.variants.some((v) => v.supported); }
function runSearch() { void store.searchRegistry(registryQuery.value); }
async function installRemoteSkill() {
  const url = remoteSkill.url.trim();
  if (!url || remoteSkillBusy.value) return;
  remoteSkillBusy.value = true;
  try {
    if (url.toLowerCase().endsWith(".md") && !remoteSkill.subdir.trim()) await store.installSkillFromUrl(url);
    else await store.installSkillFromGit(url, remoteSkill.subdir.trim() || null);
    if (!store.panelError) {
      remoteSkillOpen.value = false;
      remoteSkill.url = "";
      remoteSkill.subdir = "";
      tab.value = "skills";
    }
  } finally {
    remoteSkillBusy.value = false;
  }
}
async function removeSkill(id: string) { if (!window.confirm(`删除技能 ${id}？会删掉 data_dir/skills 下的目录。`)) return; await store.removeSkill(id); }
async function removeTool(id: string) { if (!window.confirm(`删除工具 ${id}？运行中会先停止，不会停当前对话。`)) return; await store.removeMcp(id); }
async function saveTool() { const id = toolForm.id.trim(); if (!id) return; const config: McpServerConfig = toolForm.kind === "http" ? { id, enabled: true, url: toolForm.url.trim(), transport: "http" } : { id, enabled: true, command: toolForm.command.trim(), args: toolForm.args.trim() ? toolForm.args.trim().split(/\s+/) : [], cwd: null }; await store.upsertMcp(config); toolFormOpen.value = false; toolForm.id = ""; toolForm.command = ""; toolForm.args = ""; toolForm.url = ""; }
</script>

<style scoped>
.extensions-page { flex: 1; min-width: 0; min-height: 0; overflow: auto; box-sizing: border-box; padding: 28px clamp(24px, 5vw, 80px) 40px; background: var(--bg); color: var(--text); }
.extensions-content { max-width: 1120px; margin: 0 auto; }
.extensions-header { display: flex; align-items: center; justify-content: space-between; gap: 24px; margin-bottom: 20px; }
.extensions-header h1 { margin: 0 0 8px; font-size: 26px; font-weight: 650; letter-spacing: -.03em; line-height: 1.3; }
.extensions-header p { margin: 0; font-size: var(--fs-sm); color: var(--muted); line-height: 1.75; }
.extensions-header-actions { position: relative; display: flex; align-items: center; gap: 8px; flex-shrink: 0; }
.extensions-add-menu { position: absolute; z-index: 20; top: calc(100% + 8px); right: 0; display: grid; min-width: 185px; padding: 5px; border: 1px solid var(--border); border-radius: 9px; background: var(--panel); box-shadow: var(--shadow-menu); }
.extensions-add-menu button { border: 0; background: transparent; border-radius: 5px; padding: 10px; font: inherit; font-size: var(--fs-xs); text-align: left; color: var(--text); cursor: pointer; }
.extensions-add-menu button:hover { background: var(--hover); }
.extensions-tabs { display: flex; gap: 28px; border-bottom: 1px solid var(--border); margin-bottom: 20px; }
.extensions-tabs button { display: flex; align-items: center; gap: 8px; border: 0; border-bottom: 2px solid transparent; background: transparent; padding: 12px 0; color: var(--muted); font: inherit; font-size: var(--fs-sm); cursor: pointer; }
.extensions-tabs button.on { border-bottom-color: var(--accent); color: var(--accent); font-weight: 600; }
.extensions-tabs span { min-width: 14px; color: var(--muted); font-size: 11px; font-weight: 500; text-align: center; }
.extensions-section-heading { display: flex; align-items: center; justify-content: space-between; gap: 24px; margin: 26px 0 20px; }
.extensions-section-heading h2, .extension-form h2 { margin: 0; font-size: 17px; font-weight: 600; }
.extensions-section-heading p { margin: 7px 0 0; font-size: var(--fs-xs); line-height: 1.75; color: var(--muted); }
.extensions-search { display: flex; align-items: center; gap: 9px; padding: 0 11px; border: 1px solid var(--border); border-radius: 7px; background: var(--panel); color: var(--muted); width: 260px; flex-shrink: 0; }
.extensions-search input { min-width: 0; width: 100%; border: 0; outline: 0; padding: 10px 0; background: transparent; color: var(--text); font: inherit; font-size: var(--fs-xs); }
.extensions-items { border: 1px solid var(--border); border-radius: 10px; background: var(--panel); overflow: hidden; }
.extensions-columns { display: flex; justify-content: space-between; padding: 11px 20px; border-bottom: 1px solid var(--border); color: var(--muted); font-size: 11px; background: color-mix(in srgb, var(--bg) 65%, var(--panel)); }
.extension-row { display: flex; align-items: center; gap: 16px; padding: 22px 20px; border-bottom: 1px solid var(--border); min-width: 0; }
.extension-row:last-child { border-bottom: 0; }
.extension-icon { display: grid; place-items: center; width: 40px; height: 40px; flex-shrink: 0; border-radius: 10px; background: var(--bg-2); color: var(--accent); font-size: 20px; }
.extension-icon.tool { color: var(--ok); background: color-mix(in srgb, var(--ok) 7%, var(--panel)); }
.extension-copy { flex: 1; min-width: 0; }
.extension-copy h3 { margin: 0; color: var(--text); font-size: 15px; font-weight: 600; overflow-wrap: anywhere; line-height: 1.5; }
.extension-copy p { max-width: 75ch; margin: 5px 0 0; color: var(--muted); font-size: var(--fs-xs); line-height: 1.75; overflow-wrap: anywhere; white-space: pre-line; }
.extension-copy .extension-warning { color: var(--warn); }
.extension-actions { display: flex; align-items: center; justify-content: flex-end; flex-wrap: wrap; gap: 14px; flex-shrink: 0; }
.extension-state { display: inline-flex; align-items: center; gap: 6px; color: var(--muted); font-size: 11px; white-space: nowrap; }
.extension-state::before { content: ''; width: 5px; height: 5px; border-radius: 50%; background: currentColor; }
.extension-state.enabled { color: var(--ok); }.extension-state.warning { color: var(--warn); }
.extension-text-action { border: 0; padding: 6px 0; background: transparent; color: var(--text); cursor: pointer; font: inherit; font-size: var(--fs-xs); white-space: nowrap; }
.extension-text-action:hover { color: var(--accent); }.extension-text-action.remove { color: var(--muted); }.extension-text-action.remove:hover { color: var(--danger); }
.extension-toggle { position: relative; display: inline-flex; width: 31px; height: 18px; flex: none; cursor: pointer; }
.extension-toggle input { position: absolute; inset: 0; width: 100%; height: 100%; opacity: 0; margin: 0; z-index: 1; cursor: pointer; }
.extension-toggle i { position: absolute; inset: 0; border-radius: 10px; background: var(--bg-3); border: 1px solid var(--border); }
.extension-toggle i::after { content: ''; position: absolute; top: 2px; left: 2px; width: 12px; height: 12px; border-radius: 50%; background: var(--surface); box-shadow: 0 1px 2px #0002; }
.extension-toggle input:checked + i { background: var(--accent); border-color: var(--accent); }.extension-toggle input:checked + i::after { transform: translateX(13px); }.extension-toggle input:disabled { cursor: default; }.extension-toggle input:disabled + i { opacity: .45; }
.extensions-footnote { margin: 16px 0 0; color: var(--muted); font-size: var(--fs-xs); line-height: 1.8; }
.extensions-empty { display: flex; flex-direction: column; align-items: center; justify-content: center; min-height: 260px; padding: 30px; border: 1px dashed var(--border); border-radius: 10px; text-align: center; }
.extensions-empty-symbol { display: grid; place-items: center; width: 48px; height: 48px; margin-bottom: 18px; border-radius: 12px; color: var(--accent); background: var(--bg-2); font-size: 24px; }
.extensions-empty h3 { font-size: 17px; font-weight: 500; margin: 0 0 9px; }.extensions-empty p { font-size: var(--fs-sm); line-height: 1.8; color: var(--muted); margin: 0; max-width: 42ch; }
.extensions-registry { padding: 24px; border: 1px solid var(--border); border-radius: 10px; background: var(--panel); }
.extensions-registry-intro { display: flex; align-items: center; gap: 14px; margin-bottom: 22px; }.extensions-registry-intro h3 { margin: 0 0 4px; font-size: 15px; font-weight: 600; }.extensions-registry-intro p { margin: 0; color: var(--muted); font-size: var(--fs-xs); line-height: 1.7; }
.extensions-registry-search { display: flex; gap: 10px; max-width: 700px; }.extensions-registry-search label { flex: 1; min-width: 0; }.extensions-registry-search input { width: 100%; box-sizing: border-box; border: 1px solid var(--border); border-radius: 7px; padding: 10px 12px; font: inherit; font-size: var(--fs-sm); background: var(--bg); color: var(--text); }
.extensions-registry-note { margin: 14px 0 0; font-size: var(--fs-xs); color: var(--muted); line-height: 1.8; }.extensions-discovery-note { margin: 32px 0; color: var(--muted); font-size: var(--fs-xs); text-align: center; }
.extensions-results { margin-top: 24px; }.extension-registry-meta { display: flex; flex-wrap: wrap; gap: 6px 14px; color: var(--muted); font-size: 11px; margin-top: 9px; overflow-wrap: anywhere; }.extensions-load-more { text-align: center; margin: 20px 0; }
.extension-form { display: grid; grid-template-columns: 1fr 1fr; gap: 18px; padding: 24px; margin: 20px 0; border: 1px solid var(--border); border-radius: 10px; background: var(--panel); }
.extension-form-heading, .extension-form-wide, .extension-form-actions { grid-column: 1 / -1; }.extension-form-heading p { margin: 7px 0 0; color: var(--muted); font-size: var(--fs-xs); line-height: 1.8; }.extension-form label { display: grid; gap: 8px; color: var(--text); font-size: var(--fs-xs); }.extension-form input, .extension-form select { box-sizing: border-box; width: 100%; min-width: 0; padding: 10px 12px; border: 1px solid var(--border); border-radius: 7px; background: var(--bg); color: var(--text); font: inherit; font-size: var(--fs-sm); }.extension-form-actions { display: flex; gap: 8px; justify-content: flex-end; }
.extensions-error { padding: 12px 14px; border: 1px solid color-mix(in srgb, var(--danger) 30%, var(--border)); border-radius: 8px; color: var(--danger); font-size: var(--fs-xs); line-height: 1.8; overflow-wrap: anywhere; }.extensions-notes { padding: 12px 14px 12px 30px; border-radius: 8px; background: var(--bg-2); color: var(--muted); font-size: var(--fs-xs); line-height: 1.8; }
.sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0,0,0,0); white-space: nowrap; border: 0; }
.extensions-page :is(button, input, select):focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }.extensions-search:focus-within { outline: 2px solid var(--accent); outline-offset: 3px; }.extensions-search input:focus-visible { outline: 0; }.extension-toggle input:focus-visible + i { outline: 2px solid var(--accent); outline-offset: 3px; }.extensions-page button:disabled { opacity: .5; cursor: default; }
@media (max-width: 1000px) { .extension-row { flex-wrap: wrap; }.extension-copy { flex-basis: calc(100% - 56px); }.extension-actions { width: 100%; padding-left: 56px; box-sizing: border-box; justify-content: flex-start; }.extensions-columns span:last-child { display: none; } }
@media (max-width: 650px) { .extensions-page { padding: 24px 18px 32px; }.extensions-header, .extensions-section-heading { flex-direction: column; align-items: flex-start; gap: 16px; }.extensions-header { margin-bottom: 16px; }.extensions-tabs { gap: 24px; }.extensions-search { width: 100%; box-sizing: border-box; }.extension-row { padding: 18px 16px; gap: 12px; }.extension-actions { padding-left: 0; gap: 14px; }.extension-copy { flex-basis: calc(100% - 52px); }.extension-form { grid-template-columns: 1fr; padding: 20px; }.extensions-registry { padding: 20px; }.extensions-registry-search { flex-wrap: wrap; }.extensions-registry-search label { flex-basis: 100%; }.extensions-registry-search button { margin-left: auto; }.extensions-empty { padding: 24px; } }
</style>
