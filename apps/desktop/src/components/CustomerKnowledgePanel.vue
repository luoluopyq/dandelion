<template>
  <section class="knowledge-page" :class="{ embedded }" :aria-busy="busy">
    <header class="materials-header">
      <div><h2>本地资料 <span class="file-count">{{ documents.length }}</span></h2><p>集中保存参考资料，在主对话中使用。</p></div>
      <div class="button-row">
        <button type="button" class="button-quiet" :disabled="busy" @click="reload">刷新</button>
        <details ref="addMenu" class="add-menu"><summary :aria-disabled="busy">＋ 添加资料</summary><div>
          <button type="button" :disabled="busy || !ready" @click="importFiles">选择文件</button>
          <button type="button" :disabled="busy || !ready" @click="importFolder">选择文件夹</button>
          <button type="button" :disabled="busy || !ready" @click="openPaste">粘贴文本</button>
        </div></details>
      </div>
    </header>
    <p v-if="error" class="corpus-error" role="alert">{{ error }}</p>
    <p v-if="fileImportSummary" class="import-notice" role="status">{{ fileImportSummary }}</p>
    <details v-if="importFailures.length" class="import-failures"><summary>{{ importFailures.length }} 份资料导入失败，查看原因</summary><ul><li v-for="failure in importFailures" :key="failure.sourceId"><strong>{{ fileName(failure.sourceId) }}</strong><p>{{ failure.message }}</p></li></ul></details>
    <p v-if="busy" class="processing-note" role="status">正在处理，请稍候…</p>
    <template v-if="ready">
      <div class="materials-controls">
        <label class="file-filter"><span>筛选</span><input v-model="fileFilter" type="search" placeholder="按文件名查找" aria-label="按文件名筛选资料" /></label>
        <label class="enable-option"><input v-model="enableOnImport" type="checkbox" :disabled="busy" />导入后立即启用</label>
      </div>
      <p class="materials-caption">支持文本、Markdown、PDF 和 HTML。文件夹为一次性导入，源文件更新后需重新导入。</p>
      <form v-if="showPaste" class="paste-panel" @submit.prevent="importDocument">
        <label>资料名称<input v-model.trim="sourceId" placeholder="例如：产品说明" required :disabled="busy" /></label>
        <label>正文<textarea v-model="content" rows="7" placeholder="粘贴需要保存的内容" required :disabled="busy" /></label>
        <div class="button-row"><button type="submit" class="button-primary" :disabled="busy || !content.trim() || !sourceId">添加文本</button><button type="button" class="button-quiet" @click="showPaste = false">收起</button></div>
      </form>
      <div v-if="pendingVersions.length" class="pending-bar">
        <span>{{ pendingVersions.length }} 份资料尚未启用</span><button type="button" :disabled="busy" @click="publishImported">启用待处理资料</button>
      </div>
      <div v-if="documents.length" class="materials-table-wrap">
        <table class="materials-table">
          <thead><tr><th scope="col">文件名</th><th scope="col">状态</th><th scope="col">添加时间</th><th scope="col"><span class="sr-only">操作</span></th></tr></thead>
          <tbody><tr v-for="doc in filteredDocuments" :key="doc.id">
            <td><div class="file-name"><span class="file-type" aria-hidden="true">{{ fileType(doc.sourceId) }}</span><div><button type="button" class="source-open" :disabled="busy" @click="openDocument(doc)">{{ fileName(doc.sourceId) }}</button><small :title="doc.sourceId">{{ doc.sourceId }}</small></div></div></td>
            <td><span class="material-status" :data-status="doc.status">{{ doc.status === 'published' ? '可用' : '待启用' }}</span></td>
            <td class="file-date">{{ formatDate(doc.createdAt) }}</td>
            <td><button v-if="doc.status !== 'published'" type="button" class="file-remove" :disabled="busy" title="查看并启用已保存的资料版本" @click="openDocument(doc)">查看版本</button><button type="button" class="file-remove" :disabled="busy" :aria-label="`删除 ${fileName(doc.sourceId)}`" @click="deleteDocument(doc)">删除</button></td>
          </tr></tbody>
        </table>
        <p v-if="!filteredDocuments.length" class="filter-empty">没有匹配的文件，试试其他名称。</p>
      </div>
      <div v-else class="materials-empty">
        <svg width="60" height="60" viewBox="0 0 60 60" fill="none" aria-hidden="true"><path d="M10 19a4 4 0 0 1 4-4h12l5 5h15a4 4 0 0 1 4 4v21H10V19Z" fill="currentColor" opacity=".1"/><path d="M10 28V19a4 4 0 0 1 4-4h12l5 5h15a4 4 0 0 1 4 4v21H10V28Z" stroke="currentColor" stroke-width="1.5"/><path d="M24 33h12M30 27v12" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/></svg>
        <h3>把散落的资料，放在一起</h3><p>添加文档后，可在主对话中引用，<br />也可以整理成便于阅读的知识文章。</p>
        <div class="button-row"><button type="button" class="button-primary" :disabled="busy" @click="importFiles">添加第一份资料</button><button type="button" class="button-quiet" :disabled="busy" @click="importFolder">导入文件夹</button></div>
      </div>
    </template>
    <section v-else class="materials-empty"><h3>为资料留一个位置</h3><p>创建知识库，保存本地文档和整理后的文章。</p><button type="button" class="button-primary" :disabled="busy" @click="createSpace">创建知识库</button></section>
    <details class="materials-management" @toggle="showAdvanced = ($event.target as HTMLDetailsElement).open">
      <summary>资料库管理</summary>
      <div class="management-actions"><button type="button" class="button-quiet" :disabled="busy" @click="createSpace">新建知识库</button><button type="button" class="button-quiet" :disabled="busy || !ready" @click="exportKnowledge">导出资料</button><button type="button" class="button-quiet" :disabled="busy || !ready" @click="createCollection">新建分组</button></div>
      <label v-if="!embedded && spaces.length" class="standalone-scope">当前知识库<select v-model="spaceId" :disabled="busy" @change="changeSpace"><option v-for="item in spaces" :key="item.id" :value="item.id">{{ item.name }}</option></select></label>
      <template v-if="ready">
          <section v-if="showAdvanced" class="knowledge-card import-card">
            <div><h2>增量同步来源</h2><p>先预览内容变化，再按来源 cursor 应用；重复内容不会新增版本或分块。</p></div>
            <label>同步 cursor（可选）<input v-model.trim="syncCursor" placeholder="例如远端版本号或时间戳" /></label>
            <div class="button-row"><button type="button" class="button-quiet" :disabled="busy || !content.trim() || !sourceId" @click="previewSync">预览差异</button><button type="button" class="button-primary" :disabled="busy || !content.trim() || !sourceId" @click="applySync">应用同步</button></div>
            <p v-if="syncPreview" class="knowledge-policy">{{ syncPreview.changed ? '检测到变更：将创建新版本。' : '内容未变化：不会生成重复版本。' }} {{ syncPreview.incomingSha256 }}</p>
            <ul v-if="syncSources.length" class="knowledge-documents"><li v-for="item in syncSources" :key="item.id"><div><strong>{{ item.sourceKind }} · {{ item.sourceId }}</strong><small>{{ item.status }} · 重试 {{ item.retryCount }} 次 · {{ item.cursor || '无 cursor' }}</small></div><button type="button" class="button-quiet" :disabled="busy" @click="toggleSync(item)">{{ item.status === 'paused' ? '恢复' : '暂停' }}</button></li></ul>
          </section>

          <template v-if="showAdvanced">
            <section v-if="deletions.length" class="knowledge-card"><div class="knowledge-section-title"><div><h2>删除证明</h2><p>仅所有者可查看的级联清理审计。</p></div><span>{{ deletions.length }} 条</span></div><ul class="knowledge-documents"><li v-for="item in deletions" :key="item.id"><div><strong>{{ item.documentId }}</strong><small>{{ item.deletedAt }} · 删除 {{ item.cascadeVersions }} 个版本、{{ item.cascadeChunks }} 个分块</small></div></li></ul></section>
            <section class="knowledge-card retention-card"><div class="knowledge-section-title"><div><h2>保留策略</h2><p>只计算已撤回或已删除的旧版本；设置策略不会静默删除资料。</p></div></div><div class="button-row"><label>保留天数<input v-model.number="retentionDays" type="number" min="1" max="3650" /></label><button type="button" class="button-quiet" :disabled="busy" @click="saveRetention">保存策略</button></div><p v-if="retention" class="knowledge-policy">当前保留 {{ retention.retentionDays }} 天 · 可清理 {{ retention.eligibleVersions }} 个版本{{ retention.oldestEligibleAt ? ` · 最早 ${retention.oldestEligibleAt}` : '' }}</p><p v-else class="knowledge-empty">尚未设置保留策略。</p></section>
            <section class="knowledge-card"><div class="knowledge-section-title"><div><h2>成员与受限 API</h2><p>仅所有者可管理成员或创建 token；token 只显示一次，系统只保存其哈希。</p></div></div><div class="button-row"><button type="button" class="button-quiet" :disabled="busy" @click="manageMember">添加或更新成员</button><button type="button" class="button-quiet" :disabled="busy" @click="createApiClient">创建嵌入式问答 token</button></div><p v-if="apiToken" class="knowledge-policy">仅此一次显示，请立即保存：{{ apiToken }}</p><ul v-if="members.length" class="knowledge-documents"><li v-for="member in members" :key="member.subjectId"><div><strong>{{ member.subjectId }}</strong><small>{{ member.role }} · {{ member.createdAt }}</small></div><button type="button" class="button-danger" :disabled="busy || member.subjectId === subjectId" @click="removeMember(member)">移除</button></li></ul></section>
            <section class="knowledge-card"><div class="knowledge-section-title"><div><h2>质量与反馈审核</h2><p>质量数据仅聚合当前空间的受限 API、同步和反馈记录。</p></div><button type="button" class="button-quiet" :disabled="busy" @click="loadGovernance">刷新</button></div><p v-if="quality" class="knowledge-policy">已发布 {{ quality.publishedDocuments }} 文档 / {{ quality.publishedChunks }} 分块 · 引用覆盖 {{ Math.round(quality.citationCoverage * 100) }}% · 无答案 {{ Math.round(quality.noAnswerRate * 100) }}% · 待审核反馈 {{ quality.openFeedback }} · 同步失败 {{ quality.failedSyncSources }}</p><div class="button-row"><button type="button" class="button-quiet" :disabled="busy" @click="submitFeedback('helpful')">反馈：有帮助</button><button type="button" class="button-quiet" :disabled="busy" @click="submitFeedback('incorrect')">反馈：不正确</button><button type="button" class="button-quiet" :disabled="busy" @click="submitFeedback('no_answer')">反馈：未解决</button></div><ul v-if="feedback.length" class="knowledge-documents"><li v-for="item in feedback" :key="item.id"><div><strong>{{ item.kind }} · {{ item.status }}</strong><small>{{ item.comment || '无说明' }} · {{ item.createdAt }}</small></div><div v-if="item.status === 'open'" class="button-row"><button type="button" class="button-quiet" :disabled="busy" @click="reviewFeedback(item, 'accepted')">采纳</button><button type="button" class="button-quiet" :disabled="busy" @click="reviewFeedback(item, 'dismissed')">驳回</button></div></li></ul></section>
          </template>
      </template>
    </details>
    <n-modal :show="!!previewDocumentId" preset="card" title="本地资料" style="width: 800px; max-width: calc(100vw - 32px)" :auto-focus="false" @close="previewDocumentId = ''" @update:show="value => { if (!value) previewDocumentId = ''; }">
      <p v-if="previewLoading" role="status">正在读取原文…</p>
      <p v-else-if="previewError" class="corpus-error" role="alert">{{ previewError }}</p>
      <template v-else-if="previewDocument">
        <div class="preview-heading"><strong>{{ fileName(previewDocument.sourceId) }}</strong><span>版本 {{ previewDocument.versionNo }} · {{ previewDocument.enabled ? '已启用' : '未启用' }}</span></div>
        <p class="preview-path">{{ previewDocument.sourceId }}</p>
        <pre class="document-content">{{ previewDocument.content }}</pre>
        <button v-if="!previewDocument.enabled" type="button" class="button-primary" :disabled="busy" @click="enablePreview">启用此版本</button>
      </template>
    </n-modal>
    <n-modal :show="dialog !== null" preset="card" :title="dialogTitle" class="knowledge-modal" style="width: 480px; max-width: calc(100vw - 40px)" :mask-closable="false" @close="closeDialog">
      <p v-if="dialog === 'space'" class="dialog-description">给本地知识库起一个名称，随后就可以添加资料。</p>
      <template v-if="dialog === 'space'"><label>资料库名称<input v-model.trim="dialogName" autofocus placeholder="例如：产品资料" /></label><label>数据使用方式<select v-model="dialogPolicy"><option value="local_only">仅在本机使用（推荐）</option><option value="allow_summary">允许发送摘要</option><option value="deny_cloud">禁止云端</option></select></label></template>
      <template v-else-if="dialog === 'collection'"><label>集合名称<input v-model.trim="dialogName" autofocus placeholder="例如：上手指南" /></label></template>
      <template v-else-if="dialog === 'assign'"><label>归入集合<select v-model="dialogCollectionId"><option value="">选择集合</option><option v-for="item in collections" :key="item.id" :value="item.id">{{ item.name }}</option></select></label></template>
      <template v-else-if="dialog === 'member'"><label>成员身份 ID<input v-model.trim="dialogName" autofocus placeholder="例如：alex" /></label><label>成员角色<select v-model="dialogRole"><option value="owner">所有者</option><option value="editor">编辑者</option><option value="reader">阅读者</option></select></label></template>
      <template v-else-if="dialog === 'api'"><label>嵌入组件名称<input v-model.trim="dialogName" autofocus placeholder="例如：support-widget" /></label></template>
      <template v-else><label>说明（可选）<textarea v-model.trim="dialogNote" rows="4" :placeholder="dialog === 'feedback' ? '告诉我们哪里有帮助或需要改进' : '添加审核说明'" /></label></template>
      <template #footer><div class="modal-actions"><button type="button" class="button-quiet" @click="closeDialog">取消</button><button type="button" class="button-primary" :disabled="busy || (dialogNeedsName && !dialogName) || (dialog === 'assign' && !dialogCollectionId)" @click="submitDialog">确定</button></div></template>
    </n-modal>
    <n-modal :show="confirmDialog !== null" preset="dialog" :title="confirmDialog?.title" :content="confirmDialog?.content" positive-text="确认" negative-text="取消" @positive-click="confirmPending" @negative-click="confirmDialog = null" @close="confirmDialog = null" />
  </section>
