<template>
  <section class="project-memory">
    <header class="pm-header">
      <div><h1>项目记忆</h1><p>管理这个项目需要跨对话沿用的事实与偏好。</p></div>
      <div class="pm-actions">
        <button type="button" class="button-quiet" :disabled="!projectPath" @click="openPreview">检查效果</button>
        <button type="button" class="button-primary" :disabled="!projectPath || mutationBusy" @click="openEditor()">＋ 添加记忆</button>
        <details ref="moreMenu" class="pm-more"><summary aria-label="更多记忆操作">更多</summary><div>
          <button type="button" @click="openSettings('skills')">记忆使用设置</button>
          <button type="button" @click="openSettings('memory')">旧版记忆文件</button>
          <button type="button" :disabled="!projectPath" @click="openDiagnostics">诊断信息</button>
        </div></details>
      </div>
    </header>

    <div class="pm-project">
      <label><span>项目</span><select v-model="projectPath" aria-label="记忆所属项目"><option value="" disabled>{{ projectOptions.length ? '选择项目' : '暂无项目' }}</option><option v-for="project in projectOptions" :key="project.workspace" :value="project.workspace">{{ project.name }}</option></select></label>
      <span v-if="projectPath" class="pm-path" :title="projectPath">{{ projectPath }}</span>
      <span class="pm-use-state" :data-enabled="memoryEnabled"><i aria-hidden="true"></i>{{ memoryEnabled ? '对话按相关性选取' : '自动使用已关闭' }}</span>
    </div>
    <p v-if="!memoryEnabled" class="pm-state-note">记忆仍保存在本机，但当前不会自动用于对话。<button type="button" @click="openSettings('skills')">打开使用设置</button></p>
    <p v-if="error" class="corpus-error" role="alert">{{ error }}</p>
    <p v-if="notice" class="pm-notice" role="status">{{ notice }}</p>

    <nav class="pm-tabs" aria-label="项目记忆视图">
      <button v-for="item in views" :key="item.id" type="button" :class="{ active: view === item.id }" :aria-current="view === item.id ? 'page' : undefined" @click="view = item.id">{{ item.label }}<span :class="{ 'pm-pending-count': item.id === 'pending' && item.count > 0 }">{{ item.count }}</span></button>
    </nav>
    <div class="pm-toolbar">
      <label class="pm-search"><svg width="15" height="15" viewBox="0 0 20 20" fill="none" aria-hidden="true"><circle cx="8.5" cy="8.5" r="5.5" stroke="currentColor" stroke-width="1.5"/><path d="m13 13 4 4" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/></svg><input v-model="filter" type="search" placeholder="搜索记忆内容" aria-label="搜索项目记忆" :disabled="!projectPath" /></label>
      <select v-model="kindFilter" aria-label="筛选记忆类型" :disabled="!projectPath"><option value="all">全部类型</option><option value="fact">事实</option><option value="preference">偏好</option><option value="decision">决策</option><option value="other">其他</option></select>
      <button type="button" class="pm-refresh" :disabled="loading || mutationBusy || !projectPath" @click="refresh">{{ loading ? '刷新中…' : '刷新' }}</button>
    </div>
    <p v-if="view === 'pending'" class="pm-tab-note">来自对话或助手的提议。只有经你确认，才会用于后续对话。</p>
    <p v-else-if="view === 'history'" class="pm-tab-note">保留添加、编辑和忘记的记录，历史版本不会作为当前记忆使用。</p>

    <div v-if="loading && !events.length" class="pm-empty" role="status">正在读取项目记忆…</div>
    <section v-else-if="view === 'history' && historyRows.length" class="pm-history" aria-label="记忆变更记录">
      <article v-for="row in historyRows" :key="row.event.id" class="pm-history-row">
        <div class="pm-history-mark" :data-action="row.event.kind" aria-hidden="true"></div>
        <div><div class="pm-history-heading"><strong>{{ row.action }}</strong><span class="pm-kind">{{ memoryKindLabel(row.kind) }}</span><span v-if="row.current" class="pm-history-current">{{ row.event.authority === 'model_candidate' ? '待确认' : '当前版本' }}</span><time>{{ formatTime(row.event.observedAt) }}</time></div><p>{{ row.content }}</p></div>
      </article>
    </section>
    <section v-else-if="view !== 'history' && rows.length" class="pm-workspace" aria-label="项目记忆列表与详情">
      <div class="pm-list" aria-label="记忆列表">
        <button v-for="event in rows" :key="event.id" type="button" class="pm-row" :class="{ selected: event.id === selectedId }" :aria-pressed="event.id === selectedId" @click="selectedId = event.id">
          <span class="pm-row-heading"><span class="pm-kind">{{ memoryKindLabel(event.kind) }}</span><time>{{ formatDate(event.observedAt) }}</time></span>
          <span class="pm-row-content">{{ memoryText(event) }}</span>
          <span class="pm-row-origin">{{ memorySourceLabel(event, events) }}</span>
        </button>
      </div>
      <article v-if="selected" class="pm-detail" aria-label="记忆详情">
        <div class="pm-detail-heading"><span>记忆详情</span><span class="pm-status" :class="{ pending: view === 'pending' }">{{ view === 'pending' ? '等待你确认' : '已确认' }}</span></div>
        <p class="pm-detail-content">{{ memoryText(selected) }}</p>
        <dl class="pm-detail-meta">
          <dt>类型</dt><dd>{{ memoryKindLabel(selected.kind) }}</dd>
          <dt>来源</dt><dd>{{ memorySourceLabel(selected, events) }}<button v-if="sourceSessionId" type="button" class="pm-text-button" @click="store.openSession(sourceSessionId)">查看来源对话</button></dd>
          <dt>{{ selected.revisionOf ? '更新于' : '记录于' }}</dt><dd>{{ formatTime(selected.observedAt) }}</dd>
          <dt>使用范围</dt><dd>{{ projectName }}<small>仅用于同一工作目录的对话</small></dd>
          <template v-if="selected.validTo"><dt>有效期至</dt><dd>{{ formatTime(selected.validTo) }}</dd></template>
        </dl>
        <p class="pm-detail-note">{{ view === 'pending' ? '确认前，这条提议不会影响任务。你也可以先修改表述再确认。' : '已确认不代表每轮必用。助手会根据当前问题选取相关记忆。' }}</p>
        <div v-if="view === 'pending'" class="pm-detail-actions"><button type="button" class="button-primary" :disabled="mutationBusy" @click="confirmMemory(selected)">确认记忆</button><button type="button" class="button-quiet" :disabled="mutationBusy" @click="openEditor(selected)">编辑后确认</button><button type="button" class="pm-subtle" :disabled="mutationBusy" @click="forgetTarget = selected">忽略</button></div>
        <div v-else class="pm-detail-actions"><button type="button" class="button-quiet" :disabled="mutationBusy" @click="openEditor(selected)">编辑记忆</button><button type="button" class="pm-subtle" :disabled="mutationBusy" @click="forgetTarget = selected">忘记</button></div>
      </article>
    </section>
    <div v-else class="pm-empty">
      <svg width="52" height="52" viewBox="0 0 52 52" fill="none" aria-hidden="true"><rect x="11" y="8" width="31" height="37" rx="4" stroke="currentColor" stroke-width="1.4"/><path d="M17 8v37M23 18h12M23 25h9M23 32h11" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/><path d="M8 17h6M8 26h6M8 35h6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/></svg>
      <h2>{{ emptyTitle }}</h2><p>{{ emptyDescription }}</p>
      <button v-if="projectPath && view === 'confirmed' && !filter && kindFilter === 'all'" type="button" class="button-primary" @click="openEditor()">添加第一条记忆</button>
      <button v-else-if="filter || kindFilter !== 'all'" type="button" class="button-quiet" @click="clearFilters">清除筛选</button>
    </div>

    <footer class="pm-footer"><span>记忆保存项目约定与偏好；参考文档请放在知识库。</span><button type="button" @click="store.openKnowledge()">管理本地资料</button></footer>

    <n-modal :show="editorOpen" preset="card" :title="editorTitle" class="pm-modal" style="width: 600px; max-width: calc(100vw - 32px)" :mask-closable="!mutationBusy" :closable="!mutationBusy" @close="editorOpen = false" @update:show="value => { if (!mutationBusy) editorOpen = value; }">
      <form class="pm-editor" @submit.prevent="saveEditor">
        <p>保存到 <strong>{{ projectName }}</strong>。写下一条清楚、可以长期沿用的约定。</p>
        <label>记忆内容<textarea v-model="draft" rows="6" :maxlength="MEMORY_CONTENT_BYTES" placeholder="例如：这个项目的界面文案统一使用简体中文。" aria-label="记忆内容" :disabled="mutationBusy" autofocus /></label>
        <div class="pm-editor-meta"><label v-if="!editorTarget">类型<select v-model="draftKind" aria-label="记忆类型" :disabled="mutationBusy"><option value="fact">事实</option><option value="preference">偏好</option></select></label><span v-else class="pm-kind">{{ memoryKindLabel(editorTarget.kind) }}</span><span :class="{ 'pm-too-long': draftBytes > MEMORY_CONTENT_BYTES }">{{ draftBytes }} / {{ MEMORY_CONTENT_BYTES }} 字节</span></div>
        <p v-if="editorTarget" class="pm-editor-help">{{ editorTarget.authority === 'model_candidate' ? '点击“保存并确认”后，这条提议才会加入项目记忆。' : '编辑会保留变更记录，后续对话使用新内容。' }}</p>
        <p v-if="editorError" class="corpus-error" role="alert">{{ editorError }}</p>
        <div class="pm-modal-actions"><button type="button" class="button-quiet" :disabled="mutationBusy" @click="editorOpen = false">取消</button><button type="submit" class="button-primary" :disabled="mutationBusy || !draft.trim() || draftBytes > MEMORY_CONTENT_BYTES">{{ mutationBusy ? '保存中…' : editorTarget?.authority === 'model_candidate' ? '保存并确认' : '保存记忆' }}</button></div>
      </form>
    </n-modal>
    <n-modal :show="!!forgetTarget" preset="dialog" :title="forgetTarget?.authority === 'model_candidate' ? '忽略这条提议？' : '忘记这条记忆？'" :positive-text="forgetTarget?.authority === 'model_candidate' ? '忽略提议' : '忘记记忆'" negative-text="取消" :loading="mutationBusy" :mask-closable="!mutationBusy" :closable="!mutationBusy" @positive-click="forgetMemory" @negative-click="forgetTarget = null" @close="forgetTarget = null"><p>{{ forgetTarget?.authority === 'model_candidate' ? '此提议不会加入项目记忆，操作记录仍会保留。' : '后续对话不再使用这条记忆，变更记录仍会保留。' }}</p><p v-if="forgetError" class="corpus-error" role="alert">{{ forgetError }}</p></n-modal>

    <n-modal :show="previewOpen" preset="card" title="检查记忆效果" class="pm-modal" style="width: 700px; max-width: calc(100vw - 32px)" @close="previewOpen = false" @update:show="value => { previewOpen = value; }">
      <div class="pm-preview">
        <p>试着输入一个问题，看看 <strong>{{ projectName }}</strong> 的哪些记忆会被选中。此操作不调用模型。</p>
        <p v-if="!memoryEnabled" class="pm-state-note">自动使用记忆已关闭；这里仍可检查匹配结果，实际对话不会自动使用。</p>
        <form class="pm-preview-query" @submit.prevent="rebuild"><input v-model="query" aria-label="记忆检查问题" placeholder="例如：这个项目的界面文案用什么语言？" :disabled="previewBusy" /><button type="submit" class="button-primary" :disabled="previewBusy || !query.trim()">{{ previewBusy ? '检查中…' : '检查匹配' }}</button></form>
        <p v-if="previewError" class="corpus-error" role="alert">{{ previewError }}</p>
        <template v-if="pack"><div class="pm-preview-result"><strong>{{ pack.items.length ? `选中 ${pack.items.length} 条记忆` : '没有选中记忆' }}</strong><span>仅预览，不改变已保存内容</span></div>
          <ol v-if="pack.items.length" class="pm-preview-items"><li v-for="item in pack.items" :key="item.sourceEventId"><p>{{ item.content }}</p><button type="button" class="pm-text-button" @click="locateMemory(item.sourceEventId)">查看这条记忆</button><details v-if="item.whySelected?.length"><summary>查看选取依据</summary><ul><li v-for="reason in item.whySelected" :key="reason">{{ reason }}</li></ul></details></li></ol>
          <p v-else class="pm-preview-help">尝试使用记忆里的具体名称；也请确认相关提议已确认、所属项目正确。</p>
          <details class="pm-preview-diagnostics"><summary>诊断信息</summary><p>使用 {{ pack.usedTokens }} / {{ pack.tokenBudget }} tokens；最多选择 {{ previewMaxItems }} 条。预算沿用当前对话配置。</p></details>
        </template>
      </div>
    </n-modal>
    <n-modal :show="diagnosticsOpen" preset="card" title="记忆诊断" class="pm-modal" style="width: 600px; max-width: calc(100vw - 32px)" @close="diagnosticsOpen = false" @update:show="value => { diagnosticsOpen = value; }">
      <div class="pm-diagnostics"><p>仅在排查记忆使用问题时查看。下列统计不会修改记忆或调用模型。</p><div class="pm-actions"><button type="button" class="button-quiet" :disabled="diagnosticsBusy" @click="loadGraph">查看项目关系</button><button type="button" class="button-quiet" :disabled="diagnosticsBusy" @click="loadMetrics">查看应用运行统计</button></div><p v-if="diagnosticsError" class="corpus-error" role="alert">{{ diagnosticsError }}</p><p v-if="graph">当前项目：{{ graph.nodes.length }} 个实体，{{ graph.edges.length }} 条关系。</p><p v-if="metrics">应用整体预检：{{ metrics.total }} 次，其中 {{ metrics.degraded }} 次降级，平均 {{ metrics.averageLatencyMs }} ms。</p></div>
    </n-modal>
  </section>