</template>

<script setup lang="ts">
import { NModal } from "naive-ui";
import { computed, onMounted, ref, watch } from "vue";
import {
  spaceDocumentPreview,
  knowledgeAssignCollection,
  knowledgeCollections,
  knowledgeCreateApiClient,
  knowledgeCreateCollection,
  knowledgeCreateLocalSpace,
  knowledgeDeleteDocument,
  knowledgeDeletionAudits,
  knowledgeDocuments,
  knowledgeExportSave,
  knowledgeExternalDecision,
  knowledgeFeedbackList,
  knowledgeImportDocument,
  knowledgeImportPick,
  knowledgeLocalSpaces,
  knowledgeManageMember,
  knowledgeMembers,
  knowledgePublishVersion,
  knowledgeQualityMetrics,
  knowledgeRemoveMember,
  knowledgeReviewFeedback,
  knowledgeSetRetentionDays,
  knowledgeSubmitFeedback,
  knowledgeSyncApply,
  knowledgeSyncPreview,
  knowledgeSyncSetPaused,
  knowledgeSyncSources,
  knowledgeRetentionPreview,
} from "../lib/ipc";
import type {
  KnowledgeCollectionRow as Collection,
  KnowledgeDeletionAuditRow as Deletion,
  KnowledgeDocumentRow as Document,
  KnowledgeExternalDecision as ExternalDecision,
  KnowledgeFeedbackRow as Feedback,
  KnowledgeMemberRow as Member,
  KnowledgeQualityMetricsRow as Quality,
  KnowledgeRetentionPreviewRow as RetentionPreview,
  KnowledgeSpaceRow as Space,
  KnowledgeSyncPreview as SyncPreview,
  KnowledgeSyncSourceRow as SyncSource,
  KnowledgeVersionRow as Version,
} from "../lib/types";
import type { KnowledgeScope } from "../lib/ipc";
import { createSpaceGate, runSpaceRead, spaceTargetKey } from "../lib/space-gate";

const { embedded = false, hideSearch = false, selectedSpaceId = "" } = defineProps<{ embedded?: boolean; hideSearch?: boolean; selectedSpaceId?: string }>();
const emit = defineEmits<{ spaceChange: [id: string]; spacesLoaded: [spaces: { id: string; name: string }[]] }>();

// 资料读取走空间身份闸门：换库后旧响应不得盖到新库上。
const gate = createSpaceGate();

const busy = ref(false), error = ref(""), tenantId = ref("local"), customerId = ref("local"), spaceId = ref(""), subjectId = ref("local-user");
const spaces = ref<Space[]>([]), documents = ref<Document[]>([]), collections = ref<Collection[]>([]), deletions = ref<Deletion[]>([]);
const sourceKind = ref("file"), sourceId = ref(""), content = ref(""), lastVersion = ref<Version | null>(null), externalPolicy = ref<ExternalDecision | null>(null), syncCursor = ref(""), syncPreview = ref<SyncPreview | null>(null), syncSources = ref<SyncSource[]>([]), retention = ref<RetentionPreview | null>(null), retentionDays = ref(365), fileImportSummary = ref(""), importedVersions = ref<Version[]>([]), members = ref<Member[]>([]), feedback = ref<Feedback[]>([]), quality = ref<Quality | null>(null), apiToken = ref("");
const previewDocumentId = ref("");
const previewDocument = ref<Awaited<ReturnType<typeof spaceDocumentPreview>> | null>(null);
const previewLoading = ref(false), previewError = ref("");
async function openDocument(doc: Document) {
  previewDocumentId.value = doc.id; previewDocument.value = null; previewError.value = ""; previewLoading.value = true;
  const target = scope();
  await runSpaceRead(gate, spaceTargetKey([target.spaceId]), "document-preview", {
    read: () => spaceDocumentPreview(target.subjectId, target.spaceId, doc.id),
    write: result => { previewDocument.value = result; previewLoading.value = false; },
    writeError: err => { previewError.value = String(err); previewLoading.value = false; },
  });
}
async function enablePreview() {
  const revision = previewDocument.value, target = scope();
  if (!revision) return;
  await run(async () => {
    try { await knowledgePublishVersion(target, revision.documentId, revision.versionId); }
    catch (err) { if (spaceId.value === target.spaceId) previewError.value = String(err); throw err; }
    if (spaceId.value !== target.spaceId) return;
    if (previewDocument.value?.versionId === revision.versionId) previewDocument.value.enabled = true;
    importedVersions.value = importedVersions.value.filter(v => v.id !== revision.versionId);
    fileImportSummary.value = "资料已启用，可在对话中引用。";
    await loadDocuments();
  });
}
const ready = computed(() => Boolean(spaceId.value && subjectId.value));
const showAdvanced = ref(false);
const importFailures = ref<{ sourceId: string; message: string }[]>([]);
const addMenu = ref<HTMLDetailsElement | null>(null);
function openPaste() { if (addMenu.value) addMenu.value.open = false; showPaste.value = true; }
const showPaste = ref(false), enableOnImport = ref(true), fileFilter = ref("");
const filteredDocuments = computed(() => documents.value.filter(doc => doc.sourceId.toLocaleLowerCase().includes(fileFilter.value.toLocaleLowerCase())));
const pendingVersions = computed(() => importedVersions.value);
function fileName(path: string) { return path.replace(/\\/g, "/").split("/").pop() || path; }
function fileType(path: string) { const name = fileName(path); return name.includes(".") ? name.split(".").pop()!.slice(0, 4).toUpperCase() : "TXT"; }
function formatDate(value: string) { return new Date(value).toLocaleDateString("zh-CN", { year: "numeric", month: "2-digit", day: "2-digit" }); }