</template>

<script setup lang="ts">
import { NModal } from 'naive-ui';
import { computed, ref, watch } from 'vue';
import { useSessionStore } from '../stores/session';
import { createSpaceGate, runSpaceRead } from '../lib/space-gate';
import { projectMemoryList, projectMemoryAdd, projectMemoryRevise, projectMemoryConfirm, projectMemoryForget, projectMemoryPreview, projectMemoryGraph, projectMemoryMetrics } from '../lib/ipc';
import { MEMORY_CONTENT_BYTES, memoryBytes, memoryText, memoryKindLabel, projectMemoryView, filterMemoryEvents, resolveMemorySelection, memorySourceLabel, memorySourceSession, matchesMemoryKind, type ProjectMemoryEvent, type MemoryPreviewPack, type MemoryView } from '../lib/memory-view';

const store = useSessionStore();
const projectPath = ref('');
const projectOptions = computed(() => {
  const rows = store.projects.map(project => ({ name: project.name, workspace: project.workspace }));
  const current = store.activeWorkspace;
  if (current && !rows.some(row => row.workspace === current)) rows.unshift({ name: `${current.split(/[\\/]/).pop()}（当前对话）`, workspace: current });
  return rows;
});
const projectName = computed(() => projectOptions.value.find(p => p.workspace === projectPath.value)?.name || '当前项目');
const memoryEnabled = computed(() => store.settings?.values['dandelion.contextPack.enabled'] !== false);
const previewTokenBudget = computed(() => Number(store.settings?.values['dandelion.contextPack.tokenBudget'] || 1200));
const previewMaxItems = computed(() => Number(store.settings?.values['dandelion.contextPack.maxItems'] || 12));
const events = ref<ProjectMemoryEvent[]>([]);
const projection = computed(() => projectMemoryView(events.value));
const view = ref<MemoryView>('confirmed'), filter = ref(''), kindFilter = ref('all'), selectedId = ref('');
const views = computed(() => [
  { id: 'confirmed' as const, label: '已确认', count: projection.value.confirmed.length },
  { id: 'pending' as const, label: '待确认', count: projection.value.pending.length },
  { id: 'history' as const, label: '变更记录', count: projection.value.history.length },
]);
const rows = computed(() => filterMemoryEvents(view.value === 'pending' ? projection.value.pending : projection.value.confirmed, filter.value, kindFilter.value));
const historyRows = computed(() => projection.value.history.filter(row => matchesMemoryKind(row.kind, kindFilter.value) && (!filter.value.trim() || `${row.content} ${row.action}`.toLocaleLowerCase().includes(filter.value.trim().toLocaleLowerCase()))));
const selected = computed(() => rows.value.find(event => event.id === selectedId.value));
const sourceSessionId = computed(() => selected.value ? memorySourceSession(selected.value, events.value) : null);
const emptyTitle = computed(() => !projectPath.value ? '先选择一个项目' : filter.value || kindFilter.value !== 'all' ? '没有匹配的记忆' : view.value === 'pending' ? '没有待确认的记忆' : view.value === 'history' ? '暂无变更记录' : '先记下一条项目约定');
const emptyDescription = computed(() => !projectPath.value ? '记忆按工作目录隔离，选择项目后即可查看和管理。' : filter.value || kindFilter.value !== 'all' ? '换一个关键词，或清除筛选条件。' : view.value === 'pending' ? '在对话中说“记住”，或让助手提议一条记忆，之后来这里确认。' : view.value === 'history' ? '添加、编辑和忘记记忆后，记录会出现在这里。' : '例如：界面使用简体中文，或提交前需要完成哪些验证。');
const gate = createSpaceGate();
const key = (path = projectPath.value) => `memory:${path}`;
const loading = ref(false), mutationBusy = ref(false), error = ref(''), notice = ref('');
const editorOpen = ref(false), editorTarget = ref<ProjectMemoryEvent | null>(null), draft = ref(''), draftKind = ref<'fact'|'preference'>('fact'), editorError = ref('');
const draftBytes = computed(() => memoryBytes(draft.value));
const editorTitle = computed(() => !editorTarget.value ? '添加项目记忆' : editorTarget.value.authority === 'model_candidate' ? '编辑后确认' : '编辑记忆');
const forgetTarget = ref<ProjectMemoryEvent | null>(null), forgetError = ref('');
watch(forgetTarget, () => { forgetError.value = ''; });
const previewOpen = ref(false), query = ref(''), pack = ref<MemoryPreviewPack | null>(null), previewBusy = ref(false), previewError = ref('');
const moreMenu = ref<HTMLDetailsElement | null>(null), diagnosticsOpen = ref(false), diagnosticsBusy = ref(false), diagnosticsError = ref('');
const graph = ref<Awaited<ReturnType<typeof projectMemoryGraph>> | null>(null), metrics = ref<Awaited<ReturnType<typeof projectMemoryMetrics>> | null>(null);
function formatTime(value: string) { const date = new Date(value); return Number.isNaN(date.getTime()) ? value : date.toLocaleString('zh-CN', { hour12: false }); }
function formatDate(value: string) { const date = new Date(value); return Number.isNaN(date.getTime()) ? value : date.toLocaleDateString('zh-CN', { month: '2-digit', day: '2-digit' }); }
function clearFilters() { filter.value = ''; kindFilter.value = 'all'; }
function closeMore() { if (moreMenu.value) moreMenu.value.open = false; }
function openSettings(section: string) { closeMore(); void store.openSettings(section); }
function openDiagnostics() { closeMore(); diagnosticsOpen.value = true; }
function openPreview() { previewError.value = ''; previewOpen.value = true; }
function openEditor(event?: ProjectMemoryEvent) {
  editorTarget.value = event || null; draft.value = event ? memoryText(event) : ''; draftKind.value = event?.kind === 'preference' ? 'preference' : 'fact'; editorError.value = ''; editorOpen.value = true;
}
async function refresh() {
  const target = projectPath.value;
  if (!target) return;
  loading.value = true; error.value = '';
  await runSpaceRead(gate, key(target), 'list', {
    read: () => projectMemoryList(target),
    write: result => { events.value = result; loading.value = false; },
    writeError: err => { error.value = `记忆列表读取失败：${String(err)}`; loading.value = false; },
  });
}
async function mutate(action: (project: string) => Promise<ProjectMemoryEvent>, message: string, onSuccess?: (event: ProjectMemoryEvent) => void) {
  if (!projectPath.value || mutationBusy.value) return;
  const target = projectPath.value, identity = gate.claim(key(target), 'mutation');
  gate.claim(key(target), 'list'); // Invalidate a pre-mutation snapshot.
  gate.claim(key(target), 'preview'); previewBusy.value = false;
  loading.value = false; mutationBusy.value = true; error.value = ''; editorError.value = ''; notice.value = '';
  try {
    const result = await action(target);
    if (!gate.isCurrent(identity)) return;
    events.value = [...events.value.filter(event => event.id !== result.id), result];
    pack.value = null; notice.value = message;
    onSuccess?.(result);
    await refresh();
  } catch (err) {
    if (gate.isCurrent(identity)) {
      if (editorOpen.value) editorError.value = String(err);
      else if (forgetTarget.value) forgetError.value = String(err);
      else error.value = String(err);
    }
  } finally { if (gate.isCurrent(identity)) mutationBusy.value = false; }
}
async function saveEditor() {
  if (!draft.value.trim() || draftBytes.value > MEMORY_CONTENT_BYTES) return;
  const text = draft.value.trim(), target = editorTarget.value, kind = draftKind.value;
  await mutate(project => target ? projectMemoryRevise(project, target.id, text) : projectMemoryAdd(project, text, kind), target?.authority === 'model_candidate' ? '已修改并确认，后续对话可按相关性选取。' : '记忆已保存到当前项目。', result => {
    editorOpen.value = false; view.value = 'confirmed'; clearFilters(); selectedId.value = result.id;
  });
}
async function confirmMemory(event: ProjectMemoryEvent) {
  await mutate(project => projectMemoryConfirm(project, event.id), '已确认这条记忆。', result => { view.value = 'confirmed'; clearFilters(); selectedId.value = result.id; });
}
async function forgetMemory() {
  const target = forgetTarget.value;
  if (!target || mutationBusy.value) return false;
  await mutate(project => projectMemoryForget(project, target.id), target.authority === 'model_candidate' ? '已忽略提议，未加入项目记忆。' : '已忘记这条记忆，后续对话不再使用。', () => { forgetTarget.value = null; });
  return false; // Keep the dialog visible if core rejected the action.
}
async function rebuild() {
  if (!projectPath.value || !query.value.trim() || previewBusy.value) return;
  const target = projectPath.value, question = query.value.trim();
  const sessionId = store.activeWorkspace === target ? store.activeId || 'memory-preview' : 'memory-preview';
  previewBusy.value = true; previewError.value = ''; pack.value = null;
  await runSpaceRead(gate, key(target), 'preview', {
    read: () => projectMemoryPreview(target, question, sessionId, previewTokenBudget.value, previewMaxItems.value),
    write: result => { pack.value = result; previewBusy.value = false; },
    writeError: err => { previewError.value = String(err); previewBusy.value = false; },
  });
}
function locateMemory(id: string) {
  if (!projection.value.confirmed.some(event => event.id === id)) { previewError.value = '这条记忆的状态已变化，请刷新列表后重试。'; return; }
  clearFilters(); view.value = 'confirmed'; selectedId.value = id; previewOpen.value = false;
}
async function loadGraph() {
  if (!projectPath.value || diagnosticsBusy.value) return;
  diagnosticsBusy.value = true; diagnosticsError.value = '';
  const target = projectPath.value;
  await runSpaceRead(gate, key(target), 'diagnostics', { read: () => projectMemoryGraph(target), write: result => { graph.value = result; diagnosticsBusy.value = false; }, writeError: err => { diagnosticsError.value = String(err); diagnosticsBusy.value = false; } });
}
async function loadMetrics() {
  if (!projectPath.value || diagnosticsBusy.value) return;
  diagnosticsBusy.value = true; diagnosticsError.value = '';
  await runSpaceRead(gate, key(), 'diagnostics', { read: () => projectMemoryMetrics(), write: result => { metrics.value = result; diagnosticsBusy.value = false; }, writeError: err => { diagnosticsError.value = String(err); diagnosticsBusy.value = false; } });
}
watch(projectOptions, options => {
  if (!options.some(option => option.workspace === projectPath.value)) projectPath.value = options.find(option => option.workspace === store.activeWorkspace)?.workspace || options[0]?.workspace || '';
}, { immediate: true });
watch(projectPath, () => {
  gate.bind(key()); events.value = []; pack.value = null; graph.value = null; metrics.value = null;
  editorOpen.value = false; editorTarget.value = null; forgetTarget.value = null; previewOpen.value = false; diagnosticsOpen.value = false;
  mutationBusy.value = false; loading.value = false; previewBusy.value = false; diagnosticsBusy.value = false;
  error.value = ''; notice.value = ''; query.value = ''; previewError.value = ''; diagnosticsError.value = ''; selectedId.value = ''; clearFilters(); closeMore();
  void refresh();
}, { immediate: true, flush: 'sync' });
watch(rows, value => { selectedId.value = resolveMemorySelection(value, selectedId.value); }, { immediate: true });
watch(query, () => { gate.claim(key(), 'preview'); pack.value = null; previewError.value = ''; previewBusy.value = false; }, { flush: 'sync' });
</script>