watch(() => selectedSpaceId, id => { if (id && id !== spaceId.value && spaces.value.some(space => space.id === id)) { spaceId.value = id; void changeSpace(); } });
type Dialog = "space" | "collection" | "assign" | "member" | "api" | "feedback" | "review";
const dialog = ref<Dialog | null>(null), dialogName = ref(""), dialogPolicy = ref("local_only"), dialogRole = ref("reader"), dialogCollectionId = ref(""), dialogNote = ref(""), pendingDocument = ref<Document | null>(null), pendingFeedback = ref<Feedback | null>(null), pendingFeedbackKind = ref(""), pendingReviewStatus = ref("");
const confirmDialog = ref<{ title: string; content: string; action: () => Promise<void> } | null>(null);
const dialogTitle = computed(() => ({ space: "新建资料库", collection: "新建集合", assign: "归入集合", member: "添加或更新成员", api: "创建嵌入式问答 token", feedback: "提交反馈", review: "审核反馈" }[dialog.value || "space"]));
const dialogNeedsName = computed(() => ["space", "collection", "member", "api"].includes(dialog.value || ""));
function openDialog(kind: Dialog) { dialog.value = kind; dialogName.value = ""; dialogPolicy.value = "local_only"; dialogRole.value = "reader"; dialogCollectionId.value = ""; dialogNote.value = ""; }
function closeDialog() { dialog.value = null; pendingDocument.value = null; pendingFeedback.value = null; }
const scope = (): KnowledgeScope => ({ tenantId: tenantId.value, customerId: customerId.value, spaceId: spaceId.value, subjectId: subjectId.value });

// 换读取目标即升代次：在途旧响应（含失败）全体失去回写资格。
watch(spaceId, id => gate.bind(spaceTargetKey([id])));
async function changeSpace() { closeDialog(); confirmDialog.value = null; previewDocumentId.value = ""; previewDocument.value = null; importFailures.value = []; emit("spaceChange", spaceId.value); documents.value = []; collections.value = []; lastVersion.value = null; importedVersions.value = []; externalPolicy.value = null; syncPreview.value = null; syncSources.value = []; deletions.value = []; retention.value = null; members.value = []; feedback.value = []; quality.value = null; apiToken.value = ""; fileFilter.value = ""; showPaste.value = false; fileImportSummary.value = ""; sourceId.value = ""; content.value = ""; syncCursor.value = ""; if (ready.value) await loadDocuments(); }
let busyCount = 0;
async function run(action: () => Promise<void>) {
  const target = spaceId.value;
  if (busyCount === 0) error.value = "";
  busyCount++; busy.value = true;
  try { await action(); }
  catch (e) { if (target === spaceId.value) error.value = String(e); }
  finally { busyCount--; busy.value = busyCount > 0; }
}
async function reload() { await run(async () => { spaces.value = await knowledgeLocalSpaces(); emit("spacesLoaded", spaces.value); if (!spaces.value.some(item => item.id === spaceId.value)) spaceId.value = spaces.value.find(item => item.id === selectedSpaceId)?.id || spaces.value[0]?.id || ""; emit("spaceChange", spaceId.value); if (spaceId.value) await loadDocuments(); }); }
async function loadDocuments() {
  if (!ready.value) return;
  const targetScope = scope();
  await run(() => runSpaceRead(gate, spaceTargetKey([targetScope.spaceId]), "documents", {
    read: async () => {
      const [docs, groups, sources, decision] = await Promise.all([
        knowledgeDocuments(targetScope), knowledgeCollections(targetScope),
        knowledgeSyncSources(targetScope), knowledgeExternalDecision(targetScope, "cloud_full"),
      ]);
      // 管理信息读取失败不影响普通用户查看自己的资料。
      const [audit, keep, people, reviews, metrics] = await Promise.allSettled([
        knowledgeDeletionAudits(targetScope), knowledgeRetentionPreview(targetScope),
        knowledgeMembers(targetScope), knowledgeFeedbackList(targetScope), knowledgeQualityMetrics(targetScope),
      ]);
      return { docs, groups, sources, decision,
        audit: audit.status === "fulfilled" ? audit.value : [],
        keep: keep.status === "fulfilled" ? keep.value : null,
        people: people.status === "fulfilled" ? people.value : [],
        reviews: reviews.status === "fulfilled" ? reviews.value : [],
        metrics: metrics.status === "fulfilled" ? metrics.value : null,
      };
    },
    write: result => {
      documents.value = result.docs; collections.value = result.groups;
      syncSources.value = result.sources; externalPolicy.value = result.decision;
      deletions.value = result.audit; retention.value = result.keep; members.value = result.people;
      feedback.value = result.reviews; quality.value = result.metrics;
      if (result.keep) retentionDays.value = result.keep.retentionDays;
    },
    writeError: err => { error.value = String(err); },
  }));
}
async function createSpace() { openDialog("space"); }
async function createCollection() { openDialog("collection"); }
async function enableVersions(importScope: KnowledgeScope, versions: Version[], immediately: boolean) {
  const pending: Version[] = [];
  for (const version of versions) {
    if (immediately) {
      try { await knowledgePublishVersion(importScope, version.documentId, version.id); }
      catch (err) { pending.push(version); if (spaceId.value === importScope.spaceId) error.value = `资料已保存，但启用失败：${String(err)}`; }
    } else pending.push(version);
  }
  return pending;
}
async function importDocument() {
  const importScope = scope(), immediately = enableOnImport.value;
  await run(async () => {
    const [, version] = await knowledgeImportDocument(importScope, sourceKind.value, sourceId.value, content.value, 1200);
    const pending = await enableVersions(importScope, [version], immediately);
    if (spaceId.value !== importScope.spaceId) return;
    lastVersion.value = version;
    importedVersions.value.push(...pending);
    fileImportSummary.value = pending.length ? "文本已保存，尚未启用。可点击下方按钮启用。" : "文本已添加，可在主对话中使用。";
    content.value = ""; showPaste.value = false;
    await loadDocuments();
  });
}
async function importPicked(folder: boolean) {
  const importScope = scope(), immediately = enableOnImport.value;
  await run(async () => {
    const result = await knowledgeImportPick(importScope, folder);
    if (!result) return;
    const pending = await enableVersions(importScope, result.imported, immediately);
    if (spaceId.value !== importScope.spaceId) return;
    importFailures.value = result.failures;
    importedVersions.value.push(...pending);
    lastVersion.value = result.imported[result.imported.length - 1] || lastVersion.value;
    fileImportSummary.value = `已添加 ${result.imported.length} 份资料，${result.imported.length - pending.length} 份已启用${pending.length ? `，${pending.length} 份待启用` : ""}${result.failures.length ? `；${result.failures.length} 份导入失败：${result.failures[0].message}` : ""}`;
    await loadDocuments();
  });
}
async function importFiles() { if (addMenu.value) addMenu.value.open = false; await importPicked(false); }
async function importFolder() { if (addMenu.value) addMenu.value.open = false; await importPicked(true); }
async function publishImported() {
  const importScope = scope(), versions = [...importedVersions.value];
  await run(async () => {
    const pending = await enableVersions(importScope, versions, true);
    if (spaceId.value !== importScope.spaceId) return;
    importedVersions.value = pending;
    fileImportSummary.value = pending.length ? `${pending.length} 份资料未能启用，请检查权限后重试。` : "资料已启用，可在主对话中使用。";
    await loadDocuments();
  });
}
async function previewSync() { await run(async () => { syncPreview.value = await knowledgeSyncPreview(scope(), sourceKind.value, sourceId.value, content.value); }); }
async function applySync() { await run(async () => { const [, version] = await knowledgeSyncApply(scope(), sourceKind.value, sourceId.value, content.value, syncCursor.value || null); if (version) { lastVersion.value = version; } await loadDocuments(); }); }
async function toggleSync(item: SyncSource) { await run(async () => { await knowledgeSyncSetPaused(scope(), item.sourceKind, item.sourceId, item.status !== "paused"); await loadDocuments(); }); }
async function deleteDocument(doc: Document) { confirmDialog.value = { title: "永久删除文档", content: `将删除“${doc.sourceId}”及其全部版本和分块投影，此操作不可恢复。`, action: async () => { await knowledgeDeleteDocument(scope(), doc.id); await loadDocuments(); } }; }
async function exportKnowledge() { await run(async () => { const path = await knowledgeExportSave(scope()); if (path) fileImportSummary.value = `已导出到：${path}`; }); }
async function saveRetention() { await run(async () => { await knowledgeSetRetentionDays(scope(), retentionDays.value); retention.value = await knowledgeRetentionPreview(scope()); }); }
async function manageMember() { openDialog("member"); }
async function removeMember(member: Member) { confirmDialog.value = { title: "移除成员", content: `确定移除 ${member.subjectId} 的空间访问权限？`, action: async () => { await knowledgeRemoveMember(scope(), subjectId.value, member.subjectId); await loadDocuments(); } }; }
async function createApiClient() { openDialog("api"); }
async function submitFeedback(kind: string) { pendingFeedbackKind.value = kind; openDialog("feedback"); }
async function reviewFeedback(item: Feedback, status: string) { pendingFeedback.value = item; pendingReviewStatus.value = status; openDialog("review"); }
async function submitDialog() { const kind = dialog.value; if (!kind) return; if (dialogNeedsName.value && !dialogName.value) return; await run(async () => { if (kind === "space") { const item = await knowledgeCreateLocalSpace(subjectId.value, dialogName.value, dialogPolicy.value); await reload(); spaceId.value = item.id; await changeSpace(); } else if (kind === "collection") { await knowledgeCreateCollection(scope(), dialogName.value); await loadDocuments(); } else if (kind === "assign" && pendingDocument.value && dialogCollectionId.value) { await knowledgeAssignCollection(scope(), dialogCollectionId.value, pendingDocument.value.id); } else if (kind === "member") { await knowledgeManageMember(scope(), subjectId.value, dialogName.value, dialogRole.value); await loadDocuments(); } else if (kind === "api") { const provision = await knowledgeCreateApiClient(scope(), dialogName.value, 60, 1200); apiToken.value = provision.token; } else if (kind === "feedback") { await knowledgeSubmitFeedback(scope(), pendingFeedbackKind.value, dialogNote.value || null); await loadDocuments(); } else if (kind === "review" && pendingFeedback.value) { await knowledgeReviewFeedback(scope(), subjectId.value, pendingFeedback.value.id, pendingReviewStatus.value, dialogNote.value || null); await loadDocuments(); } }); closeDialog(); }
async function confirmPending() { const pending = confirmDialog.value; if (!pending) return; confirmDialog.value = null; await run(pending.action); }
async function loadGovernance() { await loadDocuments(); }
onMounted(() => void reload());
</script>