<style scoped>
.project-memory { flex: 1; box-sizing: border-box; overflow: auto; padding: 24px clamp(24px, 5vw, 80px) 32px; color: var(--text); }
.pm-header { display: flex; align-items: center; justify-content: space-between; gap: 24px; }
.pm-header h1 { font-size: 26px; letter-spacing: -.03em; margin: 0 0 8px; font-weight: 650; }
.pm-header p { margin: 0; font-size: var(--fs-sm); color: var(--muted); line-height: 1.65; }
.pm-actions { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
.pm-project { display: flex; align-items: center; gap: 14px; padding: 16px 0; font-size: var(--fs-xs); }
.pm-project label { display: flex; align-items: center; gap: 10px; color: var(--muted); flex-shrink: 0; }
.pm-project select { width: 195px; }
.pm-path { min-width: 0; max-width: 440px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--muted); }
.pm-use-state { display: flex; align-items: center; gap: 7px; margin-left: auto; white-space: nowrap; color: var(--muted); }
.pm-use-state i { width: 5px; height: 5px; border-radius: 50%; background: currentColor; }.pm-use-state[data-enabled="true"] i { background: var(--ok, #287b5b); }
.pm-state-note, .pm-notice { margin: 0 0 14px; padding: 10px 12px; border-radius: 6px; background: var(--bg-2); font-size: var(--fs-xs); line-height: 1.7; }.pm-notice { color: var(--accent); }
.pm-state-note button, .pm-footer button { border: 0; padding: 0; background: transparent; color: var(--accent); font: inherit; cursor: pointer; }
.pm-tabs { display: flex; gap: 26px; border-bottom: 1px solid var(--border); }
.pm-tabs button { display: flex; align-items: center; gap: 8px; border: 0; border-bottom: 2px solid transparent; background: transparent; color: var(--muted); padding: 13px 0; font: inherit; font-size: var(--fs-sm); cursor: pointer; }
.pm-tabs button.active { color: var(--accent); border-bottom-color: var(--accent); font-weight: 600; }.pm-tabs span { min-width: 15px; font-size: 11px; font-weight: 500; text-align: center; color: var(--muted); }.pm-tabs span.pm-pending-count { padding: 1px 5px; border-radius: 4px; background: color-mix(in srgb, var(--warn, #946b26) 10%, transparent); color: var(--warn, #946b26); }
.pm-toolbar { display: flex; align-items: center; gap: 12px; padding: 16px 0; }
.pm-search { display: flex; align-items: center; gap: 8px; width: min(380px, 55%); padding: 0 10px; border: 1px solid var(--border); border-radius: 7px; color: var(--muted); background: var(--panel); }
.pm-search input { width: 100%; min-width: 0; padding: 9px 0; border: 0; background: transparent; color: var(--text); font: inherit; font-size: var(--fs-xs); outline: none; }
.pm-refresh { margin-left: auto; border: 0; background: transparent; color: var(--muted); padding: 8px; font: inherit; font-size: var(--fs-xs); cursor: pointer; }
.pm-tab-note { margin: 0 0 14px; font-size: var(--fs-xs); color: var(--muted); line-height: 1.7; }
.pm-workspace { display: grid; grid-template-columns: minmax(270px, 1.15fr) minmax(300px, 1fr); min-height: 400px; align-items: stretch; border: 1px solid var(--border); border-radius: 10px; overflow: hidden; background: var(--panel); }
.pm-list { display: flex; flex-direction: column; border-right: 1px solid var(--border); }
.pm-row { display: grid; gap: 10px; text-align: left; border: 0; border-bottom: 1px solid var(--border); background: transparent; color: var(--text); padding: 18px 20px; cursor: pointer; border-left: 2px solid transparent; font: inherit; }
.pm-row:last-child { border-bottom: 0; }.pm-row:hover { background: var(--hover-faint); }.pm-row.selected { border-left-color: var(--accent); background: color-mix(in srgb, var(--accent) 6%, var(--panel)); }
.pm-row-heading { display: flex; align-items: center; justify-content: space-between; gap: 10px; }.pm-row time, .pm-row-origin { color: var(--muted); font-size: var(--fs-xs); }
.pm-kind { display: inline-block; width: fit-content; color: var(--muted); font-size: 11px; line-height: 1.5; border-radius: 4px; padding: 2px 6px; background: var(--bg-2); }
.pm-row-content { display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; overflow-wrap: anywhere; font-size: var(--fs-sm); line-height: 1.75; }
.pm-detail { padding: 22px 26px; min-width: 0; }
.pm-detail-heading { display: flex; align-items: center; justify-content: space-between; gap: 10px; font-size: var(--fs-xs); color: var(--muted); }
.pm-status { color: var(--accent); }.pm-status.pending { color: var(--warn, #946b26); }
.pm-detail-content { font-size: 17px; font-weight: 500; line-height: 1.85; white-space: pre-wrap; overflow-wrap: anywhere; margin: 20px 0; max-width: 65ch; }
.pm-detail-meta { display: grid; grid-template-columns: 72px minmax(0,1fr); gap: 12px 12px; font-size: var(--fs-xs); margin: 0; line-height: 1.65; }
.pm-detail-meta dt { color: var(--muted); }.pm-detail-meta dd { margin: 0; overflow-wrap: anywhere; }.pm-detail-meta small { display: block; color: var(--muted); margin-top: 3px; font-size: inherit; }
.pm-text-button { padding: 0; border: 0; background: transparent; color: var(--accent); cursor: pointer; font: inherit; }.pm-detail-meta .pm-text-button { display: block; margin-top: 4px; }
.pm-detail-note { border-top: 1px solid var(--border); padding-top: 17px; margin: 20px 0 16px; font-size: var(--fs-xs); line-height: 1.75; color: var(--muted); }
.pm-detail-actions { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; }
.pm-subtle { border: 0; padding: 8px; background: transparent; font: inherit; font-size: var(--fs-xs); color: var(--muted); cursor: pointer; }.pm-subtle:hover { color: var(--danger); }
.pm-empty { min-height: 330px; box-sizing: border-box; padding: 60px 24px; display: flex; flex-direction: column; align-items: center; justify-content: center; text-align: center; border: 1px dashed var(--border); border-radius: 10px; color: var(--muted); }
.pm-empty svg { margin-bottom: 19px; color: var(--accent); opacity: .75; }.pm-empty h2 { font-size: 18px; color: var(--text); font-weight: 500; margin: 0 0 10px; }.pm-empty p { max-width: 420px; margin: 0 0 22px; font-size: var(--fs-sm); line-height: 1.8; }
.pm-history { border: 1px solid var(--border); border-radius: 10px; overflow: hidden; background: var(--panel); }
.pm-history-row { display: grid; grid-template-columns: 8px minmax(0,1fr); gap: 16px; padding: 19px 22px; border-top: 1px solid var(--border); }.pm-history-row:first-child { border-top: 0; }.pm-history-row p { white-space: pre-wrap; overflow-wrap: anywhere; margin: 10px 0 0; font-size: var(--fs-sm); line-height: 1.75; }
.pm-history-mark { width: 6px; height: 6px; border-radius: 50%; background: var(--accent); margin-top: 8px; }.pm-history-mark[data-action="retraction"] { background: var(--muted); }
.pm-history-heading { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; font-size: var(--fs-xs); }.pm-history-heading strong { font-weight: 500; }.pm-history-heading time { margin-left: auto; color: var(--muted); }.pm-history-current { color: var(--accent); }
.pm-footer { display: flex; align-items: baseline; justify-content: space-between; gap: 14px; margin-top: 20px; color: var(--muted); font-size: var(--fs-xs); line-height: 1.7; }
.pm-more { position: relative; font-size: var(--fs-xs); }.pm-more summary { padding: 9px; color: var(--muted); cursor: pointer; list-style: none; }.pm-more summary::-webkit-details-marker { display: none; }.pm-more > div { position: absolute; right: 0; top: calc(100% + 6px); z-index: 5; min-width: 175px; display: grid; padding: 5px; border: 1px solid var(--border); border-radius: 8px; background: var(--panel); box-shadow: var(--shadow-menu); }.pm-more button { border: 0; padding: 10px; background: transparent; color: var(--text); text-align: left; font: inherit; cursor: pointer; }.pm-more button:hover { background: var(--hover); }
.pm-project select, .pm-toolbar > select { border: 1px solid var(--border); border-radius: 7px; padding: 8px 10px; color: var(--text); background: var(--panel); font: inherit; font-size: var(--fs-xs); }
.project-memory button:disabled { cursor: default; opacity: .5; }.project-memory button:focus-visible, .project-memory summary:focus-visible, .project-memory select:focus-visible, .pm-search:focus-within { outline: 2px solid var(--accent); outline-offset: 3px; }
.pm-editor { display: grid; gap: 16px; }.pm-editor > p, .pm-preview > p, .pm-diagnostics > p { margin: 0; color: var(--muted); font-size: var(--fs-sm); line-height: 1.8; }.pm-editor > label { display: grid; gap: 9px; font-size: var(--fs-xs); }
.pm-editor textarea, .pm-editor select, .pm-preview-query input { box-sizing: border-box; width: 100%; border: 1px solid var(--border); border-radius: 7px; padding: 10px 12px; background: var(--bg); color: var(--text); font: inherit; font-size: var(--fs-sm); line-height: 1.7; }.pm-editor textarea { resize: vertical; }.pm-editor-meta { display: flex; align-items: center; justify-content: space-between; gap: 12px; color: var(--muted); font-size: var(--fs-xs); }.pm-editor-meta label { display: flex; align-items: center; gap: 9px; }.pm-editor select { width: auto; padding: 5px 8px; }.pm-editor-meta .pm-too-long { color: var(--danger); }.pm-editor .pm-editor-help { font-size: var(--fs-xs); }
.pm-modal-actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 4px; }
.pm-preview, .pm-diagnostics { display: grid; gap: 16px; }.pm-preview-query { display: flex; gap: 8px; }.pm-preview-query input { min-width: 0; flex: 1; }.pm-preview-result { display: flex; align-items: baseline; justify-content: space-between; gap: 10px; font-size: var(--fs-sm); padding-top: 16px; border-top: 1px solid var(--border); }.pm-preview-result span { font-size: var(--fs-xs); color: var(--muted); }.pm-preview-items { list-style: none; padding: 0; margin: 0; max-height: 40vh; overflow: auto; }.pm-preview-items > li { border-top: 1px solid var(--border); padding: 14px 0; }.pm-preview-items p { margin: 0 0 9px; font-size: var(--fs-sm); line-height: 1.8; white-space: pre-wrap; overflow-wrap: anywhere; }.pm-preview-items .pm-text-button { font-size: var(--fs-xs); }.pm-preview-items details { margin-top: 10px; color: var(--muted); font-size: var(--fs-xs); line-height: 1.7; }.pm-preview-items summary, .pm-preview-diagnostics summary { cursor: pointer; }.pm-preview-diagnostics, .pm-preview .pm-preview-help { color: var(--muted); font-size: var(--fs-xs); line-height: 1.8; }
.pm-editor :is(input, textarea, select, button):focus-visible, .pm-preview :is(input, button, summary):focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
@media (max-width: 900px) { .pm-header { align-items: flex-start; flex-direction: column; gap: 16px; }.pm-path { display: none; }.pm-project { gap: 10px; }.pm-workspace { grid-template-columns: minmax(230px,1fr) minmax(260px,1fr); }.pm-detail { padding: 20px; } }
@media (max-width: 650px) { .project-memory { padding: 24px 18px 32px; }.pm-project { flex-wrap: wrap; }.pm-use-state { margin-left: 0; flex-basis: 100%; }.pm-workspace { grid-template-columns: 1fr; }.pm-list { border-right: 0; border-bottom: 1px solid var(--border); max-height: 300px; overflow: auto; }.pm-detail { padding: 22px; }.pm-tabs { gap: 20px; }.pm-toolbar { flex-wrap: wrap; }.pm-search { flex: 1; min-width: 150px; }.pm-footer { flex-direction: column; gap: 5px; }.pm-preview-query, .pm-preview-result { flex-direction: column; align-items: stretch; }.pm-preview-query button { align-self: flex-end; }.pm-history-heading time { margin-left: 0; flex-basis: 100%; } }
</style>