<style scoped>
.knowledge-page { color: var(--text); padding: 28px; overflow: auto; }
.knowledge-page.embedded { padding: 10px 0 0; overflow: visible; }
.materials-header, .button-row, .management-actions { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.materials-header { justify-content: space-between; gap: 16px; margin-bottom: 22px; }
.materials-header h2 { display: flex; align-items: center; gap: 9px; margin: 0 0 7px; font-size: 19px; font-weight: 600; letter-spacing: -.02em; }
.materials-header p, .materials-caption { margin: 0; color: var(--muted); font-size: var(--fs-xs); line-height: 1.6; }
.file-count { font-size: 12px; font-weight: 500; padding: 2px 7px; border-radius: 5px; background: var(--bg-2); color: var(--muted); }
.add-menu { position: relative; }
.add-menu summary { list-style: none; padding: 10px 16px; border-radius: 8px; background: var(--accent); color: white; cursor: pointer; font-size: var(--fs-sm); font-weight: 500; }
.add-menu summary::-webkit-details-marker { display: none; }
.add-menu > div { position: absolute; right: 0; top: calc(100% + 6px); display: grid; min-width: 165px; padding: 5px; z-index: 4; border: 1px solid var(--border); border-radius: 8px; background: var(--panel); box-shadow: var(--shadow-menu); }
.add-menu button { text-align: left; border: 0; padding: 10px; background: transparent; color: var(--text); font: inherit; font-size: var(--fs-sm); cursor: pointer; }
.add-menu button:hover { background: var(--hover); }
.materials-controls { display: flex; justify-content: space-between; gap: 12px; align-items: center; margin-bottom: 10px; flex-wrap: wrap; }
.file-filter { display: flex; align-items: center; gap: 10px; font-size: var(--fs-xs); color: var(--muted); }
.file-filter input { width: 230px; padding: 8px 10px; font: inherit; }
.enable-option { display: flex; gap: 7px; align-items: center; font-size: var(--fs-xs); color: var(--muted); cursor: pointer; }
.enable-option input { accent-color: var(--accent); }
.materials-table-wrap { margin-top: 20px; border: 1px solid var(--border); border-radius: 10px; overflow-x: auto; background: var(--panel); }
.materials-table { width: 100%; border-collapse: collapse; text-align: left; font-size: var(--fs-sm); }
.materials-table th { color: var(--muted); font-size: var(--fs-xs); font-weight: 500; padding: 12px 16px; background: var(--bg-2); white-space: nowrap; }
.materials-table td { padding: 17px 16px; border-top: 1px solid var(--border); }
.materials-table tbody tr:hover { background: var(--hover-faint); }
.file-name { display: flex; align-items: center; gap: 12px; min-width: 180px; }
.file-name > div { min-width: 0; }
.source-open { background: transparent; border: 0; padding: 0; color: var(--text); font: inherit; text-align: left; cursor: pointer; }.source-open:hover { color: var(--accent); text-decoration: underline; }
.preview-heading { display: flex; justify-content: space-between; gap: 12px; flex-wrap: wrap; }.preview-heading span, .preview-path { color: var(--muted); font-size: var(--fs-xs); overflow-wrap: anywhere; }.document-content { max-height: 55vh; overflow: auto; white-space: pre-wrap; overflow-wrap: anywhere; font: inherit; font-size: var(--fs-sm); line-height: 1.8; padding: 18px; border: 1px solid var(--border); background: var(--bg-2); border-radius: 8px; }
.file-name strong { font-weight: 500; overflow-wrap: anywhere; }
.file-name small { display: block; margin-top: 4px; color: var(--muted); font-size: var(--fs-xs); max-width: 350px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.file-type { display: grid; place-items: center; width: 36px; height: 42px; flex-shrink: 0; border: 1px solid var(--border); border-radius: 4px 9px 4px 4px; background: var(--bg); color: var(--muted); font-size: 9px; font-weight: 600; }
.material-status { display: inline-flex; align-items: center; gap: 6px; color: var(--muted); white-space: nowrap; font-size: var(--fs-xs); }
.material-status::before { content: ""; width: 5px; height: 5px; border-radius: 50%; background: currentColor; }
.material-status[data-status="published"] { color: var(--ok, #25805a); }
.file-date { color: var(--muted); font-size: var(--fs-xs); white-space: nowrap; }
.file-remove { color: var(--muted); border: 0; background: transparent; cursor: pointer; white-space: nowrap; font: inherit; font-size: var(--fs-xs); padding: 6px; }
.file-remove:hover { color: var(--danger, #b84b4b); }
.materials-empty { display: flex; flex-direction: column; align-items: center; text-align: center; padding: 58px 20px; margin-top: 22px; border: 1px dashed var(--border); border-radius: 12px; }
.materials-empty svg { color: var(--accent); margin-bottom: 18px; }
.materials-empty h3 { font-size: 18px; font-weight: 500; margin: 0 0 10px; }
.materials-empty p { color: var(--muted); line-height: 1.8; font-size: var(--fs-sm); margin: 0 0 22px; }
.import-notice, .pending-bar { padding: 12px 14px; background: var(--bg-2); border-radius: 8px; font-size: var(--fs-sm); line-height: 1.6; }
.pending-bar { display: flex; gap: 16px; align-items: center; margin-top: 16px; }
.import-failures { color: var(--danger, #b84b4b); font-size: var(--fs-xs); line-height: 1.7; padding: 10px 0; }.import-failures summary { cursor: pointer; }.import-failures p { margin-top: 4px; }
.processing-note { color: var(--accent); font-size: var(--fs-xs); }
.filter-empty { padding: 24px; text-align: center; color: var(--muted); font-size: var(--fs-sm); }
.materials-management { margin-top: 28px; border-top: 1px solid var(--border); padding-top: 16px; }
.materials-management > summary { cursor: pointer; color: var(--muted); font-size: var(--fs-xs); width: fit-content; }
.management-actions { margin: 18px 0; }
.knowledge-card { padding: 20px 0; border-top: 1px solid var(--border); }
.knowledge-card h2 { font-size: var(--fs-sm); margin: 0 0 8px; }
.knowledge-card p, .knowledge-card small { color: var(--muted); font-size: var(--fs-xs); line-height: 1.6; }
.knowledge-documents { list-style: none; padding: 0; }
.knowledge-documents li { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 12px 0; border-top: 1px solid var(--border); }
.knowledge-documents small { display: block; }
.paste-panel { display: grid; gap: 14px; padding: 18px; margin-top: 18px; background: var(--bg-2); border-radius: 8px; }
.paste-panel label, .import-card label, .standalone-scope { display: grid; gap: 6px; font-size: var(--fs-xs); color: var(--muted); }
.knowledge-page input:not([type="checkbox"]), .knowledge-page textarea, .knowledge-page select { box-sizing: border-box; border: 1px solid var(--border); border-radius: 6px; background: var(--panel); color: var(--text); padding: 8px 10px; font: inherit; }
.knowledge-page button:focus-visible, summary:focus-visible, input:focus-visible, textarea:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
.sr-only { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); }
.modal-actions { display: flex; justify-content: flex-end; gap: 8px; }
:global(.knowledge-modal label) { display: grid; gap: 6px; margin: 12px 0; }
:global(.knowledge-modal input), :global(.knowledge-modal select), :global(.knowledge-modal textarea) { width: 100%; box-sizing: border-box; padding: 9px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); color: var(--text); font: inherit; }
@media (max-width: 720px) { .materials-header { align-items: flex-start; }.materials-table td, .materials-table th { padding: 12px 10px; }.file-name small { max-width: 170px; }.materials-controls { align-items: flex-start; flex-direction: column; }.file-filter input { width: min(230px, 65vw); }.knowledge-documents li { flex-wrap: wrap; } }
</style>
